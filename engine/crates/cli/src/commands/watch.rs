//! `validate --watch`: notice that the inputs changed, and print what the
//! re-run did to the diagnostics.

use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use cwtools_driver::search_config_for;
use cwtools_file_manager::file_manager::ignore_glob_match;
use cwtools_file_manager::{
    DirectoryType, classify_directory, expand_multiple_mods, is_excluded_dir, is_excluded_root_dir,
    path_identity,
};

use crate::diag::{Diag, severity_rank};
use crate::run::note;

/// The shortest pause between polls. A change must hold still for one more
/// poll before it counts, so a save that touches many files reloads once.
const POLL: Duration = Duration::from_millis(500);

/// Which loader decides what a root's contents mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootRole {
    /// `ruleset_loader::collect_cwt_files` descends into every directory under
    /// the rules root, tool trees and hidden ones included, and reads `.cwt`
    /// files plus `logs/modifiers.log`.
    Rules,
    /// Mod discovery reads hidden directories too, enters include folders it
    /// joins per name, and walks multi-mod member directories resolved from
    /// `.mod` descriptors.
    Mods,
}

/// One watched root and the policy of the loader that reads it.
#[derive(Debug)]
pub(crate) struct Root {
    path: PathBuf,
    role: RootRole,
    /// The workspace's `--ignore-dirs` globs, which discovery applies under
    /// this root (vanilla discovery resets them).
    ignore_dirs: Vec<String>,
}

/// The roots a watch walks: the ruleset, the workspace, the parent roots, and
/// the vanilla install when one was supplied explicitly. Mod-side roots are
/// deduplicated by path and ignore list, so a same-path root with a different
/// ignore list is kept; identical policies converge.
pub(crate) fn roots(
    rules: &Path,
    directory: &Path,
    parents: &[PathBuf],
    vanilla: Option<&Path>,
    workspace_ignore_dirs: &[String],
) -> Vec<Root> {
    let mut seen: HashSet<(String, Vec<String>)> = HashSet::new();
    let mut watched = vec![Root {
        path: rules.to_path_buf(),
        role: RootRole::Rules,
        ignore_dirs: Vec::new(),
    }];
    let mut mod_roots: Vec<(&Path, Vec<String>)> =
        vec![(directory, workspace_ignore_dirs.to_vec())];
    for parent in parents {
        mod_roots.push((parent.as_path(), workspace_ignore_dirs.to_vec()));
    }
    if let Some(vanilla) = vanilla {
        mod_roots.push((vanilla, Vec::new()));
    }
    for (root, ignore_dirs) in mod_roots {
        let path = root.to_path_buf();
        if seen.insert((path_identity(&path).into_owned(), ignore_dirs.clone())) {
            watched.push(Root {
                path,
                role: RootRole::Mods,
                ignore_dirs,
            });
        }
    }
    watched
}

/// Watches the roots a validate run reads by polling file metadata, so it
/// needs no platform watcher.
pub(crate) struct Poller {
    roots: Vec<Root>,
    /// The loaded ruleset's folders; `None` before any load, while nothing
    /// may be pruned by name because the include folders are unknown.
    folders: Option<Vec<String>>,
    seen: u64,
}

impl Poller {
    /// Snapshot the roots. Take it before the first load starts, so an edit
    /// saved while that load runs is still seen as a change afterwards.
    pub(crate) fn new(roots: Vec<Root>) -> Self {
        let seen = fingerprint(&roots, None);
        Self {
            roots,
            folders: None,
            seen,
        }
    }

    /// Keep the pre-load snapshot so edits saved during the load remain detectable.
    pub(crate) fn retune(&mut self, folders: Vec<String>) {
        self.folders = Some(folders);
    }

    /// Block until the roots differ from the last successful snapshot and have
    /// stopped changing. The caller adopts the snapshot after validation succeeds.
    fn wait(&self) -> u64 {
        let mut last = self.seen;
        let mut pause = POLL;
        loop {
            thread::sleep(pause);
            let started = Instant::now();
            let now = fingerprint(&self.roots, self.folders.as_deref());
            // Ten times the walk keeps an idle watch of a large mod near a tenth of a core.
            pause = POLL.max(started.elapsed() * 10);
            if now != self.seen && now == last {
                return now;
            }
            last = now;
        }
    }
}

/// Run `rerun` after every change and print its delta against the previous
/// result. `rerun` returns `None` when the pass could not read its inputs, which
/// keeps the previous result, and may retune the poller with what that pass
/// learned. Never returns: Ctrl-C ends the process.
pub(crate) fn follow(
    mut poller: Poller,
    mut previous: Vec<Diag>,
    mut rerun: impl FnMut(&mut Poller) -> Option<Vec<Diag>>,
) -> ! {
    note("Watching for changes (Ctrl-C to stop)");
    loop {
        let snapshot = poller.wait();
        note("Change detected, validating again");
        let (snapshot, next) = retry(&mut poller, snapshot, &mut rerun, thread::sleep);
        poller.seen = snapshot;
        print!("{}", delta_report(&previous, &next));
        previous = next;
    }
}

fn retry<T>(
    poller: &mut Poller,
    mut snapshot: u64,
    rerun: &mut impl FnMut(&mut Poller) -> Option<T>,
    mut sleep: impl FnMut(Duration),
) -> (u64, T) {
    let mut pause = Duration::from_secs(1);
    loop {
        if let Some(result) = rerun(poller) {
            return (snapshot, result);
        }
        sleep(pause);
        pause = (pause * 2).min(Duration::from_secs(30));
        snapshot = fingerprint(&poller.roots, poller.folders.as_deref());
    }
}

/// Order-independent digest of the path, modified time and size of what each
/// root's loader reads. Per poll, mod discovery is re-derived: the include
/// folders it enters, fresh from any ruleset folder data the poller holds, and
/// the multi-mod member directories it resolves. Unreadable entries are
/// skipped, since the pass that follows reports them.
fn fingerprint(roots: &[Root], folders: Option<&[String]>) -> u64 {
    let mut total = 0u64;
    for root in roots {
        let Ok(meta) = fs::metadata(&root.path) else {
            continue;
        };
        if !meta.is_dir() {
            total = total.wrapping_add(entry_digest(&root.path, &meta));
            continue;
        }
        let multi = root.role == RootRole::Mods
            && classify_directory(&root.path) == DirectoryType::MultipleMod;
        let includes = if root.role == RootRole::Mods {
            mod_includes(&root.path, folders)
        } else {
            None
        };
        total = total.wrapping_add(fingerprint_tree(
            root,
            &root.path,
            !multi,
            includes.as_deref(),
        ));
        if multi {
            let mut seen = HashSet::from([path_identity(&root.path).into_owned()]);
            for member in expand_multiple_mods(&root.path) {
                if seen.insert(path_identity(&member.root).into_owned()) {
                    total = total.wrapping_add(fingerprint_tree(
                        root,
                        &member.root,
                        false,
                        includes.as_deref(),
                    ));
                }
            }
        }
    }
    total
}

fn fingerprint_tree(
    root: &Root,
    base: &Path,
    single_root: bool,
    includes: Option<&[String]>,
) -> u64 {
    let mut dirs = vec![base.to_path_buf()];
    if let Some(includes) = includes {
        dirs.extend(seed_paths(base, includes));
    }
    let mut visited = HashSet::new();
    let mut total = 0u64;
    while let Some(dir) = dirs.pop() {
        if !visited.insert(dir.clone()) {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // `file_type` does not follow symlinks, matching the loaders.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() || !(file_type.is_dir() || file_type.is_file()) {
                continue;
            }
            let path = entry.path();
            if file_type.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !dir_skipped(
                    root,
                    dir == base,
                    single_root,
                    includes.is_some(),
                    &name,
                    &root_relative(base, &path),
                ) {
                    dirs.push(path);
                } else if dir == base && base == root.path && (name == "mod" || name == "mods") {
                    // Descriptor discovery bypasses the mod's directory exclusions.
                    total = total.wrapping_add(fingerprint_descriptors(&path));
                }
                continue;
            }
            if root.role == RootRole::Rules && !is_rules_input(&path) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            total = total.wrapping_add(entry_digest(&path, &meta));
        }
    }
    total
}

