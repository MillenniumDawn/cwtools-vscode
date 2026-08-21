//! `--file` / `--since`: which files a `validate` run reports on.
//!
//! Both are report filters, not a smaller run. The cross-file checks need every
//! file indexed regardless — CW100 resolves against the whole loc union, CW113
//! against the whole file index — so a scoped run buys a report and an exit code
//! about the files you touched, plus, where the ruleset has no cross-file use
//! pass to run, skipping the per-file validation of everything else
//! (`SessionWithFiles::validate_selected`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The files a run reports on, keyed as absolute `/`-separated paths so a
/// diagnostic matches whether the run spelled its directory relative or absolute.
#[derive(Debug)]
pub(crate) struct FileScope {
    wanted: HashSet<String>,
    /// `--file` values naming nothing on disk, for the warning. A `--since` list
    /// routinely names files that were deleted or never validated (`README.md`,
    /// `.github/…`), so only the explicitly named ones are reported.
    missing: Vec<PathBuf>,
}

/// Resolve the run's scope. `Ok(None)` means neither flag was given, i.e. the
/// run reports on everything.
pub(crate) fn resolve(
    files: &[PathBuf],
    since: Option<&str>,
    directory: &Path,
) -> Result<Option<FileScope>, String> {
    if files.is_empty() && since.is_none() {
        return Ok(None);
    }
    let mut wanted: HashSet<String> = files.iter().map(|p| key(p)).collect();
    if let Some(reference) = since {
        wanted.extend(changed_since(reference, directory)?.iter().map(|p| key(p)));
    }
    let missing = files.iter().filter(|p| !p.exists()).cloned().collect();
    Ok(Some(FileScope { wanted, missing }))
}

/// Whether `file` is in the run's scope. Mirrors [`crate::codes::wanted`]: no
/// scope means everything is wanted.
pub(crate) fn wanted(scope: Option<&FileScope>, file: &str) -> bool {
    scope.is_none_or(|s| s.contains(file))
}

impl FileScope {
    pub(crate) fn contains(&self, file: &str) -> bool {
        self.wanted.contains(&key(Path::new(file)))
    }

    /// The discovered files the scope covers, spelled the way the session
    /// discovered them so the driver can match them without path semantics.
    pub(crate) fn select<'a>(
        &self,
        discovered: impl Iterator<Item = &'a Path>,
    ) -> HashSet<PathBuf> {
        discovered
            .filter(|p| self.wanted.contains(&key(p)))
            .map(Path::to_path_buf)
            .collect()
    }

    pub(crate) fn missing(&self) -> &[PathBuf] {
        &self.missing
    }
}

/// A path as a comparison key: absolute where that can be computed, `.` and
/// duplicate separators folded out, `/`-separated. Lexical only, no filesystem
/// access — a diagnostic can name a file that has since been deleted.
fn key(path: &Path) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    abs.components()
        .collect::<PathBuf>()
        .to_string_lossy()
        .replace('\\', "/")
}

