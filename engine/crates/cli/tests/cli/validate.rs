use crate::common::{broken_rules_dir, cwtools, fixtures_dir, validate_dir};
use predicates::prelude::*;

// ── Validate ─────────────────────────────────────────────────────────────────

#[test]
fn test_validate_with_rules() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            discover_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Validation complete"));
}

#[test]
fn test_validate_bad_game_name_fails() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args([
            "validate",
            "--game",
            "not_a_real_game",
            "--directory",
            discover_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("Unknown game: not_a_real_game"));
}

/// A broken `.cwt` degrades every check that runs against it, so `validate`
/// carries the ruleset's own problems in its report and fails on them.
#[test]
fn test_validate_reports_rules_problems() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    let rules = broken_rules_dir();
    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            discover_dir.to_str().unwrap(),
            "--rules",
            rules.path().to_str().unwrap(),
            "--report-type",
            "csv",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("CW601"))
        .stdout(predicate::str::contains("CW603"));
}

/// Rules problems answer to the same code policy as everything else: suppress
/// the error and the run is clean again.
#[test]
fn test_validate_ignore_code_suppresses_a_rules_problem() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    let rules = broken_rules_dir();
    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            discover_dir.to_str().unwrap(),
            "--rules",
            rules.path().to_str().unwrap(),
            "--report-type",
            "csv",
            "--ignore-code",
            "CW601",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW601").not())
        .stdout(predicate::str::contains("CW603"));
}

#[test]
fn test_validate_loc_language_valid_accepted() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            discover_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--loc-language",
            "english",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Validation complete"));
}

#[test]
fn test_validate_loc_language_unknown_fails() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            discover_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--loc-language",
            "klingon",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid language 'klingon'"))
        .stderr(predicate::str::contains("english"));
}

#[test]
fn test_validate_min_severity_filters_lower_severities() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    // mod_a's event triggers an Information-severity CW107; --min-severity
    // error should drop it from the report.
    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            discover_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--min-severity",
            "error",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW107").not());
}

#[test]
fn test_validate_min_severity_unknown_fails() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            discover_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--min-severity",
            "bogus",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid severity 'bogus'"));
}

// ── Output style (--quiet / --no-color) ──────────────────────────────────────

#[test]
fn test_quiet_drops_the_progress_chatter() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    validate_dir(&discover_dir, &["--quiet"])
        .success()
        .stderr(predicate::str::contains("Validating").not())
        .stderr(predicate::str::contains("Discovered").not())
        .stderr(predicate::str::contains("Loaded").not())
        // The report itself is the output, not chatter.
        .stdout(predicate::str::contains("CW107"))
        .stdout(predicate::str::contains("Validation complete"));
}

#[test]
fn test_quiet_short_flag_is_accepted() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    validate_dir(&discover_dir, &["-q"])
        .success()
        .stderr(predicate::str::contains("Validating").not());
}

/// A quiet run still has to say what it could not do, or a typo'd `--file`
/// reads as a clean one.
#[test]
fn test_quiet_keeps_warnings() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    validate_dir(&discover_dir, &["--quiet", "--file", "nope/missing.txt"])
        .success()
        .stderr(predicate::str::contains("is not on disk"));
}

/// The scan banner is a status line, so `--quiet` reaches it in `loc` too.
#[test]
fn test_quiet_drops_the_loc_scan_banner() {
    let loc_dir = fixtures_dir().join("loc");
    cwtools()
        .args(["loc", loc_dir.to_str().unwrap(), "--quiet"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Scanning localisation").not())
        .stdout(predicate::str::contains("Loc validation complete"));
}

/// Color is off for a piped report either way; `--no-color` must not change
/// the bytes a redirected run produces.
#[test]
fn test_no_color_leaves_a_piped_report_byte_identical() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    let plain = validate_dir(&discover_dir, &[]).get_output().stdout.clone();
    let forced = validate_dir(&discover_dir, &["--no-color"])
        .get_output()
        .stdout
        .clone();
    assert_eq!(plain, forced);
    assert!(
        !plain.contains(&0x1b),
        "a piped report must carry no escapes"
    );
}

#[test]
fn test_no_color_is_accepted_on_every_reporting_subcommand() {
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args(["rules", rules_dir.to_str().unwrap(), "--no-color"])
        .assert()
        .success();
    let loc_dir = fixtures_dir().join("loc");
    cwtools()
        .args(["loc", loc_dir.to_str().unwrap(), "--no-color"])
        .assert()
        .success();
}

// ── Empty inputs ─────────────────────────────────────────────────────────────
//
// A run that validated nothing must not look like a clean run: exit 4 names the
// empty input, exit 3 a path that doesn't resolve, and `--allow-empty` opts back
// into the old permissive behavior.

#[test]
fn test_validate_empty_rules_dir_fails() {
    let empty_rules = tempfile::tempdir().unwrap();
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            discover_dir.to_str().unwrap(),
            "--rules",
            empty_rules.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("--rules loaded 0 types"))
        .stderr(predicate::str::contains(
            empty_rules.path().to_str().unwrap(),
        ));
}

