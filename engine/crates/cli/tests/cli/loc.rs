use crate::common::{cwtools, fixture_text, fixtures_dir, write_tree};
use predicates::prelude::*;
use std::path::PathBuf;

// ── Loc ──────────────────────────────────────────────────────────────────────

#[test]
fn test_loc_valid_directory() {
    let loc_dir = fixtures_dir().join("loc");
    cwtools()
        .args(["loc", loc_dir.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Scanning localisation"))
        .stdout(predicate::str::contains("Loc validation complete"));
}

#[test]
fn test_loc_detects_unterminated_quote() {
    // CW268 is Warning-severity, so this now exits 0 (severity-aware exit,
    // like `validate`) even though the issue is still reported.
    let loc_dir = fixtures_dir().join("loc_invalid");
    cwtools()
        .args(["loc", loc_dir.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW268"))
        .stdout(predicate::str::contains("missing_quote"))
        .stdout(predicate::str::contains("1 issues"));
}

#[test]
fn test_loc_information_only_succeeds() {
    // CW234 (REPLACE_ME placeholder) is Information-severity; exit 0.
    let loc_dir = fixtures_dir().join("loc_info_only");
    cwtools()
        .args(["loc", loc_dir.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW234"));
}

#[test]
fn test_loc_error_severity_fails() {
    // CW225 (undefined loc reference) is Error-severity; exit 1 unchanged.
    let loc_dir = fixtures_dir().join("loc_error");
    cwtools()
        .args(["loc", loc_dir.to_str().unwrap()])
        .assert()
        .failure()
        .stdout(predicate::str::contains("CW225"));
}

#[test]
fn test_loc_empty_directory_fails() {
    // A loc scan that found no files is not a clean run (a typo'd path used to
    // report "0 entries" and exit 0).
    let tmp = tempfile::tempdir().unwrap();
    cwtools()
        .args(["loc", tmp.path().to_str().unwrap()])
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("no localisation files found"))
        .stderr(predicate::str::contains(tmp.path().to_str().unwrap()));
}

#[test]
fn test_loc_empty_directory_allowed_with_flag() {
    let tmp = tempfile::tempdir().unwrap();
    cwtools()
        .args(["loc", tmp.path().to_str().unwrap(), "--allow-empty"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 entries"));
}

#[test]
fn test_loc_default_report_type_matches_explicit_cli() {
    // The default (no --report-type) must render exactly like --report-type
    // cli, byte for byte — report/hash parity must not touch the default path.
    let loc_dir = fixtures_dir().join("loc_invalid");
    let default_out = cwtools()
        .args(["loc", loc_dir.to_str().unwrap()])
        .output()
        .unwrap();
    let explicit_out = cwtools()
        .args(["loc", loc_dir.to_str().unwrap(), "--report-type", "cli"])
        .output()
        .unwrap();
    assert_eq!(default_out.stdout, explicit_out.stdout);
    assert_eq!(default_out.status.code(), explicit_out.status.code());
}

#[test]
fn test_loc_json_report() {
    let loc_dir = fixtures_dir().join("loc_invalid");
    cwtools()
        .args(["loc", loc_dir.to_str().unwrap(), "--report-type", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"code\":\"CW268\""))
        .stdout(predicate::str::contains("\"hash\":"));
}

#[test]
fn test_loc_csv_report() {
    let loc_dir = fixtures_dir().join("loc_invalid");
    cwtools()
        .args(["loc", loc_dir.to_str().unwrap(), "--report-type", "csv"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "file,line,severity,code,message,hash",
        ))
        .stdout(predicate::str::contains("CW268"));
}

#[test]
fn test_loc_output_file() {
    let tmp = tempfile::tempdir().unwrap();
    let report = tmp.path().join("report.txt");
    let loc_dir = fixtures_dir().join("loc_invalid");
    cwtools()
        .args([
            "loc",
            loc_dir.to_str().unwrap(),
            "--output-file",
            report.to_str().unwrap(),
        ])
        .assert()
        .success();
    let contents = std::fs::read_to_string(&report).unwrap();
    assert!(contents.contains("CW268"));
}

#[test]
fn test_loc_hash_write_and_ignore_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let hashes = tmp.path().join("hashes.txt");
    let loc_dir = fixtures_dir().join("loc_invalid");

    // First run: exits 0 (CW268 is Warning-severity) and writes the baseline.
    cwtools()
        .args([
            "loc",
            loc_dir.to_str().unwrap(),
            "--output-hashes",
            hashes.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW268"));
    let baseline = std::fs::read_to_string(&hashes).unwrap();
    assert_eq!(baseline.lines().count(), 1, "one surviving diagnostic hash");

    // Second run with that baseline as --ignore-hashes: the diagnostic is
    // suppressed from the report entirely.
    cwtools()
        .args([
            "loc",
            loc_dir.to_str().unwrap(),
            "--ignore-hashes",
            hashes.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW268").not())
        .stdout(predicate::str::contains("0 issues"));
}

#[test]
fn test_loc_missing_ignore_hashes_is_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("missing-hashes.txt");
    let loc_dir = fixtures_dir().join("loc_invalid");

    cwtools()
        .args([
            "loc",
            loc_dir.to_str().unwrap(),
            "--ignore-hashes",
            missing.to_str().unwrap(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("could not read --ignore-hashes"))
        .stderr(predicate::str::contains(missing.to_str().unwrap()));
}

#[test]
fn test_loc_output_hashes_write_failure_exits_two() {
    let tmp = tempfile::tempdir().unwrap();
    let loc_dir = fixtures_dir().join("loc_invalid");

    cwtools()
        .args([
            "loc",
            loc_dir.to_str().unwrap(),
            "--output-hashes",
            tmp.path().to_str().unwrap(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Error writing hashes"))
        .stderr(predicate::str::contains(tmp.path().to_str().unwrap()));
}

#[test]
fn test_loc_ignore_hashes_filters_error_before_exit_code() {
    // CW225 (undefined loc reference) is Error-severity and normally fails
    // the run. Baselining its hash must suppress it from BOTH the report and
    // the exit-code count, same placement as `validate`.
    let tmp = tempfile::tempdir().unwrap();
    let hashes = tmp.path().join("hashes.txt");
    let loc_dir = fixtures_dir().join("loc_error");

    cwtools()
        .args([
            "loc",
            loc_dir.to_str().unwrap(),
            "--output-hashes",
            hashes.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("CW225"));

    cwtools()
        .args([
            "loc",
            loc_dir.to_str().unwrap(),
            "--ignore-hashes",
            hashes.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW225").not());
}

#[test]
fn test_loc_missing_directory_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no_such_loc");
    cwtools()
        .args(["loc", missing.to_str().unwrap()])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains(missing.to_str().unwrap()));
}

// ── Loc scope/command checks (--game + --rules) ──────────────────────────────
//
// The `loc_commands` fixture holds one unknown Jomini call and one chain that
// leaves a scope its next link doesn't accept. Without a ruleset there is no
// scope registry, so neither can be judged and neither is reported.

fn loc_commands_dir() -> PathBuf {
    fixtures_dir().join("loc_commands")
}

fn loc_rules_dir() -> PathBuf {
    fixtures_dir().join("loc_rules")
}

#[test]
fn test_loc_without_rules_runs_no_command_checks() {
    cwtools()
        .args(["loc", loc_commands_dir().to_str().unwrap()])
        .assert()
        .failure()
        .stdout(predicate::str::contains("CW226").not())
        .stdout(predicate::str::contains("CW260").not());
}

#[test]
fn test_loc_parenthesised_expression_does_not_report_an_empty_command() {
    let tmp = write_tree(&[(
        "localisation/cmds_l_english.yml",
        &fixture_text("loc_parenthesised/cmds_l_english.txt"),
    )]);

    cwtools()
        .args([
            "loc",
            tmp.path().to_str().unwrap(),
            "--game",
            "hoi4",
            "--rules",
            loc_rules_dir().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Loc validation complete: 1 entries, 0 issues",
        ))
        .stdout(predicate::str::contains("CW226").not());
}

/// `loc` reads the ruleset and the `.yml` files, never the game files, so it has
/// no scripted-localisation registry and an unknown command tail could as easily
/// be a `defined_text` as a typo — it stays lenient, the way a `?`-marked
/// variable read already does (#348). What it can still judge is a scope link
/// used from a scope that does not accept it.
#[test]
fn test_loc_with_rules_reports_the_command_checks() {
    cwtools()
        .args([
            "loc",
            loc_commands_dir().to_str().unwrap(),
            "--game",
            "hoi4",
            "--rules",
            loc_rules_dir().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("CW226").not())
        .stdout(predicate::str::contains("CW260"))
        .stdout(predicate::str::contains(
            "controller used in wrong scope. In country but expected state",
        ))
        .stderr(predicate::str::contains(
            "Loaded 2 scopes, 2 links and 2 loc commands",
        ));
}

/// Half the pair checks nothing, and says so rather than looking like it did:
/// a `cwtools.toml` written for `validate` routinely sets one and not the other.
#[test]
fn test_loc_game_without_rules_warns_and_checks_nothing() {
    cwtools()
        .args([
            "loc",
            loc_commands_dir().to_str().unwrap(),
            "--game",
            "hoi4",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("CW226").not())
        .stderr(predicate::str::contains(
            "need both --game and --rules; --game on its own does nothing",
        ));
}

#[test]
fn test_loc_reads_game_and_rules_from_the_config() {
    let config = format!(
        "game = \"hoi4\"\ndirectory = \"mod\"\nrules = {:?}\n",
        loc_rules_dir().to_str().unwrap()
    );
    let tmp = write_tree(&[
        (
            "mod/localisation/cmds_l_english.yml",
            &fixture_text("loc_commands/localisation/cmds_l_english.yml"),
        ),
        ("cwtools.toml", &config),
    ]);
    cwtools()
        .arg("loc")
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stdout(predicate::str::contains("CW260"))
        .stderr(predicate::str::contains("applied: game, directory, rules"));
}

#[test]
fn test_loc_ignore_file_skips_the_file() {
    cwtools()
        .args([
            "loc",
            loc_commands_dir().to_str().unwrap(),
            "--ignore-file",
            "cmds*",
            "--allow-empty",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 entries"));
}

#[test]
fn test_loc_ignore_dir_skips_the_tree() {
    cwtools()
        .args([
            "loc",
            loc_commands_dir().to_str().unwrap(),
            "--ignore-dir",
            "localisation",
            "--allow-empty",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 entries"));
}

#[test]
fn test_loc_language_scopes_the_scan() {
    // The fixture is english-only, so scoping to french loads nothing.
    cwtools()
        .args([
            "loc",
            loc_commands_dir().to_str().unwrap(),
            "--loc-language",
            "french",
            "--allow-empty",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 entries"));
}

#[test]
fn test_loc_language_unknown_fails() {
    cwtools()
        .args([
            "loc",
            loc_commands_dir().to_str().unwrap(),
            "--loc-language",
            "klingon",
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("invalid language 'klingon'"));
}
