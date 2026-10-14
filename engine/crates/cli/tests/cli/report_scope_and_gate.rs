use crate::common::{
    INHERITED_GIT_ENV, broken_rules_dir, cwtools, fixture_text, fixtures_dir, validate_dir,
    write_tree,
};
use predicates::prelude::*;

// ── Exit gate (--fail-on) ────────────────────────────────────────────────────
//
// mod_a's only diagnostic is an Information-severity CW107, so the fixture
// exits 0 by default and the gate is the only thing that can change that.

#[test]
fn test_validate_fail_on_default_ignores_a_sub_error_diagnostic() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    validate_dir(&discover_dir, &[])
        .success()
        .stdout(predicate::str::contains("CW107"));
}

#[test]
fn test_validate_fail_on_info_fails_on_an_information_diagnostic() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    validate_dir(&discover_dir, &["--fail-on", "info"])
        .code(1)
        .stdout(predicate::str::contains("CW107"));
}

#[test]
fn test_validate_fail_on_warning_leaves_an_information_diagnostic_alone() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    validate_dir(&discover_dir, &["--fail-on", "warning"]).success();
}

/// The report-only case: publish the findings, never redden the build.
#[test]
fn test_validate_fail_on_none_never_fails() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    validate_dir(&discover_dir, &["--fail-on", "none"])
        .success()
        .stdout(predicate::str::contains("CW107"));
}

/// `--min-severity` filters before the gate counts, so asking to fail on
/// something it already dropped can never trip.
#[test]
fn test_validate_min_severity_filters_before_the_gate() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    validate_dir(
        &discover_dir,
        &["--min-severity", "error", "--fail-on", "info"],
    )
    .success()
    .stdout(predicate::str::contains("CW107").not());
}

#[test]
fn test_validate_fail_on_unknown_fails() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    validate_dir(&discover_dir, &["--fail-on", "bogus"])
        .failure()
        .stderr(predicate::str::contains("invalid severity 'bogus'"))
        .stderr(predicate::str::contains("none"));
}

/// The ruleset's own warning (CW603) is below the default gate; asking for it
/// is how CI fails on a `.cwt` that only degrades checks rather than breaking.
#[test]
fn test_rules_fail_on_none_reports_without_failing() {
    let rules = broken_rules_dir();
    cwtools()
        .args(["rules", rules.path().to_str().unwrap(), "--fail-on", "none"])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW601"));
}

/// `loc_error` carries an Error-severity CW225, so the default gate fails it.
#[test]
fn test_loc_fail_on_none_reports_an_error_without_failing() {
    let loc_dir = fixtures_dir().join("loc_error");
    cwtools()
        .args(["loc", loc_dir.to_str().unwrap(), "--fail-on", "none"])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW225"));
}