#[test]
fn test_validate_empty_rules_dir_allowed_with_flag() {
    let empty_rules = tempfile::tempdir().unwrap();
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            discover_dir.to_str().unwrap(),
            "--rules",
            empty_rules.path().to_str().unwrap(),
            "--allow-empty",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Validation complete"));
}

#[test]
fn test_validate_empty_directory_fails() {
    let empty_mod = tempfile::tempdir().unwrap();
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            empty_mod.path().to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("--directory contains no files"))
        .stderr(predicate::str::contains(empty_mod.path().to_str().unwrap()));
}

#[test]
fn test_validate_empty_directory_allowed_with_flag() {
    let empty_mod = tempfile::tempdir().unwrap();
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            empty_mod.path().to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--allow-empty",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Validation complete"));
}

#[test]
fn test_validate_missing_directory_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no_such_mod");
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            missing.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains(missing.to_str().unwrap()));
}

#[test]
fn test_validate_missing_directory_not_excused_by_allow_empty() {
    // --allow-empty covers a deliberately empty run, never a path that isn't there.
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no_such_mod");
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            missing.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--allow-empty",
        ])
        .assert()
        .failure()
        .code(3);
}

// ── Defaults are unchanged ───────────────────────────────────────────────────

/// A command line that worked before the config/CI work must still produce the
/// same bytes on stdout, the same exit code, and no extra stderr chatter.
#[test]
fn test_validate_default_report_type_matches_explicit_cli() {
    let mod_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    let args = [
        "validate",
        "--game",
        "stellaris",
        "--directory",
        mod_dir.to_str().unwrap(),
        "--rules",
        rules_dir.to_str().unwrap(),
    ];
    let default_out = cwtools().args(args).output().unwrap();
    let explicit_out = cwtools()
        .args(args)
        .args(["--report-type", "cli"])
        .output()
        .unwrap();
    assert_eq!(default_out.stdout, explicit_out.stdout);
    // Only the known transient cache warnings that race under parallel
    // `cargo test` sharing the scratch home are filtered; any other `warn:`
    // must remain visible so a real persistent cache regression is not hidden.
    fn without_cache_noise(buf: &[u8]) -> String {
        String::from_utf8_lossy(buf)
            .lines()
            .filter(|l| {
                !l.contains("warn: parse cache unavailable")
                    && !l.contains("warn: could not read base-game cache")
                    && !l.contains("  warn: could not write base-game cache")
                    && !l.contains("  Base-game cache")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    assert_eq!(
        without_cache_noise(&default_out.stderr),
        without_cache_noise(&explicit_out.stderr)
    );
    assert_eq!(default_out.status.code(), explicit_out.status.code());

    let stdout = String::from_utf8(default_out.stdout).unwrap();
    assert!(stdout.starts_with("\n  "), "grouped by file: {stdout:?}");
    assert!(stdout.contains("    [Information] [CW107] "), "{stdout:?}");
    assert!(
        stdout.ends_with("\nValidation complete: 0 errors, 0 warnings\n"),
        "{stdout:?}"
    );
    assert_eq!(default_out.status.code(), Some(0));
    // No config file anywhere above the fixtures, so no config chatter.
    let stderr = String::from_utf8(default_out.stderr).unwrap();
    assert!(!stderr.contains("Using config"), "{stderr:?}");
}

#[test]
fn test_validate_default_run_has_no_code_filter() {
    // Empty --ignore-code/--only-code lists must keep every diagnostic.
    let mod_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            mod_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--report-type",
            "csv",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(",CW107,"));
}
