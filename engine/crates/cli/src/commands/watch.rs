//! `validate --watch`: notice that the inputs changed, and print what the
//! re-run did to the diagnostics.

use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::iter;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use cwtools_driver::{
    DiscoveryPolicy, WorkspaceDiscoveryRequest, discover_workspace, workspace_discovery_config,
};
use cwtools_file_manager::{
    DiscoveredPath, DiscoveryReport, FileError, FileKind, ScanBudget, path_identity,
};
use cwtools_rules::rules_types::RuleSet;
use cwtools_rules::ruleset_loader::ruleset_input_files;

use crate::diag::{Diag, severity_rank};
use crate::run::note;

/// The shortest pause between polls. A change must hold still for one more
/// poll before it counts, so a save that touches many files reloads once.
const POLL: Duration = Duration::from_millis(500);

/// Which loader reads a root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RootRole {
    /// The ruleset: `.cwt` files and `logs/modifiers.log`.
    Rules,
    /// The workspace or a parent mod, discovered under the user's ignore lists.
    Workspace,
    /// The explicit base-game install, discovered under its default policy.
    Vanilla,
}

/// One watched root.
#[derive(Debug)]
pub(crate) struct Root {
    path: PathBuf,
    role: RootRole,
    ignore_dirs: Vec<String>,
    ignore_files: Vec<String>,
}

/// The roots a watch polls: the ruleset, the workspace, the parent roots, and
/// the vanilla install when one was supplied explicitly. A path is watched once
/// per role, so one directory can be both a ruleset and a mod.
pub(crate) fn roots(
    rules: &Path,
    directory: &Path,
    parents: &[PathBuf],
    vanilla: Option<&Path>,
    ignore_dirs: &[String],
    ignore_files: &[String],
) -> Vec<Root> {
    let plain = |path: &Path, role| Root {
        path: path.to_path_buf(),
        role,
        ignore_dirs: Vec::new(),
        ignore_files: Vec::new(),
    };
    let workspace = |path: &Path| Root {
        ignore_dirs: ignore_dirs.to_vec(),
        ignore_files: ignore_files.to_vec(),
        ..plain(path, RootRole::Workspace)
    };
    let mut seen = HashSet::new();
    iter::once(plain(rules, RootRole::Rules))
        .chain(iter::once(workspace(directory)))
        .chain(parents.iter().map(|parent| workspace(parent)))
        .chain(vanilla.map(|path| plain(path, RootRole::Vanilla)))
        .filter(|root| seen.insert((path_identity(&root.path).into_owned(), root.role)))
        .collect()
}

/// Watches the roots a validate run reads by polling the loaders' own
/// discovery, so it needs no platform watcher.
pub(crate) struct Poller {
    roots: Vec<Root>,
    /// The loaded ruleset's folders; empty until the first load reports them.
    folders: Vec<String>,
    seen: u64,
    /// The attempt's snapshot, adjusted when watched roots change.
    handoff: Option<u64>,
}

impl Poller {
    /// Snapshot the roots. Take it before the first load starts, so an edit
    /// saved while that load runs is still seen as a change afterwards.
    pub(crate) fn new(roots: Vec<Root>) -> Self {
        let seen = fingerprint(&roots, &[]);
        Self {
            roots,
            folders: Vec::new(),
            seen,
            handoff: None,
        }
    }

    /// Poll under the folders the latest load reported. Folders change which
    /// files discovery lists, so when nothing changed since the last snapshot
    /// the baseline moves to the new policy; a pending edit stays pending.
    pub(crate) fn retune(&mut self, folders: Vec<String>) {
        if folders == self.folders {
            return;
        }
        // The new policy goes first: an edit landing between the two walks then
        // shows in the second and keeps the snapshot stale.
        let after = fingerprint(&self.roots, &folders);
        if self.current() == self.seen {
            self.seen = after;
        }
        self.folders = folders;
    }

    /// Change watched roots without consuming edits saved after the attempt's snapshot.
    pub(crate) fn retune_roots(&mut self, roots: Vec<Root>) {
        let after = fingerprint(&roots, &self.folders);
        if self.handoff == Some(self.current()) {
            self.handoff = Some(after);
        }
        self.roots = roots;
    }