/// `loc_invalid`'s CW268 is a warning, below the default gate.
#[test]
fn test_loc_fail_on_warning_fails_on_a_warning() {
    let loc_dir = fixtures_dir().join("loc_invalid");
    cwtools()
        .args(["loc", loc_dir.to_str().unwrap(), "--fail-on", "warning"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("CW268"));
}

// ── Report scope (--file / --since) ──────────────────────────────────────────
//
// Both scope the report, not the run: the directory is still indexed whole, so
// the cross-file checks behind CW100/CW113 see everything either way. Of the
// mod_a fixture's two files only the event produces a diagnostic (CW107), so
// whether it survives says which files the run reported on.

/// [`validate_dir`] with `GIT_DIR` set, standing in for a run inside a git hook.
fn validate_dir_env(
    dir: &std::path::Path,
    extra: &[&str],
    git_dir: &std::path::Path,
) -> assert_cmd::assert::Assert {
    let rules_dir = fixtures_dir().join("rules");
    let mut cmd = cwtools();
    cmd.env("GIT_DIR", git_dir);
    cmd.args([
        "validate",
        "--game",
        "stellaris",
        "--directory",
        dir.to_str().unwrap(),
        "--rules",
        rules_dir.to_str().unwrap(),
    ]);
    cmd.args(extra);
    cmd.assert()
}

#[test]
fn test_validate_file_scopes_the_report() {
    let mod_dir = fixtures_dir().join("discover").join("mod_a");
    let quiet = mod_dir.join("common").join("test.txt");
    let noisy = mod_dir.join("events").join("test.txt");

    validate_dir(&mod_dir, &["--file", quiet.to_str().unwrap()])
        .success()
        .stdout(predicate::str::contains("CW107").not());
    validate_dir(&mod_dir, &["--file", noisy.to_str().unwrap()])
        .success()
        .stdout(predicate::str::contains("CW107"));
}

/// A relative `--file` and an absolute one name the same file: the diagnostic's
/// own path spelling follows `--directory`, so the two sides have to be
/// normalised before they are compared.
#[test]
fn test_validate_file_matches_across_path_spellings() {
    let mod_dir = fixtures_dir().join("discover").join("mod_a");
    let noisy = mod_dir.join("events").join(".").join("test.txt");
    validate_dir(&mod_dir, &["--file", noisy.to_str().unwrap()])
        .success()
        .stdout(predicate::str::contains("CW107"));
}

/// A typo'd `--file` is named rather than silently scoping the run to nothing.
/// It is a warning, not an error: a `--since` list legitimately covers files
/// that no longer exist.
#[test]
fn test_validate_file_that_is_not_on_disk_warns_and_still_runs() {
    let mod_dir = fixtures_dir().join("discover").join("mod_a");
    let noisy = mod_dir.join("events").join("test.txt");
    validate_dir(
        &mod_dir,
        &[
            "--file",
            noisy.to_str().unwrap(),
            "--file",
            mod_dir.join("nope.txt").to_str().unwrap(),
        ],
    )
    .success()
    .stderr(predicate::str::contains("nope.txt is not on disk"))
    .stdout(predicate::str::contains("CW107"));
}

/// A baseline written from a scoped run would drop every hash outside the
/// scope, which is a quiet way to lose one.
#[test]
fn test_validate_output_hashes_with_a_scope_warns() {
    let tmp = tempfile::tempdir().unwrap();
    let mod_dir = fixtures_dir().join("discover").join("mod_a");
    let hashes = tmp.path().join("baseline.txt");
    validate_dir(
        &mod_dir,
        &[
            "--file",
            mod_dir.join("events").join("test.txt").to_str().unwrap(),
            "--output-hashes",
            hashes.to_str().unwrap(),
        ],
    )
    .success()
    .stderr(predicate::str::contains("--output-hashes"));
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let mut command = std::process::Command::new("git");
    command.arg("-C").arg(dir).args(args);
    for key in INHERITED_GIT_ENV {
        command.env_remove(key);
    }
    let out = command.output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A git repo holding the mod_a fixture, committed, so `--since HEAD` has a
/// clean tree to start from. Identity and signing are set on the repo so the
/// commit doesn't depend on the developer's global git config.
fn git_repo_mod() -> tempfile::TempDir {
    let common = fixture_text("discover/mod_a/common/test.txt");
    let events = fixture_text("discover/mod_a/events/test.txt");
    let tmp = write_tree(&[("common/test.txt", &common), ("events/test.txt", &events)]);
    git(tmp.path(), &["init"]);
    git(tmp.path(), &["config", "user.email", "test@example.com"]);
    git(tmp.path(), &["config", "user.name", "cwtools tests"]);
    git(tmp.path(), &["config", "commit.gpgsign", "false"]);
    git(tmp.path(), &["add", "."]);
    git(tmp.path(), &["commit", "--no-verify", "-m", "fixture"]);
    tmp
}

#[test]
fn test_validate_since_scopes_to_the_changed_files() {
    let repo = git_repo_mod();
    let event = repo.path().join("events").join("test.txt");

    // Nothing has changed since HEAD, so the event's CW107 is out of scope.
    validate_dir(repo.path(), &["--since", "HEAD"])
        .success()
        .stdout(predicate::str::contains("CW107").not());

    // Editing it brings it back. The trailing comment leaves the event itself
    // (and so the diagnostic's line) alone.
    let mut text = std::fs::read_to_string(&event).unwrap();
    text.push_str("# touched\n");
    std::fs::write(&event, text).unwrap();
    validate_dir(repo.path(), &["--since", "HEAD"])
        .success()
        .stdout(predicate::str::contains("CW107"));
}

/// A file that was never committed is the case a scoped run most wants to
/// catch, so untracked files count as changed.
#[test]
fn test_validate_since_covers_untracked_files() {
    let repo = git_repo_mod();
    std::fs::write(
        repo.path().join("events").join("new.txt"),
        "namespace = new_events\n\ncountry_event = {\n\tid = new.1\n}\n",
    )
    .unwrap();
    validate_dir(repo.path(), &["--since", "HEAD"])
        .success()
        .stdout(predicate::str::contains("CW107"));
}

/// Git answers `rev-parse` in its own, resolved spelling of the repo root,
/// while discovery walks the path the run was handed. Where the two differ the
/// scope used to match nothing and the run reported on no files at all
/// ("Discovered 3 files / Reporting on 0 of them"): on Windows a temp dir is an
/// 8.3 short path (`RUNNER~1`) that git reports long, and on macOS `/var` is a
/// symlink git reports as `/private/var`.
///
/// A `..` component reproduces the same class on every platform: `absolute()`
/// keeps it, git resolves it away.
#[test]
fn test_validate_since_matches_an_unresolved_directory_spelling() {
    let repo = git_repo_mod();
    let event = repo.path().join("events").join("test.txt");
    // `<repo>/events/..` names `<repo>` without being spelled like it.
    let indirect = repo.path().join("events").join("..");

    let mut text = std::fs::read_to_string(&event).unwrap();
    text.push_str("# touched\n");
    std::fs::write(&event, text).unwrap();

    validate_dir(&indirect, &["--since", "HEAD"])
        .success()
        .stdout(predicate::str::contains("CW107"));
}

/// `--since` exists for pre-commit and pre-push hooks, and git hands every hook
/// a `GIT_DIR` naming the repo the hook fired in. That variable outranks `-C`,
/// so the scope has to be resolved with it cleared or a hook run reports on the
/// wrong repository's diff without failing.
#[test]
fn test_validate_since_ignores_an_inherited_git_dir() {
    let repo = git_repo_mod();
    let elsewhere = git_repo_mod();
    let event = repo.path().join("events").join("test.txt");
    let hook_env = elsewhere.path().join(".git");

    // Clean tree: the event's CW107 is out of scope, and stays out however the
    // ambient GIT_DIR is set.
    validate_dir_env(repo.path(), &["--since", "HEAD"], &hook_env)
        .success()
        .stdout(predicate::str::contains("CW107").not());

    // Touching the file brings it back, which it cannot do if the diff was
    // taken against `elsewhere` (where nothing changed).
    let mut text = std::fs::read_to_string(&event).unwrap();
    text.push_str("# touched\n");
    std::fs::write(&event, text).unwrap();
    validate_dir_env(repo.path(), &["--since", "HEAD"], &hook_env)
        .success()
        .stdout(predicate::str::contains("CW107"));
}

/// A ref git can't resolve fails the run. Carrying on would report either
/// everything or nothing, and both read as a pass.
#[test]
fn test_validate_since_unknown_ref_is_a_usage_error() {
    let repo = git_repo_mod();
    validate_dir(repo.path(), &["--since", "no/such/ref"])
        .failure()
        .code(2)
        .stderr(predicate::str::contains("--since"));
}