fn fingerprint_descriptors(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("mod"))
        })
        .filter_map(|entry| {
            entry
                .metadata()
                .ok()
                .map(|meta| entry_digest(&entry.path(), &meta))
        })
        .fold(0u64, u64::wrapping_add)
}

/// Whether a directory the walk reached holds nothing its root's loader reads.
fn dir_skipped(
    root: &Root,
    at_root: bool,
    single_root: bool,
    known: bool,
    name: &str,
    relative: &str,
) -> bool {
    if root.role == RootRole::Rules {
        return false;
    }
    if !known {
        // Unknown include folders can name any directory, so nothing is pruned.
        return false;
    }
    if is_excluded_dir(name) {
        return true;
    }
    // Root-level exclusions only apply to a single-root walk: multi-mod
    // discovery reads member roots for every kind.
    if at_root && single_root && is_excluded_root_dir(name) {
        return true;
    }
    root.ignore_dirs
        .iter()
        .any(|pat| ignore_glob_match(pat, name, relative))
}

/// The include folders discovery would apply to `root` right now, derived from
/// the ruleset's folder list and the filesystem, the same way
/// `workspace_discovery_config` combines search defaults, folder existence and
/// the multi-mod classification. `None` before a ruleset is loaded.
fn mod_includes(root: &Path, folders: Option<&[String]>) -> Option<Vec<String>> {
    let folders = folders?;
    let mut includes = search_config_for(root).include_dirs;
    if !folders.is_empty()
        && (folders.iter().any(|folder| root.join(folder).is_dir())
            || classify_directory(root) == DirectoryType::MultipleMod)
    {
        includes = folders.to_vec();
    }
    Some(includes)
}

/// The include folders that exist, using the same `join` paths discovery uses.
/// Keep nested roots: both are independent explicit include roots and may
/// cross directory filters differently from an ordinary parent walk.
fn seed_paths(base: &Path, includes: &[String]) -> Vec<PathBuf> {
    includes
        .iter()
        .map(|include| base.join(include))
        .filter(|path| fs::metadata(path).is_ok_and(|meta| meta.is_dir()))
        .collect()
}

/// The root-relative `/`-separated spelling discovery matches ignore-dirs
/// against (`compute_logical_path_with_root`, private to the file manager).
fn root_relative(base: &Path, path: &Path) -> String {
    let base = base.to_string_lossy().replace('\\', "/");
    let prefix = if base.ends_with('/') {
        base
    } else {
        format!("{base}/")
    };
    match path
        .to_string_lossy()
        .replace('\\', "/")
        .strip_prefix(&prefix)
    {
        Some(rest) => rest.to_string(),
        None => path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string(),
    }
}

/// Whether the ruleset loader reads `path`: the `.cwt` rules files and the
/// game-generated `logs/modifiers.log` (`ruleset_loader::is_modifiers_log`,
/// private to the rules crate, spelled the same way here).
fn is_rules_input(path: &Path) -> bool {
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cwt"))
    {
        return true;
    }
    let named = |name: Option<&std::ffi::OsStr>, want: &str| {
        name.is_some_and(|n| n.eq_ignore_ascii_case(want))
    };
    named(path.file_name(), "modifiers.log")
        && named(path.parent().and_then(Path::file_name), "logs")
}

fn entry_digest(path: &Path, meta: &fs::Metadata) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    meta.modified().ok().hash(&mut hasher);
    meta.len().hash(&mut hasher);
    hasher.finish()
}

/// What changed between two runs, in the shape `scripts/guard.py` prints: a
/// count line, the gone (`-`) and new (`+`) diagnostics, then a sorted
/// `by code (gone/new)` table. Prints `no change` when the runs agree.
///
/// Two diagnostics are the same when every field of their CSV report row
/// matches, the same test the guard applies to its baseline. A diagnostic that
/// only moved to another line is therefore reported as gone and new.
pub(crate) fn delta_report(before: &[Diag], after: &[Diag]) -> String {
    let (gone, new) = diff(before, after);
    if gone.is_empty() && new.is_empty() {
        return "no change\n".to_string();
    }

    let mut out = format!("-{} +{} diagnostics\n\n", gone.len(), new.len());
    for (sign, side) in [('-', &gone), ('+', &new)] {
        for d in side {
            let code = if d.code.is_empty() {
                String::new()
            } else {
                format!("[{}] ", d.code)
            };
            out.push_str(&format!(
                "{sign} {}:{} [{:?}] {code}{}\n",
                d.file, d.line, d.severity, d.message
            ));
        }
    }

    let mut by_code: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for d in &gone {
        by_code.entry(code_label(d)).or_default().0 += 1;
    }
    for d in &new {
        by_code.entry(code_label(d)).or_default().1 += 1;
    }
    out.push_str("\n  by code (gone/new):\n");
    for (code, (gone, new)) in by_code {
        out.push_str(&format!("    {code:<10} -{gone} +{new}\n"));
    }
    out
}

fn code_label(d: &Diag) -> &str {
    if d.code.is_empty() {
        "(no-code)"
    } else {
        d.code
    }
}

type Key<'d> = (&'d str, u32, u8, &'d str, &'d str, &'d str);

/// The fields `csv_row` writes, in the order that groups a file's rows by line.
fn key(d: &Diag) -> Key<'_> {
    (
        &d.file,
        d.line,
        severity_rank(d.severity),
        d.code,
        &d.message,
        &d.hash,
    )
}

/// The diagnostics only `before` has, then the ones only `after` has, each
/// sorted by [`key`]. Counts duplicates, so a row that went from two copies to
/// one shows once.
fn diff<'d>(before: &'d [Diag], after: &'d [Diag]) -> (Vec<&'d Diag>, Vec<&'d Diag>) {
    let sorted = |diags: &'d [Diag]| {
        let mut rows: Vec<&Diag> = diags.iter().collect();
        rows.sort_by(|a, b| key(a).cmp(&key(b)));
        rows
    };
    let (before, after) = (sorted(before), sorted(after));
    let (mut gone, mut new) = (Vec::new(), Vec::new());
    let (mut b, mut a) = (0, 0);
    while b < before.len() && a < after.len() {
        match key(before[b]).cmp(&key(after[a])) {
            Ordering::Equal => {
                b += 1;
                a += 1;
            }
            Ordering::Less => {
                gone.push(before[b]);
                b += 1;
            }
            Ordering::Greater => {
                new.push(after[a]);
                a += 1;
            }
        }
    }
    gone.extend_from_slice(&before[b..]);
    new.extend_from_slice(&after[a..]);
    (gone, new)
}

/// Make Ctrl-C end the process with status 0, at any point in a run. Installed
/// only for `--watch`: a plain `validate` keeps the default, status 130.
#[cfg(unix)]
pub(crate) fn exit_on_interrupt() {
    extern "C" fn exit_clean(_signal: libc::c_int) {
        // SAFETY: `_exit` is async-signal-safe and takes no pointers. Report
        // lines end in a newline and stdout is line buffered, so nothing is lost.
        unsafe { libc::_exit(0) }
    }
    // SAFETY: the handler only calls the async-signal-safe `_exit`.
    unsafe {
        libc::signal(libc::SIGINT, exit_clean as *const () as libc::sighandler_t);
    }
}

