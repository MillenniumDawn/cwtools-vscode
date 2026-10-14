use crate::common::{config_body, config_mod, cwtools, fixtures_dir};
use predicates::prelude::*;

// ── Per-code suppression ─────────────────────────────────────────────────────

#[test]
fn test_validate_ignore_code_drops_the_code() {
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
            // Lowercase: matched case-insensitively, like the editor setting.
            "--ignore-code",
            "cw107",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW107").not())
        .stdout(predicate::str::contains(
            "Validation complete: 0 errors, 0 warnings",
        ));
}

/// Write `content` over the mod fixture `config_mod` copies in, and validate
/// it with the same config. Shared by the inline-directive tests below.
fn validate_inline_mod(content: &str) -> assert_cmd::assert::Assert {
    let tmp = config_mod(&config_body());
    let test_txt = tmp.path().join("mod").join("events").join("test.txt");
    std::fs::write(&test_txt, content).unwrap();
    cwtools()
        .args(["validate", "--config"])
        .arg(tmp.path().join("cwtools.toml"))
        .assert()
}

/// The fixture's only diagnostic is the CW107 on the `country_event` line
/// (line 3): a directive there must drop it from the report, like
/// `--ignore-code` but scoped to the file.
#[test]
fn test_validate_inline_ignore_same_line_suppresses_the_code() {
    let content = "namespace = test_events\n\ncountry_event = { # cwtools-ignore CW107\n\tid = test.1\n\ttitle = \"Test Event\"\n\tdesc = \"A test event\"\n}\n";
    validate_inline_mod(content)
        .success()
        .stdout(predicate::str::contains("CW107").not())
        .stdout(predicate::str::contains(
            "Validation complete: 0 errors, 0 warnings",
        ));
}

/// The standalone form, on the line right after the offending one.
#[test]
fn test_validate_inline_ignore_next_line_suppresses_the_code() {
    let content = "namespace = test_events\n\ncountry_event = {\n# cwtools-ignore CW107\n\tid = test.1\n\ttitle = \"Test Event\"\n\tdesc = \"A test event\"\n}\n";
    validate_inline_mod(content)
        .success()
        .stdout(predicate::str::contains("CW107").not());
}

/// A directive naming a different code leaves the diagnostic alone.
#[test]
fn test_validate_inline_ignore_other_code_leaves_the_diagnostic() {
    let content = "namespace = test_events\n\ncountry_event = { # cwtools-ignore CW100\n\tid = test.1\n\ttitle = \"Test Event\"\n\tdesc = \"A test event\"\n}\n";
    validate_inline_mod(content)
        .success()
        .stdout(predicate::str::contains("CW107"));
}

#[test]
fn test_validate_only_code_keeps_just_that_code() {
    let mod_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    let rules = rules_dir.to_str().unwrap().to_string();
    let dir = mod_dir.to_str().unwrap().to_string();
    let base = ["validate", "--game", "stellaris", "--directory"];
    cwtools()
        .args(base)
        .arg(&dir)
        .args(["--rules", &rules, "--only-code", "CW107"])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW107"));
    cwtools()
        .args(base)
        .arg(&dir)
        .args(["--rules", &rules, "--only-code", "CW121"])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW107").not());
}

#[test]
fn test_ignore_code_beats_only_code() {
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
            "--only-code",
            "CW107",
            "--ignore-code",
            "CW107",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW107").not());
}

#[test]
fn test_unknown_ignore_code_fails() {
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
            "--ignore-code",
            "CW9999",
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("unknown diagnostic code 'CW9999'"));
}

#[test]
fn test_loc_ignore_code_changes_the_exit_code() {
    // CW225 is the only diagnostic in the fixture and it's Error-severity, so
    // suppressing it turns exit 1 into exit 0.
    let loc_dir = fixtures_dir().join("loc_error");
    cwtools()
        .args(["loc", loc_dir.to_str().unwrap()])
        .assert()
        .failure()
        .code(1);
    cwtools()
        .args(["loc", loc_dir.to_str().unwrap(), "--ignore-code", "CW225"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 issues"));
}

#[test]
fn test_loc_min_severity_filters() {
    let loc_dir = fixtures_dir().join("loc_invalid");
    cwtools()
        .args(["loc", loc_dir.to_str().unwrap(), "--min-severity", "error"])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW268").not());
}

#[test]
fn test_validate_inline_ignore_quoted_example_keeps_the_diagnostic() {
    let content = "namespace = test_events\n\ncountry_event = {\n\tid = test.1\n\ttitle = \"Test Event\"\n\tdesc = \"A test event\"\n}\n";
    let content = content.replace(
        "country_event = {",
        "country_event = { title = \"literal # cwtools-ignore CW107 example\"",
    );
    validate_inline_mod(&content)
        .success()
        .stdout(predicate::str::contains("CW107"));
}

#[test]
fn test_loc_inline_ignore_quoted_example_keeps_the_diagnostic() {
    let tmp = tempfile::tempdir().unwrap();
    let loc = tmp.path().join("localisation");
    std::fs::create_dir_all(&loc).unwrap();
    let file = loc.join("test_l_english.yml");
    std::fs::write(
        &file,
        "\u{FEFF}l_english:\n KEY:0 \"$missing_key$ \"quoted # cwtools-ignore CW225 example\" rest\"\n",
    )
    .unwrap();
    cwtools()
        .arg("loc")
        .arg(tmp.path())
        .assert()
        .failure()
        .stdout(predicate::str::contains("CW225"));
}

#[test]
fn test_loc_inline_ignore_real_comment_after_embedded_quotes_suppresses() {
    let tmp = tempfile::tempdir().unwrap();
    let loc = tmp.path().join("localisation");
    std::fs::create_dir_all(&loc).unwrap();
    std::fs::write(
        loc.join("test_l_english.yml"),
        "\u{FEFF}l_english:\n KEY:0 \"$missing_key$ \"quoted\" rest\" # cwtools-ignore CW225\n",
    )
    .unwrap();
    cwtools()
        .arg("loc")
        .arg(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("CW225").not());
}