/// The files that changed between `reference` and the working tree, absolute.
/// Diffed from the merge base rather than from `reference` itself, so a branch
/// checked against a `main` that has moved on reports what the branch changed
/// and not what main did; against `HEAD` the merge base is `HEAD`, which is what
/// a pre-commit hook wants. Untracked files count as changed — a newly added
/// script is the case a scoped run most wants to catch.
fn changed_since(reference: &str, directory: &Path) -> Result<Vec<PathBuf>, String> {
    let root = PathBuf::from(git(directory, &["rev-parse", "--show-toplevel"])?.trim_end());
    // Where `directory` sits inside the repo, `/`-separated with a trailing
    // slash, empty when it is the repo root. This is what lets a git-reported
    // path be re-spelled the way the run spells its own files; see [`rerooted`].
    let prefix = git(directory, &["rev-parse", "--show-prefix"])?;
    let prefix = prefix.trim_end_matches(['\n', '\r']);
    let base = git(&root, &["merge-base", reference, "HEAD"])?;
    let changed = git(&root, &["diff", "--name-only", "-z", base.trim_end()])?;
    let untracked = git(&root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    Ok(changed
        .split('\0')
        .chain(untracked.split('\0'))
        .filter(|s| !s.is_empty())
        .map(|s| rerooted(s, prefix, directory, &root))
        .collect())
}

/// One repo-relative path from git, as an absolute path spelled the way this run
/// spells the files it discovered.
///
/// Git answers in its own spelling of the repository root, which is the resolved
/// one: on Windows a `--directory` under an 8.3 short path (`RUNNER~1`) comes
/// back long, and on macOS a `/var/folders` temp dir comes back as
/// `/private/var/folders`. Discovery walked the path the run was *given*, so
/// comparing the two spellings directly matches nothing and the run reports on
/// no files at all. Rebuilding the path under `directory` keeps both sides in
/// the caller's spelling, which is the only one both halves agree on.
///
/// A file outside `directory` (the repo root's own README, a sibling crate)
/// keeps git's spelling: nothing discovered can match it either way, and it is
/// still the honest absolute path for the `--file` warning to name.
fn rerooted(repo_relative: &str, prefix: &str, directory: &Path, root: &Path) -> PathBuf {
    match repo_relative.strip_prefix(prefix) {
        Some(under_directory) => directory.join(under_directory),
        None => root.join(repo_relative),
    }
}

/// Variables git exports to a hook, which name the hook's own repository. An
/// inherited `GIT_DIR` outranks `-C`, so a `--since` run from a pre-commit or
/// pre-push hook (the case the flag exists for) would resolve the ref and the
/// changed-file list against that repository instead of the mod directory, and
/// report the wrong file set without failing.
const INHERITED_GIT_ENV: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_DIR",
    "GIT_INDEX_FILE",
    "GIT_NAMESPACE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX",
    "GIT_WORK_TREE",
];