/// Make Ctrl-C end the process with status 0, at any point in a run. Installed
/// only for `--watch`.
#[cfg(windows)]
pub(crate) fn exit_on_interrupt() {
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler,
    };
    use windows_sys::core::BOOL;

    extern "system" fn exit_clean(kind: u32) -> BOOL {
        if kind == CTRL_C_EVENT || kind == CTRL_BREAK_EVENT {
            std::process::exit(0);
        }
        // Not ours: let the next handler (the default one) end the process.
        0
    }
    // SAFETY: `exit_clean` matches the `PHANDLER_ROUTINE` signature and lives
    // for the whole process.
    unsafe {
        SetConsoleCtrlHandler(Some(exit_clean), 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_driver::{
        DiscoveryPolicy, discover_localisation_files, workspace_discovery_config,
    };
    use cwtools_file_manager::{
        FileKind, FileManager, FileManagerConfig, ScanBudget, discover_files_multi_mod,
        discover_paths, discover_paths_multi_mod, expand_multiple_mods,
    };
    use cwtools_rules::rules_types::RuleSet;
    use cwtools_rules::ruleset_loader::load_ruleset_from_dir;
    use cwtools_string_table::string_table::StringTable;
    use cwtools_validation::ErrorSeverity;

    fn diag(file: &str, line: u32, code: &'static str, message: &str) -> Diag {
        Diag {
            file: file.into(),
            severity: ErrorSeverity::Error,
            code,
            message: message.to_string(),
            line,
            col: 1,
            hash: format!("{file}|{code}|{message}"),
            legacy_hash: String::new(),
            end: None,
            related: Vec::new(),
        }
    }

    #[test]
    fn identical_runs_print_no_change() {
        let run = vec![diag("a.txt", 3, "CW100", "m")];
        assert_eq!(delta_report(&run, &run), "no change\n");
        assert_eq!(delta_report(&[], &[]), "no change\n");
    }

    #[test]
    fn gone_rows_precede_new_rows_and_codes_are_sorted() {
        let before = vec![
            diag("b.txt", 2, "CW282", "old"),
            diag("a.txt", 9, "CW100", "kept"),
            diag("a.txt", 1, "CW100", "old"),
        ];
        let after = vec![
            diag("a.txt", 9, "CW100", "kept"),
            diag("z.txt", 5, "CW113", "fresh"),
            diag("a.txt", 7, "CW100", "fresh"),
            diag("y.txt", 1, "", "no code"),
        ];
        let report = delta_report(&before, &after);
        assert_eq!(
            report,
            "-2 +3 diagnostics\n\n\
             - a.txt:1 [Error] [CW100] old\n\
             - b.txt:2 [Error] [CW282] old\n\
             + a.txt:7 [Error] [CW100] fresh\n\
             + y.txt:1 [Error] no code\n\
             + z.txt:5 [Error] [CW113] fresh\n\
             \n  by code (gone/new):\n\
             \x20   (no-code)  -0 +1\n\
             \x20   CW100      -1 +1\n\
             \x20   CW113      -0 +1\n\
             \x20   CW282      -1 +0\n"
        );
    }

    /// The guard diffs report rows, and a row carries its line. A diagnostic
    /// that shifted down because of an edit above it is a change there too.
    #[test]
    fn a_diagnostic_that_moved_lines_is_gone_and_new() {
        let report = delta_report(
            &[diag("a.txt", 3, "CW100", "m")],
            &[diag("a.txt", 5, "CW100", "m")],
        );
        assert!(report.starts_with("-1 +1 diagnostics"), "got: {report}");
        assert!(report.contains("CW100      -1 +1"), "got: {report}");
    }

    /// Same file, line, code and message with a different digest: the source
    /// line was edited, which the guard's row comparison also sees.
    #[test]
    fn a_changed_hash_is_a_change() {
        let before = diag("a.txt", 3, "CW100", "m");
        let mut after = diag("a.txt", 3, "CW100", "m");
        after.hash.push('x');
        assert!(delta_report(&[before], &[after]).starts_with("-1 +1"));
    }

    #[test]
    fn duplicate_rows_are_counted_not_collapsed() {
        let two = [
            diag("a.txt", 3, "CW100", "m"),
            diag("a.txt", 3, "CW100", "m"),
        ];
        let one = [diag("a.txt", 3, "CW100", "m")];
        let report = delta_report(&two, &one);
        assert!(report.starts_with("-1 +0 diagnostics"), "got: {report}");
        assert_eq!(delta_report(&two, &two), "no change\n");
    }

    #[test]
    fn row_order_alone_is_not_a_change() {
        let first = [
            diag("a.txt", 1, "CW100", "m"),
            diag("b.txt", 2, "CW282", "n"),
        ];
        let second = [
            diag("b.txt", 2, "CW282", "n"),
            diag("a.txt", 1, "CW100", "m"),
        ];
        assert_eq!(delta_report(&first, &second), "no change\n");
    }

    fn mods_root(path: &Path) -> Root {
        Root {
            path: path.to_path_buf(),
            role: RootRole::Mods,
            ignore_dirs: Vec::new(),
        }
    }

    fn rule_root(path: &Path) -> Root {
        Root {
            path: path.to_path_buf(),
            role: RootRole::Rules,
            ignore_dirs: Vec::new(),
        }
    }

    fn write_rules<T: AsRef<Path> + ?Sized>(rules_dir: &T, folders: &[&str]) -> RuleSet {
        let rules_dir = rules_dir.as_ref();
        fs::write(
            rules_dir.join("base.cwt"),
            "types = { type[base] = { path = \"game/events\" } }\n",
        )
        .unwrap();
        if !folders.is_empty() {
            fs::write(
                rules_dir.join("folders.cwt"),
                format!("{}\n", folders.join("\n")),
            )
            .unwrap();
        }
        let (rules, errors) =
            load_ruleset_from_dir(rules_dir, &StringTable::new(), ScanBudget::default());
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(rules.folders, folders);
        rules
    }

    fn discovered_scripts(dir: &Path, rules: &RuleSet, ignore_dirs: &[String]) -> Vec<PathBuf> {
        let mut config = workspace_discovery_config(dir, Some(rules));
        config.exclude_dir_patterns = ignore_dirs.to_vec();
        sorted_paths(
            FileManager::new(config)
                .discover_files()
                .unwrap()
                .into_iter()
                .map(|f| f.path),
        )
    }

    fn discovered_single_all(dir: &Path, rules: &RuleSet) -> Vec<PathBuf> {
        let config = workspace_discovery_config(dir, Some(rules));
        let report = discover_paths(
            &[dir],
            &config,
            &[FileKind::Script, FileKind::Localisation, FileKind::Resource],
        )
        .unwrap();
        sorted_paths(report.files.into_iter().map(|f| f.path))
    }

    fn discovered_multi_all(collection: &Path) -> Vec<PathBuf> {
        let config = FileManagerConfig {
            root: collection.to_path_buf(),
            include_dirs: vec![".".to_string()],
            ..Default::default()
        };
        let report = discover_paths_multi_mod(
            &config,
            &[FileKind::Script, FileKind::Localisation, FileKind::Resource],
        );
        sorted_paths(report.files.into_iter().map(|f| f.path))
    }

    fn discovered_multi_scripts(
        collection: &Path,
        include_dirs: &[String],
        ignore_dirs: &[String],
    ) -> Vec<PathBuf> {
        let mods = expand_multiple_mods(collection);
        sorted_paths(
            discover_files_multi_mod(
                None,
                &mods,
                include_dirs,
                ignore_dirs,
                ScanBudget::default(),
            )
            .into_iter()
            .map(|(path, _)| path),
        )
    }

    fn sorted_paths(paths: impl Iterator<Item = PathBuf>) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = paths.collect();
        paths.sort();
        paths
    }

    #[test]
    fn derived_includes_match_workspace_discovery_config() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("common").join("deep")).unwrap();
        for folders in [
            vec![],
            vec!["target".to_string()],
            vec!["common".to_string(), "absent".to_string()],
            vec!["common/deep".to_string()],
        ] {
            let rules_dir = tempfile::tempdir().unwrap();
            let rules = write_rules(
                &rules_dir,
                &folders.iter().map(String::as_str).collect::<Vec<_>>(),
            );
            assert_eq!(
                mod_includes(dir.path(), Some(&folders)).unwrap(),
                workspace_discovery_config(dir.path(), Some(&rules)).include_dirs,
                "{folders:?}"
            );
        }
    }

    fn folders<const N: usize>(list: [&str; N]) -> Option<Vec<String>> {
        Some(list.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn fingerprint_moves_when_a_file_is_added_changed_or_removed() {
        let dir = tempfile::tempdir().unwrap();
        let roots = [mods_root(dir.path())];
        let file = dir.path().join("sub").join("x.txt");
        fs::create_dir(dir.path().join("sub")).unwrap();
        let empty = fingerprint(&roots, Some(&[]));
        assert_eq!(
            empty,
            fingerprint(&roots, Some(&[])),
            "stable while nothing changes"
        );

        fs::write(&file, "a = 1\n").unwrap();
        let added = fingerprint(&roots, Some(&[]));
        assert_ne!(added, empty);

        fs::write(&file, "a = 10\n").unwrap();
        let resized = fingerprint(&roots, Some(&[]));
        assert_ne!(resized, added);

        fs::remove_file(&file).unwrap();
        assert_eq!(fingerprint(&roots, Some(&[])), empty);
    }

    #[test]
    fn fingerprint_accepts_a_single_file_root_and_a_missing_root() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rules.cwt");
        fs::write(&file, "types = {}\n").unwrap();
        let roots = [
            rule_root(&file),
            rule_root(&dir.path().join("myrules.txt")),
            mods_root(&dir.path().join("missing")),
        ];
        let before = fingerprint(&roots, Some(&[]));
        fs::write(&file, "types = { }\n").unwrap();
        assert_ne!(fingerprint(&roots, Some(&[])), before);
        fs::write(dir.path().join("myrules.txt"), "types = {  }\n").unwrap();
        assert_ne!(fingerprint(&roots, Some(&[])), before);
    }

    #[test]
    fn failed_validation_does_not_consume_the_change() {
        let dir = tempfile::tempdir().unwrap();
        let poller = Poller::new(vec![mods_root(dir.path())]);
        let previous = poller.seen;
        fs::write(dir.path().join("changed.txt"), "change").unwrap();
        poller.wait();
        assert_eq!(poller.seen, previous);
    }

    #[test]
    fn retries_failed_passes_with_bounded_backoff_and_a_fresh_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let mut poller = Poller::new(vec![mods_root(dir.path())]);
        let original = fingerprint(&poller.roots, poller.folders.as_deref());
        let mut attempts = 0;
        let mut waits = Vec::new();
        let (snapshot, result) = retry(
            &mut poller,
            original,
            &mut |_poller| {
                attempts += 1;
                (attempts == 8).then_some("ok")
            },
            |pause| {
                waits.push(pause.as_secs());
                fs::write(dir.path().join("changed.txt"), "change").unwrap();
            },
        );
        assert_eq!(result, "ok");
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30]);
        assert_ne!(snapshot, original);
        assert_eq!(
            snapshot,
            fingerprint(&poller.roots, poller.folders.as_deref())
        );
    }

    #[test]
    fn change_during_successful_retry_is_not_consumed() {
        let dir = tempfile::tempdir().unwrap();
        let mut poller = Poller::new(vec![mods_root(dir.path())]);
        let before = fingerprint(&poller.roots, poller.folders.as_deref());
        let mut attempts = 0;
        let (snapshot, ()) = retry(
            &mut poller,
            before,
            &mut |_poller| {
                attempts += 1;
                if attempts == 1 {
                    None
                } else {
                    fs::write(dir.path().join("during.txt"), "new input").unwrap();
                    Some(())
                }
            },
            |_| {},
        );
        assert_eq!(attempts, 2);
        assert_eq!(snapshot, before);
        assert_ne!(
            snapshot,
            fingerprint(&poller.roots, poller.folders.as_deref())
        );
    }

    /// A retuned policy stays fresh: the next poll uses the new folders.
    #[test]
    fn retune_changes_what_the_next_poll_prunes() {
        let rules_dir = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("common")).unwrap();
        let watched = roots(rules_dir.path(), dir.path(), &[], None, &[]);
        let mut poller = Poller::new(watched);
        let unknown = fingerprint(&poller.roots, None);
        fs::write(dir.path().join("common").join("x.txt"), "x = 1 with more\n").unwrap();
        assert_ne!(fingerprint(&poller.roots, None), unknown);

        poller.retune(Vec::new());
        assert_eq!(poller.seen, unknown);
        assert_eq!(poller.folders.as_deref(), Some([].as_slice()));
        let tuned = fingerprint(&poller.roots, Some(&[]));
        fs::write(dir.path().join("common").join("x.txt"), "x = 1\n").unwrap();
        assert_ne!(fingerprint(&poller.roots, Some(&[])), tuned);
    }

    /// `.git` under a mod root is a tool tree discovery excludes and a rules
    /// root's `.git` holds no rules inputs.
    #[test]
    fn git_under_a_root_is_not_watched() {
        let dir = tempfile::tempdir().unwrap();
        let roots = [mods_root(dir.path()), rule_root(dir.path())];
        let before = fingerprint(&roots, Some(&[]));
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git").join("index"), "x").unwrap();
        assert_eq!(fingerprint(&roots, Some(&[])), before);
    }

    #[test]
    fn a_hidden_rules_directory_is_watched() {
        let dir = tempfile::tempdir().unwrap();
        let hidden = dir.path().join(".patches");
        fs::create_dir(&hidden).unwrap();
        let patch = hidden.join("patch.cwt");
        let roots = [rule_root(dir.path())];
        let before = fingerprint(&roots, Some(&[]));

        fs::write(&patch, "types = { }\n").unwrap();
        assert_ne!(fingerprint(&roots, Some(&[])), before);
        fs::write(&patch, "types = {  }\n").unwrap();
        assert_ne!(fingerprint(&roots, Some(&[])), before);
    }

    /// A hidden rules directory is now entered. A symlink there must not be
    /// followed, or a link back to its parent hangs the poll.
    #[cfg(unix)]
    #[test]
    fn a_symlink_cycle_under_hidden_rules_is_not_followed() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let hidden = dir.path().join(".patches");
        fs::create_dir(&hidden).unwrap();
        symlink(&hidden, hidden.join("loop")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_rule = outside.path().join("outside.cwt");
        fs::write(&outside_rule, "types = { }\n").unwrap();
        symlink(&outside_rule, hidden.join("link.cwt")).unwrap();
        symlink(outside.path(), dir.path().join("linked-rules")).unwrap();

        let roots = [rule_root(dir.path())];
        let before = fingerprint(&roots, Some(&[]));
        fs::write(&outside_rule, "types = {  }\n").unwrap();
        assert_eq!(fingerprint(&roots, Some(&[])), before);

        fs::write(hidden.join("real.cwt"), "types = { }\n").unwrap();
        assert_ne!(fingerprint(&roots, Some(&[])), before);
    }

    #[test]
    fn a_modifiers_log_is_watched_only_under_a_logs_directory() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("logs")).unwrap();
        let roots = [rule_root(dir.path())];
        let before = fingerprint(&roots, Some(&[]));

        fs::write(
            dir.path().join("logs").join("modifiers.log"),
            "- a, Category: Pops\n",
        )
        .unwrap();
        assert_ne!(fingerprint(&roots, Some(&[])), before);
        let after = fingerprint(&roots, Some(&[]));

        fs::write(dir.path().join("modifiers.log"), "- b, Category: Pops\n").unwrap();
        assert_eq!(fingerprint(&roots, Some(&[])), after);
    }

    #[test]
    fn a_rules_root_watches_loaded_rules_in_tool_trees() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("target")).unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join("target").join("tool.cwt"),
            "types = { type[tool_rule] = { path = \"game/events\" } }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join(".git").join("cached.cwt"),
            "types = { type[git_rule] = { path = \"game/events\" } }\n",
        )
        .unwrap();
        let (rules, errors) =
            load_ruleset_from_dir(dir.path(), &StringTable::new(), ScanBudget::default());
        assert!(errors.is_empty(), "{errors:?}");
        let names: Vec<&str> = rules.types.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"tool_rule") && names.contains(&"git_rule"));

        let roots = [rule_root(dir.path())];
        let before = fingerprint(&roots, Some(&[]));
        fs::write(
            dir.path().join("target").join("tool.cwt"),
            "types = { type[tool_rule] = { path = \"game/other\" } }\n",
        )
        .unwrap();
        assert_ne!(fingerprint(&roots, Some(&[])), before);
        let after = fingerprint(&roots, Some(&[]));
        fs::write(
            dir.path().join(".git").join("cached.cwt"),
            "types = { type[git_rule] = { path = \"game/deeper/way\" } }\n",
        )
        .unwrap();
        assert_ne!(fingerprint(&roots, Some(&[])), after);
    }

    #[test]
    fn a_rules_root_watches_only_its_loaded_inputs() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("README.md"), "how the rules are built\n").unwrap();
        fs::write(dir.path().join("notes.txt"), "to do\n").unwrap();
        let roots = [rule_root(dir.path())];
        let before = fingerprint(&roots, Some(&[]));

        fs::write(
            dir.path().join("README.md"),
            "how the rules are built, extended\n",
        )
        .unwrap();
        fs::write(dir.path().join("notes.txt"), "to do, much more\n").unwrap();
        assert_eq!(fingerprint(&roots, Some(&[])), before);

        fs::write(dir.path().join("base.cwt"), "types = { }\n").unwrap();
        assert_ne!(fingerprint(&roots, Some(&[])), before);
    }

    /// The rules loader reads `.cwt` files wherever they sit and never follows
    /// `.mod` descriptors, so a rules root's walk must not expand members.
    #[test]
    fn a_rules_root_does_not_expand_mod_descriptors() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("mods")).unwrap();
        fs::create_dir_all(dir.path().join("alpha").join("common")).unwrap();
        fs::write(
            dir.path().join("mods").join("alpha.mod"),
            "name = \"Alpha\"\npath = \"alpha\"\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("alpha").join("deep.cwt"),
            "types = { type[deep_rule] = { path = \"game/events\" } }\n",
        )
        .unwrap();
        let (rules, errors) =
            load_ruleset_from_dir(dir.path(), &StringTable::new(), ScanBudget::default());
        assert!(errors.is_empty(), "{errors:?}");
        assert!(rules.types.iter().any(|t| t.name == "deep_rule"));
        assert!(
            !expand_multiple_mods(dir.path()).is_empty(),
            "the fixture has resolvable descriptors for the loader to ignore"
        );

        let roots = [rule_root(dir.path())];
        let before = fingerprint(&roots, Some(&[]));
        fs::write(dir.path().join("alpha").join("note.txt"), "not a rule\n").unwrap();
        assert_eq!(fingerprint(&roots, Some(&[])), before);
        let after = fingerprint(&roots, Some(&[]));
        fs::write(
            dir.path().join("alpha").join("deep.cwt"),
            "types = { type[deep_rule] = { path = \"game/other\" } }\n",
        )
        .unwrap();
        assert_ne!(fingerprint(&roots, Some(&[])), after);
    }

    #[test]
    fn a_hidden_mod_directory_is_watched() {
        let dir = tempfile::tempdir().unwrap();
        let roots = [mods_root(dir.path())];
        let before = fingerprint(&roots, Some(&[]));

        let patch = dir.path().join(".patchmod").join("common").join("x.txt");
        fs::create_dir_all(patch.parent().unwrap()).unwrap();
        fs::write(&patch, "x = 1\n").unwrap();
        assert_ne!(fingerprint(&roots, Some(&[])), before);
    }

    #[test]
    fn roots_preserve_dot_paths_used_by_cli_defaults() {
        let watched = roots(Path::new("."), Path::new("."), &[], None, &[]);

        assert_eq!(watched[0].path, Path::new("."));
        assert_eq!(watched[1].path, Path::new("."));
        assert_ne!(fingerprint(&watched, Some(&[])), 0);
    }

    #[test]
    fn overlapping_includes_fingerprint_each_file_once() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("common/nested")).unwrap();
        fs::write(dir.path().join("common/nested/input.txt"), "x = 1\n").unwrap();
        let watched = [mods_root(dir.path())];
        assert_eq!(
            fingerprint(&watched, Some(&["common".into()])),
            fingerprint(&watched, Some(&["common".into(), "common/nested".into()])),
        );
    }

    #[test]
    fn nested_explicit_includes_are_not_pruned_by_parent_filters() {
        let rules_dir = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("common").join("target")).unwrap();
        fs::write(
            dir.path().join("common").join("target").join("nested.txt"),
            "x = 1\n",
        )
        .unwrap();
        let rules = write_rules(rules_dir.path(), &["common", "common/target"]);
        let watched = roots(
            rules_dir.path(),
            dir.path(),
            &[],
            None,
            &["target".to_string()],
        );
        let folders = rules.folders.clone();
        let before = fingerprint(&watched, Some(&folders));

        fs::write(
            dir.path().join("common").join("target").join("nested.txt"),
            "x = 12\n",
        )
        .unwrap();
        assert_ne!(fingerprint(&watched, Some(&folders)), before);
    }

    #[test]
    fn explicit_includes_are_watched_like_discovery() {
        let rules_dir = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        for folder in ["resources", "gfx/deep", "target/deep"] {
            fs::create_dir_all(dir.path().join(folder)).unwrap();
        }
        fs::write(dir.path().join("resources").join("included.txt"), "x = 1\n").unwrap();
        fs::write(
            dir.path().join("gfx").join("deep").join("nested.txt"),
            "y = 1\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("target").join("deep").join("buried.txt"),
            "z = 1\n",
        )
        .unwrap();
        let rules = write_rules(rules_dir.path(), &["resources", "gfx/deep", "target/deep"]);
        let scripts = discovered_scripts(dir.path(), &rules, &[]);
        assert!(
            scripts.contains(&dir.path().join("resources").join("included.txt"))
                && scripts.contains(&dir.path().join("gfx").join("deep").join("nested.txt"))
                && scripts.contains(&dir.path().join("target").join("deep").join("buried.txt")),
            "{scripts:?}"
        );

        let watched = roots(rules_dir.path(), dir.path(), &[], None, &[]);
        let before = fingerprint(
            &watched,
            folders(["resources", "gfx/deep", "target/deep"]).as_deref(),
        );
        fs::write(
            dir.path().join("resources").join("included.txt"),
            "x = 12\n",
        )
        .unwrap();
        assert_ne!(
            fingerprint(
                &watched,
                folders(["resources", "gfx/deep", "target/deep"]).as_deref()
            ),
            before
        );
        let after = fingerprint(
            &watched,
            folders(["resources", "gfx/deep", "target/deep"]).as_deref(),
        );
        fs::write(
            dir.path().join("gfx").join("deep").join("nested.txt"),
            "y = 12\n",
        )
        .unwrap();
        assert_ne!(
            fingerprint(
                &watched,
                folders(["resources", "gfx/deep", "target/deep"]).as_deref()
            ),
            after
        );
        let after = fingerprint(
            &watched,
            folders(["resources", "gfx/deep", "target/deep"]).as_deref(),
        );
        fs::write(
            dir.path().join("target").join("deep").join("buried.txt"),
            "z = 12\n",
        )
        .unwrap();
        assert_ne!(
            fingerprint(
                &watched,
                folders(["resources", "gfx/deep", "target/deep"]).as_deref()
            ),
            after
        );
    }

    #[test]
    fn an_included_folder_matching_ignore_dirs_is_watched_like_discovery() {
        let rules_dir = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("archived")).unwrap();
        fs::write(dir.path().join("archived").join("included.txt"), "x = 1\n").unwrap();
        let rules = write_rules(rules_dir.path(), &["archived"]);
        let scripts = discovered_scripts(dir.path(), &rules, &["archived".to_string()]);
        assert!(
            scripts.contains(&dir.path().join("archived").join("included.txt")),
            "{scripts:?}"
        );

        let watched = roots(
            rules_dir.path(),
            dir.path(),
            &[],
            None,
            &["archived".to_string()],
        );
        let before = fingerprint(&watched, folders(["archived"]).as_deref());
        fs::write(dir.path().join("archived").join("included.txt"), "x = 12\n").unwrap();
        assert_ne!(
            fingerprint(&watched, folders(["archived"]).as_deref()),
            before
        );
    }

    #[test]
    fn a_single_root_mod_skips_a_root_level_resources_directory() {
        let rules_dir = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("common")).unwrap();
        fs::write(dir.path().join("common").join("x.txt"), "x = 1\n").unwrap();
        fs::create_dir(dir.path().join("resources")).unwrap();
        fs::write(dir.path().join("resources").join("sprite.png"), "img").unwrap();
        let rules = write_rules(rules_dir.path(), &[]);
        let loaded = discovered_single_all(dir.path(), &rules);
        assert!(
            !loaded.contains(&dir.path().join("resources").join("sprite.png")),
            "{loaded:?}"
        );

        let watched = roots(rules_dir.path(), dir.path(), &[], None, &[]);
        let before = fingerprint(&watched, Some(&[]));
        fs::write(dir.path().join("resources").join("sprite.png"), "img 2").unwrap();
        assert_eq!(fingerprint(&watched, Some(&[])), before);

        let nested = dir.path().join("common").join("resources").join("deep.txt");
        fs::create_dir_all(nested.parent().unwrap()).unwrap();
        fs::write(&nested, "nested\n").unwrap();
        assert_ne!(fingerprint(&watched, Some(&[])), before);
    }

    #[test]
    fn multi_mod_members_are_watched_like_discovery() {
        let tree = tempfile::tempdir().unwrap();
        let collection = tree.path().join("collection");
        fs::create_dir_all(tree.path().join("outside")).unwrap();
        fs::create_dir_all(collection.join("mods")).unwrap();
        fs::write(
            collection.join("mods").join("alpha.mod"),
            "name = \"Alpha\"\npath = \"alpha\"\n",
        )
        .unwrap();
        fs::write(
            collection.join("mods").join("beta.mod"),
            "name = \"Beta\"\npath = \"target/beta\"\n",
        )
        .unwrap();
        fs::write(
            collection.join("mods").join("outer.mod"),
            "name = \"Outer\"\npath = \"../outside\"\n",
        )
        .unwrap();
        fs::create_dir_all(collection.join("alpha").join("common")).unwrap();
        fs::create_dir_all(collection.join("alpha").join("resources")).unwrap();
        fs::create_dir_all(collection.join("target").join("beta").join("common")).unwrap();
        fs::create_dir_all(collection.join("../outside").join("common")).unwrap();
        fs::write(
            collection.join("alpha").join("common").join("x.txt"),
            "x = 1\n",
        )
        .unwrap();
        fs::write(
            collection.join("alpha").join("resources").join("r.png"),
            "img\n",
        )
        .unwrap();
        fs::write(
            collection
                .join("target")
                .join("beta")
                .join("common")
                .join("y.txt"),
            "y = 1\n",
        )
        .unwrap();
        fs::write(
            collection.join("../outside").join("common").join("w.txt"),
            "w = 1\n",
        )
        .unwrap();

        let rules_dir = tempfile::tempdir().unwrap();
        let _rules = write_rules(rules_dir.path(), &[]);
        let members: Vec<PathBuf> = expand_multiple_mods(&collection)
            .into_iter()
            .map(|m| m.root)
            .collect();
        assert_eq!(
            members,
            vec![
                collection.join("alpha"),
                collection.join("target").join("beta"),
                collection.join("../outside"),
            ]
        );
        let loaded = discovered_multi_all(&collection);
        for file in [
            collection.join("alpha").join("common").join("x.txt"),
            collection.join("alpha").join("resources").join("r.png"),
            collection
                .join("target")
                .join("beta")
                .join("common")
                .join("y.txt"),
            collection.join("../outside").join("common").join("w.txt"),
        ] {
            assert!(loaded.contains(&file), "{loaded:?}");
        }

        let watched = roots(rules_dir.path(), &collection, &[], None, &[]);
        let before = fingerprint(&watched, Some(&[]));
        fs::write(
            collection.join("alpha").join("resources").join("r.png"),
            "img 2\n",
        )
        .unwrap();
        assert_ne!(fingerprint(&watched, Some(&[])), before);
        let after = fingerprint(&watched, Some(&[]));
        fs::write(
            collection
                .join("target")
                .join("beta")
                .join("common")
                .join("y.txt"),
            "y = 12\n",
        )
        .unwrap();
        assert_ne!(fingerprint(&watched, Some(&[])), after);
        let after = fingerprint(&watched, Some(&[]));
        fs::write(
            collection.join("../outside").join("common").join("w.txt"),
            "w = 12\n",
        )
        .unwrap();
        assert_ne!(fingerprint(&watched, Some(&[])), after);
    }

    #[test]
    fn collection_ignore_patterns_do_not_hide_members() {
        let collection = tempfile::tempdir().unwrap();
        let rules_dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(collection.path().join("mods")).unwrap();
        fs::write(
            collection.path().join("mods").join("alpha.mod"),
            "name = \"Alpha\"\npath = \"alpha\"\n",
        )
        .unwrap();
        fs::create_dir_all(collection.path().join("alpha").join("common")).unwrap();
        fs::write(
            collection.path().join("alpha").join("common").join("x.txt"),
            "x = 1\n",
        )
        .unwrap();
        let scripts = discovered_multi_scripts(
            collection.path(),
            &[".".to_string()],
            &["alpha".to_string()],
        );
        assert!(
            scripts.contains(&collection.path().join("alpha").join("common").join("x.txt")),
            "{scripts:?}"
        );

        let watched = roots(
            rules_dir.path(),
            collection.path(),
            &[],
            None,
            &["alpha".to_string()],
        );
        let before = fingerprint(&watched, Some(&[]));
        fs::write(
            collection.path().join("alpha").join("common").join("x.txt"),
            "x = 12\n",
        )
        .unwrap();
        assert_ne!(fingerprint(&watched, Some(&[])), before);
    }

    #[test]
    fn a_new_descriptor_member_becomes_watched() {
        let collection = tempfile::tempdir().unwrap();
        let rules_dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(collection.path().join("mods")).unwrap();
        fs::write(
            collection.path().join("mods").join("alpha.mod"),
            "name = \"Alpha\"\npath = \"alpha\"\n",
        )
        .unwrap();
        let _rules = write_rules(&rules_dir, &[]);

        let watched = roots(rules_dir.path(), collection.path(), &[], None, &[]);
        let before = fingerprint(&watched, Some(&[]));
        fs::write(
            collection.path().join("mods").join("gamma.mod"),
            "name = \"Gamma\"\npath = \"gamma\"\n",
        )
        .unwrap();
        fs::create_dir_all(collection.path().join("gamma").join("common")).unwrap();
        assert_ne!(fingerprint(&watched, Some(&[])), before);
        let after = fingerprint(&watched, Some(&[]));
        fs::write(
            collection.path().join("gamma").join("common").join("z.txt"),
            "z = 1\n",
        )
        .unwrap();
        assert_ne!(fingerprint(&watched, Some(&[])), after);
    }

    /// From Unknown to MultipleMod: a member root under a root-level excluded
    /// name becomes watched from the poll after the descriptor appears.
    #[test]
    fn classification_changes_are_re_evaluated_each_poll() {
        let rules_dir = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("mods")).unwrap();
        fs::create_dir_all(dir.path().join("resources").join("alpha").join("common")).unwrap();
        let rules = write_rules(rules_dir.path(), &[]);
        assert!(expand_multiple_mods(dir.path()).is_empty());
        let member_file = dir
            .path()
            .join("resources")
            .join("alpha")
            .join("common")
            .join("x.txt");
        assert!(!discovered_single_all(dir.path(), &rules).contains(&member_file));

        let watched = roots(rules_dir.path(), dir.path(), &[], None, &[]);
        let before = fingerprint(&watched, Some(&[]));
        fs::write(&member_file, "x = 2\n").unwrap();
        assert_eq!(fingerprint(&watched, Some(&[])), before);

        fs::write(
            dir.path().join("mods").join("alpha.mod"),
            "name = \"Alpha\"\npath = \"resources/alpha\"\n",
        )
        .unwrap();
        assert!(discovered_multi_all(dir.path()).contains(&member_file));
        let after = fingerprint(&watched, Some(&[]));
        assert_ne!(after, before);
        fs::write(&member_file, "x = 3, longer\n").unwrap();
        assert_ne!(fingerprint(&watched, Some(&[])), after);
    }

    /// `index_game_dir` narrows vanilla script discovery to the ruleset's
    /// folders once one of them exists under the vanilla root, so a folder
    /// named like a root-level exclusion (`resources`) is loaded and watched.
    #[test]
    fn a_vanilla_root_gets_the_ruleset_include_folders() {
        let rules_dir = tempfile::tempdir().unwrap();
        let rules = write_rules(&rules_dir, &["resources"]);
        let workspace = tempfile::tempdir().unwrap();
        let vanilla = tempfile::tempdir().unwrap();
        fs::create_dir_all(vanilla.path().join("common")).unwrap();
        fs::create_dir_all(vanilla.path().join("resources")).unwrap();
        fs::write(vanilla.path().join("common").join("x.txt"), "x = 1\n").unwrap();
        fs::write(
            vanilla.path().join("resources").join("vanilla.txt"),
            "v = 1\n",
        )
        .unwrap();
        let scripts = discovered_scripts(vanilla.path(), &rules, &[]);
        assert!(
            scripts.contains(&vanilla.path().join("resources").join("vanilla.txt")),
            "{scripts:?}"
        );
        assert!(
            !scripts.contains(&vanilla.path().join("common").join("x.txt")),
            "the folder list replaces the searched defaults: {scripts:?}"
        );
        assert_eq!(
            mod_includes(vanilla.path(), Some(&["resources".to_string()])).unwrap(),
            workspace_discovery_config(vanilla.path(), Some(&rules)).include_dirs
        );

        let watched = roots(
            rules_dir.path(),
            workspace.path(),
            &[],
            Some(vanilla.path()),
            &[],
        );
        let vanilla_root = watched.iter().find(|r| r.path == vanilla.path()).unwrap();
        assert!(vanilla_root.ignore_dirs.is_empty());
        let before = fingerprint(&watched, folders(["resources"]).as_deref());
        fs::write(
            vanilla.path().join("resources").join("vanilla.txt"),
            "v = 12\n",
        )
        .unwrap();
        assert_ne!(
            fingerprint(&watched, folders(["resources"]).as_deref()),
            before
        );
    }

    /// The same collection under a workspace policy and a vanilla policy keep
    /// their own member coverage: the vanilla policy catches member files the
    /// workspace's ignore-dirs prune.
    #[test]
    fn same_collection_under_two_policies_keeps_member_coverage() {
        let rules_dir = tempfile::tempdir().unwrap();
        let _rules = write_rules(&rules_dir, &[]);
        let collection = tempfile::tempdir().unwrap();
        fs::create_dir_all(collection.path().join("mods")).unwrap();
        fs::write(
            collection.path().join("mods").join("alpha.mod"),
            "name = \"Alpha\"\npath = \"alpha\"\n",
        )
        .unwrap();
        fs::create_dir_all(collection.path().join("alpha").join("alpha")).unwrap();
        fs::create_dir_all(collection.path().join("alpha").join("localisation")).unwrap();
        fs::write(
            collection
                .path()
                .join("alpha")
                .join("alpha")
                .join("inner.txt"),
            "i = 1\n",
        )
        .unwrap();
        fs::write(
            collection
                .path()
                .join("alpha")
                .join("localisation")
                .join("a_l_english.yml"),
            "A: 0\n",
        )
        .unwrap();
        // Vanilla localisation discovery walks the whole tree unpruned.
        let loc = discover_localisation_files(
            &[collection.path().to_path_buf()],
            &[],
            &[],
            DiscoveryPolicy::Vanilla,
        )
        .unwrap();
        assert!(
            loc.files.iter().any(|f| f.path
                == collection
                    .path()
                    .join("alpha")
                    .join("localisation")
                    .join("a_l_english.yml")),
            "{loc:?}"
        );

        let watched = roots(
            rules_dir.path(),
            collection.path(),
            &[],
            Some(collection.path()),
            &["alpha".to_string()],
        );
        let same_path: Vec<&Root> = watched
            .iter()
            .filter(|r| r.path == collection.path())
            .collect();
        assert_eq!(same_path.len(), 2);
        assert!(same_path.iter().any(|r| r.ignore_dirs == ["alpha"]));
        assert!(same_path.iter().any(|r| r.ignore_dirs.is_empty()));

        let before = fingerprint(&watched, Some(&[]));
        fs::write(
            collection
                .path()
                .join("alpha")
                .join("alpha")
                .join("inner.txt"),
            "i = 12\n",
        )
        .unwrap();
        assert_ne!(fingerprint(&watched, Some(&[])), before);
    }

    /// Include folders are walked exactly as discovery joins them: relative
    /// with `./`, climbing `../`, all entered whatever their names are.
    #[test]
    fn include_folders_discovery_joins_are_watched() {
        let rules_dir = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("common")).unwrap();
        fs::create_dir_all(dir.path().join("target")).unwrap();
        fs::create_dir_all(dir.path().join("resources")).unwrap();
        fs::write(dir.path().join("common").join("x.txt"), "x = 1\n").unwrap();
        fs::write(dir.path().join("target").join("in.txt"), "t = 1\n").unwrap();
        fs::write(dir.path().join("resources").join("res.txt"), "r = 1\n").unwrap();
        let rules = write_rules(rules_dir.path(), &["./target", "target/../resources"]);
        // Discovery returns walked spellings: each include dir joined verbatim.
        let scripts = discovered_scripts(dir.path(), &rules, &[]);
        assert!(
            scripts.contains(&dir.path().join("./target").join("in.txt"))
                && scripts.contains(&dir.path().join("target/../resources").join("res.txt")),
            "{scripts:?}"
        );

        let watched = roots(rules_dir.path(), dir.path(), &[], None, &[]);
        let folders = ["./target".to_string(), "target/../resources".to_string()];
        let before = fingerprint(&watched, Some(&folders));
        fs::write(dir.path().join("target").join("in.txt"), "t = 12\n").unwrap();
        assert_ne!(fingerprint(&watched, Some(&folders)), before);
        let after = fingerprint(&watched, Some(&folders));
        fs::write(dir.path().join("resources").join("res.txt"), "r = 12\n").unwrap();
        assert_ne!(fingerprint(&watched, Some(&folders)), after);
    }

    /// `expand_multiple_mods` parses `mod`/`mods` descriptors whatever the
    /// ignore lists say, so descriptor files stay watched even when the folder
    /// name is itself ignored.
    #[test]
    fn collection_descriptor_edits_are_watched_despite_ignore_dirs() {
        let collection = tempfile::tempdir().unwrap();
        let rules_dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(collection.path().join("mods")).unwrap();
        fs::create_dir_all(collection.path().join("alpha").join("common")).unwrap();
        fs::write(
            collection.path().join("mods").join("alpha.mod"),
            "name = \"Alpha\"\npath = \"alpha\"\n",
        )
        .unwrap();
        fs::write(collection.path().join("mods").join("readme.txt"), "notes\n").unwrap();
        fs::write(
            collection.path().join("alpha").join("common").join("x.txt"),
            "x = 1\n",
        )
        .unwrap();
        let _rules = write_rules(&rules_dir, &[]);
        assert!(!expand_multiple_mods(collection.path()).is_empty());

        let watched = roots(
            rules_dir.path(),
            collection.path(),
            &[],
            None,
            &["mods".to_string()],
        );
        let before = fingerprint(&watched, Some(&[]));
        fs::write(
            collection.path().join("mods").join("alpha.mod"),
            "name = \"Alpha\"\npath = \"alpha/renamed\"\n",
        )
        .unwrap();
        assert_ne!(fingerprint(&watched, Some(&[])), before);
        let after = fingerprint(&watched, Some(&[]));
        // Non-descriptor files in the ignored folder are not inputs.
        fs::write(
            collection.path().join("mods").join("readme.txt"),
            "notes more\n",
        )
        .unwrap();
        assert_eq!(fingerprint(&watched, Some(&[])), after);
    }

    /// A ruleset folder that does not exist yet is applied the moment it does:
    /// discovery is re-derived every poll, so a later `target/` directory is
    /// entered, not left pruned behind its tool-tree name.
    #[test]
    fn a_configured_folder_created_later_becomes_watched() {
        let rules_dir = tempfile::tempdir().unwrap();
        let rules = write_rules(&rules_dir, &["target"]);
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            workspace_discovery_config(dir.path(), Some(&rules)).include_dirs,
            search_config_for(dir.path()).include_dirs,
            "the folder is not applied while it does not exist"
        );

        let watched = roots(rules_dir.path(), dir.path(), &[], None, &[]);
        let before = fingerprint(&watched, folders(["target"]).as_deref());
        fs::create_dir_all(dir.path().join("target")).unwrap();
        fs::write(dir.path().join("target").join("in.txt"), "t = 1\n").unwrap();
        assert_eq!(
            workspace_discovery_config(dir.path(), Some(&rules)).include_dirs,
            vec!["target".to_string()]
        );
        assert!(
            discovered_scripts(dir.path(), &rules, &[])
                .contains(&dir.path().join("target").join("in.txt"))
        );
        assert_ne!(
            fingerprint(&watched, folders(["target"]).as_deref()),
            before
        );
        let after = fingerprint(&watched, folders(["target"]).as_deref());
        fs::write(dir.path().join("target").join("in.txt"), "t = 12\n").unwrap();
        assert_ne!(fingerprint(&watched, folders(["target"]).as_deref()), after);
    }

    /// A change in the explicit vanilla install wakes the poll.
    #[test]
    fn an_explicit_vanilla_directory_is_watched() {
        let rules_dir = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let vanilla = tempfile::tempdir().unwrap();
        let watched = roots(
            rules_dir.path(),
            workspace.path(),
            &[],
            Some(vanilla.path()),
            &[],
        );
        let poller = Poller::new(watched);
        let before = poller.seen;

        let file = vanilla.path().join("events").join("vanilla.txt");
        fs::create_dir(vanilla.path().join("events")).unwrap();
        fs::write(&file, "x = 1\n").unwrap();

        assert_ne!(
            fingerprint(&poller.roots, poller.folders.as_deref()),
            before
        );
    }

    #[test]
    fn repeated_roots_dedup_within_a_role_and_keep_both_roles() {
        let rules_dir = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir_all(workspace.path().join("common")).unwrap();

        // The same path and policy is watched once.
        let watched = roots(
            rules_dir.path(),
            workspace.path(),
            &[workspace.path().to_path_buf()],
            None,
            &[],
        );
        let mod_paths: Vec<PathBuf> = watched[1..].iter().map(|r| r.path.clone()).collect();
        assert_eq!(mod_paths, vec![workspace.path().to_path_buf()]);

        // A different ignore list is a different policy and stays.
        let watched = roots(
            rules_dir.path(),
            workspace.path(),
            &[],
            Some(workspace.path()),
            &["ignored".to_string()],
        );
        let same_path: Vec<&Root> = watched[1..]
            .iter()
            .filter(|r| r.path == workspace.path())
            .collect();
        assert_eq!(same_path.len(), 2);

        // The rules root never merges into a mod root.
        let watched = roots(rules_dir.path(), rules_dir.path(), &[], None, &[]);
        assert_eq!(watched.len(), 2);
        assert_eq!(watched[0].role, RootRole::Rules);
        assert_eq!(watched[1].role, RootRole::Mods);
        assert_eq!(watched[0].path, watched[1].path);
    }

    #[test]
    fn a_path_serving_both_roles_watches_under_both_policies() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("events")).unwrap();
        fs::write(dir.path().join("events").join("x.txt"), "x = 1\n").unwrap();
        let watched = roots(dir.path(), dir.path(), &[], None, &[]);
        assert_eq!(watched.len(), 2);
        let before = fingerprint(&watched, Some(&[]));

        let patch = dir.path().join(".patches").join("patch.cwt");
        fs::create_dir(patch.parent().unwrap()).unwrap();
        fs::write(&patch, "types = { }\n").unwrap();
        assert_ne!(fingerprint(&watched, Some(&[])), before);
        let after = fingerprint(&watched, Some(&[]));

        fs::write(dir.path().join("events").join("x.txt"), "x = 12\n").unwrap();
        assert_ne!(fingerprint(&watched, Some(&[])), after);
    }

    #[test]
    fn an_unset_vanilla_adds_no_root() {
        let rules_dir = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let vanilla = tempfile::tempdir().unwrap();
        let watched = roots(
            rules_dir.path(),
            workspace.path(),
            &[workspace.path().to_path_buf()],
            None,
            &[],
        );
        let paths: Vec<PathBuf> = watched.iter().map(|r| r.path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                rules_dir.path().to_path_buf(),
                workspace.path().to_path_buf()
            ]
        );

        let before = fingerprint(&watched, Some(&[]));
        fs::write(vanilla.path().join("outside.txt"), "x = 1\n").unwrap();
        assert_eq!(fingerprint(&watched, Some(&[])), before);
    }

    #[test]
    fn each_role_keeps_its_own_exclusions() {
        let rules_dir = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let vanilla = tempfile::tempdir().unwrap();
        for root in [dir.path(), parent.path(), vanilla.path()] {
            fs::create_dir_all(root.join("common")).unwrap();
        }
        let watched = roots(
            rules_dir.path(),
            dir.path(),
            &[parent.path().to_path_buf()],
            Some(vanilla.path()),
            &[],
        );

        let before = fingerprint(&watched, Some(&[]));
        fs::write(dir.path().join("common").join("x.txt"), "x = 1\n").unwrap();
        fs::write(parent.path().join("common").join("p.txt"), "p = 1\n").unwrap();
        fs::write(vanilla.path().join("common").join("v.txt"), "v = 1\n").unwrap();
        assert_ne!(fingerprint(&watched, Some(&[])), before);
    }

    #[test]
    fn workspace_ignore_dirs_are_skipped_but_vanilla_roots_keep_watching() {
        let rules_dir = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let vanilla = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("common")).unwrap();
        fs::create_dir(dir.path().join("ignored")).unwrap();
        fs::create_dir(vanilla.path().join("ignored")).unwrap();
        fs::write(dir.path().join("common").join("x.txt"), "x = 1\n").unwrap();
        fs::write(dir.path().join("ignored").join("notes.txt"), "draft\n").unwrap();

        let watched = roots(
            rules_dir.path(),
            dir.path(),
            &[],
            Some(vanilla.path()),
            &["ignored".to_string()],
        );
        let watched_dir = watched.iter().find(|r| r.path == dir.path()).unwrap();
        assert!(watched_dir.ignore_dirs == ["ignored"]);
        let watched_vanilla = watched.iter().find(|r| r.path == vanilla.path()).unwrap();
        assert!(watched_vanilla.ignore_dirs.is_empty());

        let before = fingerprint(&watched, Some(&[]));
        fs::write(dir.path().join("ignored").join("notes.txt"), "draft 2\n").unwrap();
        assert_eq!(fingerprint(&watched, Some(&[])), before);

        fs::write(vanilla.path().join("ignored").join("loc.yml"), "x").unwrap();
        assert_ne!(fingerprint(&watched, Some(&[])), before);
    }

    #[test]
    fn path_pattern_ignore_dirs_skip_their_subtree() {
        let rules_dir = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("common")).unwrap();
        fs::create_dir_all(dir.path().join("docs").join("tmp")).unwrap();
        fs::write(dir.path().join("common").join("x.txt"), "x = 1\n").unwrap();
        let loaded = discovered_scripts(
            dir.path(),
            &write_rules(&rules_dir, &[]),
            &["docs/tmp".to_string()],
        );
        assert!(!loaded.contains(&dir.path().join("docs").join("tmp").join("a.txt")));

        let watched = roots(
            rules_dir.path(),
            dir.path(),
            &[],
            None,
            &["docs/tmp".to_string()],
        );
        let before = fingerprint(&watched, Some(&[]));
        fs::write(dir.path().join("docs").join("tmp").join("a.txt"), "x").unwrap();
        assert_eq!(fingerprint(&watched, Some(&[])), before);

        fs::write(dir.path().join("common").join("y.txt"), "y").unwrap();
        assert_ne!(fingerprint(&watched, Some(&[])), before);
    }
}
