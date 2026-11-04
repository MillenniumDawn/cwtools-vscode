//! `validate --watch`: notice that the inputs changed, and print what the
//! re-run did to the diagnostics.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::diag::{Diag, severity_rank};
use crate::run::note;

/// How often the inputs are stat-ed. A change must hold still for one more
/// poll before it counts, so a save that touches many files reloads once.
const POLL: Duration = Duration::from_millis(500);

/// Watches the roots a validate run reads by polling file metadata, so it
/// needs no platform watcher.
pub(crate) struct Poller {
    roots: Vec<PathBuf>,
    seen: u64,
}

impl Poller {
    /// Snapshot the roots. Take it before the first load starts, so an edit
    /// saved while that load runs is still seen as a change afterwards.
    pub(crate) fn new(roots: Vec<PathBuf>) -> Self {
        let seen = fingerprint(&roots);
        Self { roots, seen }
    }

    /// Block until the roots differ from the last snapshot and have stopped
    /// changing, then adopt the new state as the snapshot.
    fn wait(&mut self) {
        let mut last = self.seen;
        loop {
            thread::sleep(POLL);
            let now = fingerprint(&self.roots);
            if now != self.seen && now == last {
                self.seen = now;
                return;
            }
            last = now;
        }
    }
}

/// Run `rerun` after every change and print its delta against the previous
/// result. `rerun` returns `None` when the pass could not read its inputs, which
/// keeps the previous result. Never returns: Ctrl-C ends the process.
pub(crate) fn follow(
    mut poller: Poller,
    mut previous: Vec<Diag>,
    mut rerun: impl FnMut() -> Option<Vec<Diag>>,
) -> ! {
    note("Watching for changes (Ctrl-C to stop)");
    loop {
        poller.wait();
        note("Change detected, validating again");
        let Some(next) = rerun() else { continue };
        print!("{}", delta_report(&previous, &next));
        previous = next;
    }
}

/// Order-independent digest of the path, modified time and size of every file
/// under `roots`. A root may be a single file. Dot-directories such as `.git`
/// are skipped: tools rewrite their contents all the time and no input lives
/// there. Unreadable entries are skipped, since the pass that follows reports them.
fn fingerprint(roots: &[PathBuf]) -> u64 {
    let mut total = 0u64;
    let mut dirs: Vec<PathBuf> = Vec::new();
    for root in roots {
        match fs::metadata(root) {
            Ok(meta) if meta.is_dir() => dirs.push(root.clone()),
            Ok(meta) => total = total.wrapping_add(entry_digest(root, &meta)),
            Err(_) => {}
        }
    }
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_dir() {
                total = total.wrapping_add(entry_digest(&entry.path(), &meta));
            } else if !entry.file_name().to_string_lossy().starts_with('.') {
                dirs.push(entry.path());
            }
        }
    }
    total
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

    #[test]
    fn fingerprint_moves_when_a_file_is_added_changed_or_removed() {
        let dir = tempfile::tempdir().unwrap();
        let roots = vec![dir.path().to_path_buf()];
        let file = dir.path().join("sub").join("x.txt");
        fs::create_dir(dir.path().join("sub")).unwrap();
        let empty = fingerprint(&roots);
        assert_eq!(empty, fingerprint(&roots), "stable while nothing changes");

        fs::write(&file, "a = 1\n").unwrap();
        let added = fingerprint(&roots);
        assert_ne!(added, empty);

        fs::write(&file, "a = 10\n").unwrap();
        let resized = fingerprint(&roots);
        assert_ne!(resized, added);

        fs::remove_file(&file).unwrap();
        assert_eq!(fingerprint(&roots), empty);
    }

    #[test]
    fn fingerprint_accepts_a_single_file_root_and_a_missing_root() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rules.cwt");
        fs::write(&file, "types = {}\n").unwrap();
        let roots = vec![file.clone(), dir.path().join("missing")];
        let before = fingerprint(&roots);
        fs::write(&file, "types = { }\n").unwrap();
        assert_ne!(fingerprint(&roots), before);
    }

    #[test]
    fn fingerprint_ignores_dot_directories() {
        let dir = tempfile::tempdir().unwrap();
        let roots = vec![dir.path().to_path_buf()];
        let before = fingerprint(&roots);
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git").join("index"), "x").unwrap();
        assert_eq!(fingerprint(&roots), before);
    }
}