    /// Adopt the attempt's snapshot, leaving mid-pass edits pending.
    fn settle(&mut self, snapshot: u64) {
        self.seen = self.handoff.take().unwrap_or(snapshot);
    }

    fn current(&self) -> u64 {
        fingerprint(&self.roots, &self.folders)
    }

    /// Block until the roots differ from the last successful snapshot and have
    /// stopped changing. The caller adopts the snapshot after validation succeeds.
    fn wait(&self) -> u64 {
        self.wait_with(thread::sleep)
    }

    fn wait_with(&self, mut sleep: impl FnMut(Duration)) -> u64 {
        let mut last = self.seen;
        let mut pause = POLL;
        loop {
            sleep(pause);
            let started = Instant::now();
            let now = self.current();
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
///
/// Changes outside the watched roots do not wake this loop; launcher `.mod`
/// files are outside them (unless a parent root does cover one) and their
/// changes are read on the next reload.
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
        poller.settle(snapshot);
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
        // Discard any root baseline left by a failed attempt.
        poller.handoff = Some(snapshot);
        if let Some(result) = rerun(poller) {
            return (snapshot, result);
        }
        sleep(pause);
        pause = (pause * 2).min(Duration::from_secs(30));
        snapshot = poller.current();
    }
}

/// Order-independent digest of what the loaders read under `roots`, as their
/// own discovery lists it under `folders`. A root that cannot be read adds a
/// marker of the failure; the pass that follows reports it.
fn fingerprint(roots: &[Root], folders: &[String]) -> u64 {
    let mut ruleset = RuleSet::new();
    ruleset.folders = folders.to_vec();
    roots
        .iter()
        .map(|root| match root.role {
            RootRole::Rules => rules_digest(&root.path),
            RootRole::Workspace | RootRole::Vanilla => mods_digest(root, &ruleset),
        })
        .fold(0, u64::wrapping_add)
}

fn rules_digest(path: &Path) -> u64 {
    let Ok(meta) = fs::metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return entry_digest(path, &meta);
    }
    ruleset_input_files(path, ScanBudget::default())
        .iter()
        .filter_map(|file| {
            let meta = fs::metadata(file).ok()?;
            Some(entry_digest(file, &meta))
        })
        .fold(0, u64::wrapping_add)
}

fn mods_digest(root: &Root, ruleset: &RuleSet) -> u64 {
    let discovered = match discover(root, ruleset) {
        Ok(DiscoveryReport {
            files, failures, ..
        }) => {
            let failures = failures
                .iter()
                .map(|failure| digest((&failure.path, &failure.error)));
            files
                .iter()
                .map(discovered_digest)
                .chain(failures)
                .fold(0, u64::wrapping_add)
        }
        Err(error) => digest((&root.path, error.to_string())),
    };
    discovered.wrapping_add(descriptors_digest(&root.path))
}

/// The files the session loads from `root`: the call, config and policy that
/// `Session::load` makes for the workspace and its parents, plus resources.
fn discover(root: &Root, ruleset: &RuleSet) -> Result<DiscoveryReport, FileError> {
    let mut config = workspace_discovery_config(&root.path, Some(ruleset));
    config
        .exclude_patterns
        .extend(root.ignore_files.iter().cloned());
    config
        .exclude_dir_patterns
        .extend(root.ignore_dirs.iter().cloned());
    let policy = if root.role == RootRole::Vanilla {
        DiscoveryPolicy::Vanilla
    } else {
        DiscoveryPolicy::Workspace
    };
    discover_workspace(WorkspaceDiscoveryRequest {
        roots: vec![root.path.clone()],
        kinds: vec![FileKind::Script, FileKind::Localisation, FileKind::Resource],
        config,
        policy,
    })
}