/// One `git` call in `dir`: its stdout on success, a message naming the command
/// and git's own stderr on failure. A `--since` that can't be resolved fails the
/// run, because both ways of carrying on — reporting on everything, or on
/// nothing — read as a pass.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir).args(args);
    for key in INHERITED_GIT_ENV {
        command.env_remove(key);
    }
    let output = command
        .output()
        .map_err(|e| format!("--since needs git on PATH: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "--since: `git {}` failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim_end()
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|e| format!("--since: git printed a path that is not UTF-8: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same file, however the spelling reached us, is one key.
    #[test]
    fn key_is_stable_across_path_spellings() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let want = key(&dir.join("common").join("x.txt"));
        assert_eq!(key(&dir.join("./common/x.txt")), want, "a `./` component");
        assert_eq!(key(&dir.join("common//x.txt")), want, "a doubled separator");
        assert!(want.contains("common/x.txt"), "`/`-separated: {want}");
    }

    /// A relative path resolves against the CWD, so a diagnostic reported
    /// relative to it matches a `--file` given the same way.
    #[test]
    fn a_relative_path_keys_the_same_as_its_absolute_form() {
        let rel = Path::new("common/x.txt");
        let abs = std::env::current_dir().unwrap().join(rel);
        assert_eq!(key(rel), key(&abs));
    }

    #[test]
    fn no_flags_means_no_scope() {
        let scope = resolve(&[], None, Path::new(".")).unwrap();
        assert!(scope.is_none());
        assert!(wanted(None, "anything.txt"), "no scope wants everything");
    }

    #[test]
    fn contains_matches_across_spellings_and_rejects_others() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("common").join("x.txt");
        let scope = resolve(std::slice::from_ref(&file), None, tmp.path())
            .unwrap()
            .unwrap();

        assert!(scope.contains(file.to_str().unwrap()));
        assert!(scope.contains(tmp.path().join("./common/x.txt").to_str().unwrap()));
        assert!(!scope.contains(tmp.path().join("common/y.txt").to_str().unwrap()));
        assert!(wanted(Some(&scope), file.to_str().unwrap()));
        assert!(!wanted(
            Some(&scope),
            tmp.path().join("common/y.txt").to_str().unwrap()
        ));
    }

    #[test]
    fn select_keeps_the_discovered_spelling() {
        let tmp = tempfile::tempdir().unwrap();
        let wanted_file = tmp.path().join("a.txt");
        let other = tmp.path().join("b.txt");
        let scope = resolve(std::slice::from_ref(&wanted_file), None, tmp.path())
            .unwrap()
            .unwrap();

        let selected = scope.select([wanted_file.as_path(), other.as_path()].into_iter());
        assert_eq!(selected, HashSet::from([wanted_file]));
    }

    /// A typo'd `--file` is worth naming; the run still proceeds, because a
    /// scope that covers nothing is a legitimate "nothing to report".
    #[test]
    fn missing_names_only_the_files_that_are_not_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real.txt");
        std::fs::write(&real, "x = yes\n").unwrap();
        let typo = tmp.path().join("relal.txt");

        let scope = resolve(&[real, typo.clone()], None, tmp.path())
            .unwrap()
            .unwrap();
        assert_eq!(scope.missing(), [typo]);
    }

    /// `git init` in `dir`, with the hook environment cleared for the same
    /// reason [`INHERITED_GIT_ENV`] exists: run under `cargo test` from a git
    /// hook, an inherited `GIT_DIR` would re-init the developer's repository
    /// instead of the temp dir.
    fn git_init(dir: &Path) -> bool {
        let mut command = Command::new("git");
        command.arg("-C").arg(dir).arg("init");
        for key in INHERITED_GIT_ENV {
            command.env_remove(key);
        }
        command.output().is_ok_and(|o| o.status.success())
    }

    /// An absolute path spelled the host's way: a leading `/` names the current
    /// drive's root on Windows, which is not what an absolute path means there.
    fn abs(tail: &str) -> String {
        if cfg!(windows) {
            format!("C:/{tail}")
        } else {
            format!("/{tail}")
        }
    }

    /// Git answers in its own spelling of the repo root. Where that differs from
    /// the one the run was handed — an 8.3 short path on Windows, a
    /// `/var` → `/private/var` symlink on macOS — the caller's spelling has to
    /// win, because that is the one discovery walked.
    #[test]
    fn a_changed_file_is_respelled_under_the_directory_it_was_given() {
        let directory = PathBuf::from(abs("given/mod"));
        let root = PathBuf::from(abs("resolved"));
        let file = rerooted("mod/events/x.txt", "mod/", &directory, &root);
        assert_eq!(
            file,
            directory.join("events/x.txt"),
            "got: {}",
            file.display()
        );
    }

    /// The mod directory being the repo root is the empty-prefix case, where
    /// every reported path is already relative to it.
    #[test]
    fn an_empty_prefix_roots_everything_at_the_directory() {
        let directory = PathBuf::from(abs("given"));
        let root = PathBuf::from(abs("resolved"));
        let file = rerooted("events/x.txt", "", &directory, &root);
        assert_eq!(
            file,
            directory.join("events/x.txt"),
            "got: {}",
            file.display()
        );
    }

    /// A file the mod directory does not contain keeps git's spelling. Nothing
    /// discovered can match it, and it is still the honest absolute path.
    #[test]
    fn a_file_outside_the_directory_keeps_the_repo_spelling() {
        let directory = PathBuf::from(abs("given/mod"));
        let root = PathBuf::from(abs("resolved"));
        let file = rerooted("README.md", "mod/", &directory, &root);
        assert_eq!(file, root.join("README.md"), "got: {}", file.display());
    }

    /// An unresolvable ref fails the run rather than quietly scoping it to
    /// nothing, and the message says which command could not be run.
    #[test]
    fn an_unknown_ref_is_an_error_naming_the_git_command() {
        let repo = tempfile::tempdir().unwrap();
        assert!(git_init(repo.path()), "git is required for this test");
        let e = resolve(&[], Some("no/such/ref"), repo.path()).unwrap_err();
        assert!(e.contains("--since"), "got: {e}");
        assert!(e.contains("merge-base"), "names the command: {e}");
    }

    #[test]
    fn a_directory_outside_a_repository_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let e = resolve(&[], Some("HEAD"), tmp.path()).unwrap_err();
        assert!(e.contains("rev-parse"), "got: {e}");
    }
}