/// Scripts and localisation are read, so an edit counts. Resources only have to
/// exist, so re-exporting art does not.
fn discovered_digest(file: &DiscoveredPath) -> u64 {
    let location = digest((&file.path, &file.root_relative_path));
    if file.kind == FileKind::Resource {
        return location;
    }
    match fs::metadata(&file.path) {
        Ok(meta) => location.wrapping_add(entry_digest(&file.path, &meta)),
        Err(_) => location,
    }
}

/// `descriptor.mod` and the `mod`/`mods` descriptors are read outside
/// discovery, and an ignore glob can hide them from it.
fn descriptors_digest(root: &Path) -> u64 {
    let collected = ["mod", "mods"]
        .into_iter()
        .filter_map(|name| fs::read_dir(root.join(name)).ok())
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("mod"))
        });
    iter::once(root.join("descriptor.mod"))
        .chain(collected)
        .filter_map(|path| {
            let meta = fs::metadata(&path).ok()?;
            Some(entry_digest(&path, &meta))
        })
        .fold(0, u64::wrapping_add)
}

/// Path, modified time and size: what changes when an input is edited.
fn entry_digest(path: &Path, meta: &fs::Metadata) -> u64 {
    digest((path, meta.modified().ok(), meta.len()))
}

fn digest(value: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
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

    unsafe extern "system" fn exit_clean(kind: u32) -> BOOL {
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

    fn strings<const N: usize>(items: [&str; N]) -> Vec<String> {
        items.into_iter().map(String::from).collect()
    }

    fn plain_root(path: &Path, role: RootRole) -> Root {
        Root {
            path: path.to_path_buf(),
            role,
            ignore_dirs: Vec::new(),
            ignore_files: Vec::new(),
        }
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// Whether rewriting `file` moves the fingerprint. The second body is
    /// longer, so the answer never depends on the modified time's granularity.
    fn edit_moves(watched: &[Root], folders: &[String], file: &Path) -> bool {
        write(file, "a = 1\n");
        let before = fingerprint(watched, folders);
        write(file, "a = 1 # and a longer body\n");
        fingerprint(watched, folders) != before
    }

    fn discovers(watched: &Root, folders: &[String], file: &Path) -> bool {
        let mut ruleset = RuleSet::new();
        ruleset.folders = folders.to_vec();
        let report = discover(watched, &ruleset).unwrap();
        report.files.iter().any(|found| found.path == file)
    }

    #[test]
    fn fingerprint_moves_when_a_file_is_added_changed_or_removed() {
        let dir = tempfile::tempdir().unwrap();
        let watched = [plain_root(dir.path(), RootRole::Workspace)];
        let file = dir.path().join("common/x.txt");
        fs::create_dir(dir.path().join("common")).unwrap();
        let empty = fingerprint(&watched, &[]);
        assert_eq!(empty, fingerprint(&watched, &[]), "stable while idle");

        write(&file, "a = 1\n");
        let added = fingerprint(&watched, &[]);
        assert_ne!(added, empty);

        write(&file, "a = 10\n");
        assert_ne!(fingerprint(&watched, &[]), added);

        fs::remove_file(&file).unwrap();
        assert_eq!(fingerprint(&watched, &[]), empty);
    }

    #[test]
    fn scripts_and_localisation_are_read_but_resources_only_have_to_exist() {
        let dir = tempfile::tempdir().unwrap();
        let watched = [plain_root(dir.path(), RootRole::Workspace)];
        assert!(edit_moves(&watched, &[], &dir.path().join("common/x.txt")));
        let loc = dir.path().join("localisation/a_l_english.yml");
        assert!(edit_moves(&watched, &[], &loc));

        let art = dir.path().join("gfx/icon.dds");
        assert!(!edit_moves(&watched, &[], &art));
        let before = fingerprint(&watched, &[]);
        fs::remove_file(&art).unwrap();
        assert_ne!(fingerprint(&watched, &[]), before);
    }

    #[test]
    fn ignored_files_do_not_move_the_fingerprint() {
        let rules = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ignore_files = strings([
            "skip.txt",
            "common/path*.txt",
            "**/nested/no*.txt",
            "localisation/skip*.yml",
        ]);
        let watched = roots(rules.path(), dir.path(), &[], None, &[], &ignore_files);
        let cases = [
            ("common/skip.txt", false),
            ("common/path_match.txt", false),
            ("common/nested/nope.txt", false),
            ("localisation/skip_l_english.yml", false),
            ("common/README.txt", false),
            ("common/notes.md", false),
            ("common/keep.txt", true),
            ("localisation/keep_l_english.yml", true),
        ];
        for (relative, expected) in cases {
            let file = dir.path().join(relative);
            assert_eq!(edit_moves(&watched, &[], &file), expected, "{relative}");
            assert_eq!(discovers(&watched[1], &[], &file), expected, "{relative}");
        }
    }

    #[test]
    fn ignored_dirs_do_not_move_the_fingerprint() {
        let rules = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ignore_dirs = strings(["archive", "common/docs/tmp"]);
        let watched = roots(rules.path(), dir.path(), &[], None, &ignore_dirs, &[]);
        let unfiltered = plain_root(dir.path(), RootRole::Workspace);
        for relative in [
            "common/archive/old.txt",
            "common/docs/tmp/draft.txt",
            "gfx/archive/old.dds",
        ] {
            let file = dir.path().join(relative);
            assert!(!edit_moves(&watched, &[], &file), "{relative}");
            assert!(discovers(&unfiltered, &[], &file), "{relative}");
            assert!(!discovers(&watched[1], &[], &file), "{relative}");
        }
        assert!(edit_moves(
            &watched,
            &[],
            &dir.path().join("common/docs/kept.txt")
        ));

        // An explicit include folder is entered even when its name is ignored.
        let included = dir.path().join("archive/included.txt");
        assert!(!edit_moves(&watched, &[], &included));
        assert!(edit_moves(&watched, &strings(["archive"]), &included));
    }

    #[test]
    fn ruleset_folders_choose_which_scripts_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let watched = [plain_root(dir.path(), RootRole::Workspace)];
        let folders = strings(["map", "target/deep", "resources"]);
        for relative in ["map/a.txt", "target/deep/b.txt", "resources/c.txt"] {
            let file = dir.path().join(relative);
            assert!(!edit_moves(&watched, &[], &file), "{relative}");
            assert!(edit_moves(&watched, &folders, &file), "{relative}");
        }
    }

    #[test]
    fn an_empty_configured_folder_switches_the_discovery_policy() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("common/a.txt"), "a = 1\n");
        let watched = [plain_root(dir.path(), RootRole::Workspace)];
        let folders = strings(["map"]);
        let before = fingerprint(&watched, &folders);
        fs::create_dir(dir.path().join("map")).unwrap();
        assert_ne!(fingerprint(&watched, &folders), before);
        fs::remove_dir(dir.path().join("map")).unwrap();
        assert_eq!(fingerprint(&watched, &folders), before);
    }

    #[test]
    fn a_tool_directory_is_not_watched() {
        let rules = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let watched = roots(rules.path(), dir.path(), &[], None, &[], &[]);
        let before = fingerprint(&watched, &[]);
        write(&dir.path().join(".git/index"), "x");
        write(&dir.path().join("node_modules/pkg/index.txt"), "x");
        assert_eq!(fingerprint(&watched, &[]), before);
    }

    fn collection(dir: &Path) {
        write(
            &dir.join("mods/alpha.mod"),
            "name = \"Alpha\"\npath = \"alpha\"\n",
        );
        fs::create_dir_all(dir.join("alpha/common")).unwrap();
    }

    #[test]
    fn collection_members_are_polled_under_their_own_paths() {
        let rules = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        collection(dir.path());
        write(
            &dir.path().join("mods/beta.mod"),
            "name = \"Beta\"\npath = \"nested/beta\"\n",
        );
        fs::create_dir_all(dir.path().join("nested/beta")).unwrap();
        let ignore_files = strings(["common/skip.txt"]);
        let watched = roots(rules.path(), dir.path(), &[], None, &[], &ignore_files);

        for (relative, expected) in [
            ("alpha/common/keep.txt", true),
            ("nested/beta/common/x.txt", true),
            ("alpha/common/skip.txt", false),
            ("alpha/gfx/icon.dds", false),
            ("unowned.txt", false),
        ] {
            let file = dir.path().join(relative);
            assert_eq!(edit_moves(&watched, &[], &file), expected, "{relative}");
        }
    }

    #[test]
    fn descriptors_are_polled_even_when_ignored() {
        let dir = tempfile::tempdir().unwrap();
        collection(dir.path());
        let mut workspace = plain_root(dir.path(), RootRole::Workspace);
        workspace.ignore_files = strings(["*.mod"]);
        workspace.ignore_dirs = strings(["mods"]);
        let watched = [workspace];
        assert!(edit_moves(
            &watched,
            &[],
            &dir.path().join("mods/alpha.mod")
        ));
        assert!(edit_moves(
            &watched,
            &[],
            &dir.path().join("descriptor.mod")
        ));

        write(
            &dir.path().join("mods/gamma.mod"),
            "name = \"Gamma\"\npath = \"gamma\"\n",
        );
        fs::create_dir(dir.path().join("gamma")).unwrap();
        assert!(edit_moves(
            &watched,
            &[],
            &dir.path().join("gamma/common/z.txt")
        ));
    }

    #[test]
    fn the_descriptor_is_looked_up_the_way_the_loader_does() {
        use cwtools_file_manager::file_manager::read_root_descriptor;

        let dir = tempfile::tempdir().unwrap();
        let descriptor = dir.path().join("Descriptor.MOD");
        write(&descriptor, "replace_path = common\n");
        let loaded = read_root_descriptor(dir.path()).is_some();
        let watched = [plain_root(dir.path(), RootRole::Workspace)];
        assert_eq!(edit_moves(&watched, &[], &descriptor), loaded);
    }

    #[test]
    fn a_classification_change_applies_on_the_next_poll() {
        let dir = tempfile::tempdir().unwrap();
        let member_file = dir.path().join("resources/alpha/common/x.txt");
        fs::create_dir(dir.path().join("mods")).unwrap();
        write(&member_file, "x = 1\n");
        let watched = [plain_root(dir.path(), RootRole::Workspace)];
        assert!(!discovers(&watched[0], &[], &member_file));
        assert!(!edit_moves(&watched, &[], &member_file));

        write(
            &dir.path().join("mods/alpha.mod"),
            "name = \"Alpha\"\npath = \"resources/alpha\"\n",
        );
        assert!(discovers(&watched[0], &[], &member_file));
        assert!(edit_moves(&watched, &[], &member_file));
    }

    #[test]
    fn a_game_folder_reclassifies_a_collection() {
        let dir = tempfile::tempdir().unwrap();
        collection(dir.path());
        write(&dir.path().join("alpha/common/x.txt"), "x = 1\n");
        let watched = [plain_root(dir.path(), RootRole::Workspace)];
        let before = fingerprint(&watched, &[]);
        fs::create_dir(dir.path().join("events")).unwrap();
        assert_ne!(fingerprint(&watched, &[]), before);
        fs::remove_dir(dir.path().join("events")).unwrap();
        assert_eq!(fingerprint(&watched, &[]), before);
    }

    #[test]
    fn a_rules_root_polls_exactly_the_files_the_loader_reads() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("mods/alpha.mod"),
            "name = \"Alpha\"\npath = \"alpha\"\n",
        );
        let watched = [plain_root(dir.path(), RootRole::Rules)];
        for (relative, expected) in [
            ("base.cwt", true),
            (".patches/patch.cwt", true),
            ("target/tool.cwt", true),
            (".git/cached.cwt", true),
            ("logs/modifiers.log", true),
            ("modifiers.log", false),
            ("README.md", false),
            ("notes.txt", false),
            // The loader never follows the mods a descriptor names.
            ("alpha/note.txt", false),
        ] {
            let file = dir.path().join(relative);
            assert_eq!(edit_moves(&watched, &[], &file), expected, "{relative}");
        }
    }

    #[test]
    fn a_rules_root_may_be_a_single_file_and_a_missing_root_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rules.cwt");
        write(&file, "types = {}\n");
        let watched = [
            plain_root(&file, RootRole::Rules),
            plain_root(&dir.path().join("missing"), RootRole::Rules),
            plain_root(&dir.path().join("absent"), RootRole::Workspace),
        ];
        let before = fingerprint(&watched, &[]);
        write(&file, "types = { }\n");
        assert_ne!(fingerprint(&watched, &[]), before);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        use std::os::unix::fs::symlink;

        let rules = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("outside.txt"), "a = 1\n");
        write(&outside.path().join("outside.cwt"), "types = { }\n");
        let common = dir.path().join("common");
        fs::create_dir(&common).unwrap();
        symlink(outside.path().join("outside.txt"), common.join("link.txt")).unwrap();
        symlink(&common, common.join("loop")).unwrap();
        let hidden = rules.path().join(".patches");
        fs::create_dir(&hidden).unwrap();
        symlink(outside.path().join("outside.cwt"), hidden.join("link.cwt")).unwrap();
        symlink(&hidden, hidden.join("loop")).unwrap();
        symlink(outside.path(), rules.path().join("linked")).unwrap();

        let watched = roots(rules.path(), dir.path(), &[], None, &[], &[]);
        let before = fingerprint(&watched, &[]);
        write(&outside.path().join("outside.txt"), "a = 1, and longer\n");
        write(&outside.path().join("outside.cwt"), "types = {  }\n");
        assert_eq!(fingerprint(&watched, &[]), before);

        write(&hidden.join("real.cwt"), "types = { }\n");
        assert_ne!(fingerprint(&watched, &[]), before);
    }

    #[test]
    fn roots_are_watched_once_per_role() {
        let rules = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let roles = |watched: &[Root]| -> Vec<(PathBuf, RootRole)> {
            watched
                .iter()
                .map(|root| (root.path.clone(), root.role))
                .collect()
        };

        let watched = roots(
            rules.path(),
            dir.path(),
            &[dir.path().to_path_buf()],
            None,
            &[],
            &[],
        );
        assert_eq!(
            roles(&watched),
            [
                (rules.path().to_path_buf(), RootRole::Rules),
                (dir.path().to_path_buf(), RootRole::Workspace),
            ]
        );

        let same = rules.path();
        let watched = roots(same, same, &[], Some(same), &[], &[]);
        assert_eq!(
            roles(&watched),
            [
                (same.to_path_buf(), RootRole::Rules),
                (same.to_path_buf(), RootRole::Workspace),
                (same.to_path_buf(), RootRole::Vanilla),
            ]
        );
    }

    #[test]
    fn relative_default_paths_are_watched() {
        let watched = roots(Path::new("."), Path::new("."), &[], None, &[], &[]);
        assert_eq!(watched[0].path, Path::new("."));
        assert_eq!(watched[1].path, Path::new("."));
        assert_ne!(fingerprint(&watched, &[]), 0);
    }

    #[test]
    fn ignore_lists_apply_to_the_workspace_and_parents_but_not_vanilla() {
        let rules = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ignore_files = strings(["common/skip.txt"]);
        let file = dir.path().join("common/skip.txt");
        let workspace = roots(rules.path(), dir.path(), &[], None, &[], &ignore_files);
        assert!(!edit_moves(&workspace, &[], &file));

        // A pattern is relative to the root that carries it, so a parent
        // rooted at `common` does not match.
        let parents = [dir.path().join("common")];
        let parent = roots(rules.path(), dir.path(), &parents, None, &[], &ignore_files);
        assert!(edit_moves(&parent, &[], &file));

        let vanilla = Some(dir.path());
        let vanilla = roots(rules.path(), dir.path(), &[], vanilla, &[], &ignore_files);
        assert!(edit_moves(&vanilla, &[], &file));
    }

    #[test]
    fn an_explicit_vanilla_install_is_watched() {
        let rules = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let vanilla = tempfile::tempdir().unwrap();
        let vanilla_dir = Some(vanilla.path());
        let watched = roots(rules.path(), workspace.path(), &[], vanilla_dir, &[], &[]);
        let poller = Poller::new(watched);
        write(&vanilla.path().join("events/vanilla.txt"), "x = 1\n");
        assert_ne!(poller.current(), poller.seen);
    }

    #[test]
    fn an_edit_is_seen_once_the_roots_hold_still() {
        let dir = tempfile::tempdir().unwrap();
        let poller = Poller::new(vec![plain_root(dir.path(), RootRole::Workspace)]);
        let file = dir.path().join("common/x.txt");
        let mut sleeps = 0;
        let snapshot = poller.wait_with(|_| {
            sleeps += 1;
            // Two saves in a row, then quiet: the change counts once, after it settles.
            match sleeps {
                1 => write(&file, "a = 1\n"),
                2 => write(&file, "a = 1 # and a longer body\n"),
                _ => {}
            }
        });
        assert_eq!(sleeps, 3);
        assert_eq!(snapshot, poller.current());
        assert_ne!(snapshot, poller.seen);
    }

    #[test]
    fn failed_validation_does_not_consume_the_change() {
        let dir = tempfile::tempdir().unwrap();
        let poller = Poller::new(vec![plain_root(dir.path(), RootRole::Workspace)]);
        let previous = poller.seen;
        write(&dir.path().join("common/changed.txt"), "change");
        poller.wait_with(|_| {});
        assert_eq!(poller.seen, previous);
        assert_ne!(poller.current(), poller.seen);
    }

    #[test]
    fn retries_failed_passes_with_bounded_backoff_and_a_fresh_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let mut poller = Poller::new(vec![plain_root(dir.path(), RootRole::Workspace)]);
        let original = poller.current();
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
                write(&dir.path().join("common/changed.txt"), "change");
            },
        );
        assert_eq!(result, "ok");
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30]);
        assert_ne!(snapshot, original);
        assert_eq!(snapshot, poller.current());
    }

    #[test]
    fn a_change_during_a_successful_retry_is_not_consumed() {
        let dir = tempfile::tempdir().unwrap();
        let mut poller = Poller::new(vec![plain_root(dir.path(), RootRole::Workspace)]);
        let before = poller.current();
        let mut attempts = 0;
        let (snapshot, ()) = retry(
            &mut poller,
            before,
            &mut |_poller| {
                attempts += 1;
                if attempts == 1 {
                    None
                } else {
                    write(&dir.path().join("common/during.txt"), "new input");
                    Some(())
                }
            },
            |_| {},
        );
        assert_eq!(attempts, 2);
        assert_eq!(snapshot, before);
        assert_ne!(snapshot, poller.current());
    }

    /// A workspace whose `map` folder only the ruleset names, so the poll lists
    /// different files before and after the first load reports its folders.
    fn map_workspace() -> (tempfile::TempDir, Poller) {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("common/a.txt"), "a = 1\n");
        write(&dir.path().join("map/b.txt"), "b = 1\n");
        let poller = Poller::new(vec![plain_root(dir.path(), RootRole::Workspace)]);
        let tuned = fingerprint(&poller.roots, &strings(["common", "map"]));
        assert_ne!(poller.seen, tuned, "the fixture must list different files");
        (dir, poller)
    }

    #[test]
    fn learning_the_folders_is_not_a_change() {
        let (_dir, mut poller) = map_workspace();
        poller.retune(strings(["common", "map"]));
        assert_eq!(poller.current(), poller.seen);
    }

    #[test]
    fn an_edit_saved_during_the_first_load_stays_pending() {
        let (dir, mut poller) = map_workspace();
        write(
            &dir.path().join("common/a.txt"),
            "a = 1 # edited during the load\n",
        );
        poller.retune(strings(["common", "map"]));
        assert_ne!(poller.current(), poller.seen);
    }

    #[test]
    fn a_pass_that_updates_the_roots_does_not_wake_again_without_an_edit() {
        let rules = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        write(&parent.path().join("common/existing.txt"), "a = 1\n");
        let mut poller = Poller::new(roots(rules.path(), dir.path(), &[], None, &[], &[]));
        poller.retune(strings(["common"]));
        write(&dir.path().join("common/dep.txt"), "a = 1\n");
        let snapshot = poller.wait_with(|_| {});
        assert_ne!(snapshot, poller.seen);

        let (snapshot, ()) = retry(
            &mut poller,
            snapshot,
            &mut |poller| {
                poller.retune_roots(roots(
                    rules.path(),
                    dir.path(),
                    &[parent.path().to_path_buf()],
                    None,
                    &[],
                    &[],
                ));
                Some(())
            },
            |_| {},
        );
        poller.settle(snapshot);
        assert_eq!(
            poller.current(),
            poller.seen,
            "changed roots alone are not a change"
        );
        write(&parent.path().join("common/keep.txt"), "a = 2\n");
        assert_ne!(poller.current(), poller.seen, "the added root is watched");
    }

    #[test]
    fn an_edit_saved_during_a_pass_that_updates_the_roots_stays_pending() {
        let rules = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let mut poller = Poller::new(roots(rules.path(), dir.path(), &[], None, &[], &[]));
        poller.retune(strings(["common"]));
        write(&dir.path().join("common/dep.txt"), "a = 1\n");
        let snapshot = poller.wait_with(|_| {});

        let (snapshot, ()) = retry(
            &mut poller,
            snapshot,
            &mut |poller| {
                poller.retune_roots(roots(
                    rules.path(),
                    dir.path(),
                    &[parent.path().to_path_buf()],
                    None,
                    &[],
                    &[],
                ));
                write(&dir.path().join("common/mid.txt"), "a = 1\n");
                Some(())
            },
            |_| {},
        );
        poller.settle(snapshot);
        assert_ne!(
            poller.current(),
            poller.seen,
            "the mid-pass edit stays pending"
        );
    }

    #[test]
    fn an_edit_between_the_wake_and_the_roots_swap_keeps_the_pre_attempt_baseline() {
        let rules = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let mut poller = Poller::new(roots(rules.path(), dir.path(), &[], None, &[], &[]));
        poller.retune(strings(["common"]));
        write(&dir.path().join("common/dep.txt"), "a = 1\n");
        let snapshot = poller.wait_with(|_| {});

        let (snapshot, ()) = retry(
            &mut poller,
            snapshot,
            &mut |poller| {
                // The parents were resolved here; the descriptor (or another
                // watched file) changes again before the roots are retuned.
                write(&dir.path().join("common/later.txt"), "a = 1\n");
                poller.retune_roots(roots(
                    rules.path(),
                    dir.path(),
                    &[parent.path().to_path_buf()],
                    None,
                    &[],
                    &[],
                ));
                Some(())
            },
            |_| {},
        );
        poller.settle(snapshot);
        assert_ne!(
            poller.current(),
            poller.seen,
            "the edit after the wake stays pending for the next wake"
        );
    }

    #[test]
    fn a_retry_after_a_failing_roots_swapping_attempt_adopts_its_fresh_baseline() {
        let rules = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let mut poller = Poller::new(roots(rules.path(), dir.path(), &[], None, &[], &[]));
        poller.retune(strings(["common"]));
        write(&dir.path().join("common/dep.txt"), "a = 1\n");
        let snapshot = poller.wait_with(|_| {});

        let mut attempts = 0;
        let (snapshot, ()) = retry(
            &mut poller,
            snapshot,
            &mut |poller| {
                attempts += 1;
                if attempts == 1 {
                    poller.retune_roots(roots(
                        rules.path(),
                        dir.path(),
                        &[parent.path().to_path_buf()],
                        None,
                        &[],
                        &[],
                    ));
                    None // the first pass could not read its inputs
                } else {
                    Some(())
                }
            },
            |_| {
                // An edit lands between the failed attempt and the retry.
                write(&dir.path().join("common/intervening.txt"), "a = 1\n");
            },
        );
        poller.settle(snapshot);
        assert_eq!(
            poller.current(),
            poller.seen,
            "the retry's fresh baseline covers the edit its pass validated"
        );
    }
}
