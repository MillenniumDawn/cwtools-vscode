use crate::common::{cwtools, fix_mod, fixtures_dir};
use predicates::prelude::*;

// ── Fix ──────────────────────────────────────────────────────────────────────

#[test]
fn test_fix_dry_run_previews_without_writing() {
    let tmp = fix_mod();
    let rules_dir = fixtures_dir().join("rules");
    let file = tmp.path().join("common").join("hint.txt");
    cwtools()
        .args([
            "fix",
            "--game",
            "stellaris",
            "--directory",
            tmp.path().to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Dry run"))
        .stdout(predicate::str::contains("@@"));
    // Dry run must not touch the file.
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "x = { if = { } }\n"
    );
}

#[test]
fn test_fix_apply_writes_and_is_idempotent() {
    let tmp = fix_mod();
    let rules_dir = fixtures_dir().join("rules");
    let file = tmp.path().join("common").join("hint.txt");
    cwtools()
        .args([
            "fix",
            "--game",
            "stellaris",
            "--directory",
            tmp.path().to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--apply",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Applied"));
    // The empty if is gone.
    let after = std::fs::read_to_string(&file).unwrap();
    assert_eq!(after, "x = { }\n", "empty if should be removed");
    // A second run finds nothing left to fix.
    cwtools()
        .args([
            "fix",
            "--game",
            "stellaris",
            "--directory",
            tmp.path().to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 fix(es)"));
}

#[test]
fn test_fix_apply_preserves_a_leading_bom_and_surrounding_text() {
    let tmp = fix_mod();
    let rules_dir = fixtures_dir().join("rules");
    let file = tmp.path().join("common").join("hint.txt");
    std::fs::write(&file, "\u{FEFF}x = { if = { } }\n").unwrap();

    cwtools()
        .args([
            "fix",
            "--game",
            "stellaris",
            "--directory",
            tmp.path().to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--apply",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Applied 1 fix(es) across 1 file(s)",
        ));

    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "\u{FEFF}x = { }\n",
        "fixing line 1 must retain the BOM and surrounding text"
    );
}

#[test]
fn test_fix_apply_read_failure_exits_two() {
    let tmp = fix_mod();
    let rules_dir = fixtures_dir().join("rules");
    let file = tmp.path().join("common").join("hint.txt");
    std::fs::write(&file, b"x = { if = { } } # \xff\n").unwrap();

    cwtools()
        .args([
            "fix",
            "--game",
            "stellaris",
            "--directory",
            tmp.path().to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--apply",
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("could not read"));
    assert_eq!(std::fs::read(&file).unwrap(), b"x = { if = { } } # \xff\n");
}

#[test]
fn test_fix_apply_write_failure_preserves_file_and_counts_only_successes() {
    let tmp = fix_mod();
    let rules_dir = fixtures_dir().join("rules");
    let common = tmp.path().join("common");
    // The target fits a filename component; its appended temporary suffix does not.
    let filename = format!("{}.txt", "a".repeat(251));
    let file = common.join(&filename);
    let original = b"x = { if = { } }\n";
    std::fs::write(&file, original).unwrap();
    std::fs::OpenOptions::new().write(true).open(&file).unwrap();

    cwtools()
        .args([
            "fix",
            "--game",
            "stellaris",
            "--directory",
            tmp.path().to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--apply",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Error writing"))
        .stderr(predicate::str::contains(&filename))
        .stdout(predicate::str::contains(
            "Applied 1 fix(es) across 1 file(s)",
        ));

    assert_eq!(std::fs::read(&file).unwrap(), original);
    assert_eq!(
        std::fs::read_to_string(common.join("hint.txt")).unwrap(),
        "x = { }\n"
    );
    assert_eq!(std::fs::read_dir(common).unwrap().count(), 2);
}

#[test]
fn test_fix_code_filter_excludes_unmatched() {
    let tmp = fix_mod();
    let rules_dir = fixtures_dir().join("rules");
    // Filtering to a code that isn't present leaves nothing to fix.
    cwtools()
        .args([
            "fix",
            "--game",
            "stellaris",
            "--directory",
            tmp.path().to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--code",
            "CW999",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 fix(es)"));
}

#[test]
fn test_fix_empty_rules_dir_fails() {
    let empty_rules = tempfile::tempdir().unwrap();
    let tmp = fix_mod();
    cwtools()
        .args([
            "fix",
            "--game",
            "stellaris",
            "--directory",
            tmp.path().to_str().unwrap(),
            "--rules",
            empty_rules.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("--rules loaded 0 types"));
}

#[test]
fn test_fix_missing_directory_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no_such_mod");
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args([
            "fix",
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

// ── Format ───────────────────────────────────────────────────────────────────

fn format_mod_dir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let common = tmp.path().join("common");
    std::fs::create_dir_all(&common).unwrap();
    std::fs::write(common.join("a.txt"), "root={\n a=1\n}\n").unwrap();
    tmp
}

#[test]
fn test_format_dry_run_exits_nonzero_when_files_would_change() {
    let tmp = format_mod_dir();
    cwtools()
        .args(["format", "--directory", tmp.path().to_str().unwrap()])
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("would be formatted"));
}

#[test]
fn test_format_apply_rewrites_and_second_dry_run_is_clean() {
    let tmp = format_mod_dir();
    let path = tmp.path().join("common").join("a.txt");
    cwtools()
        .args([
            "format",
            "--directory",
            tmp.path().to_str().unwrap(),
            "--apply",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Formatted 1 file"));
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text, "root = {\n    a = 1\n}\n");
    cwtools()
        .args(["format", "--directory", tmp.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 file(s) would be formatted"));
}

#[test]
fn test_format_max_line_width_is_passed_to_formatter() {
    let tmp = format_mod_dir();
    let path = tmp.path().join("common").join("a.txt");
    std::fs::write(path, "root={\n    values={1 22 333}\n}\n").unwrap();
    cwtools()
        .args([
            "format",
            "--directory",
            tmp.path().to_str().unwrap(),
            "--max-line-width",
            "20",
            "--apply",
        ])
        .assert()
        .success();
    let text = std::fs::read_to_string(tmp.path().join("common").join("a.txt")).unwrap();
    assert!(text.contains("values = {\n"), "{text}");
}

#[test]
fn test_format_skips_a_parse_error() {
    let tmp = tempfile::tempdir().unwrap();
    let common = tmp.path().join("common");
    std::fs::create_dir_all(&common).unwrap();
    std::fs::write(common.join("bad.txt"), "root = {\n").unwrap();
    cwtools()
        .args(["format", "--directory", tmp.path().to_str().unwrap()])
        .assert()
        .success()
        .stderr(predicate::str::contains("failed to parse"));
}

#[test]
fn test_format_names_the_unclosed_quote_it_will_not_format() {
    let tmp = tempfile::tempdir().unwrap();
    let common = tmp.path().join("common");
    std::fs::create_dir_all(&common).unwrap();
    let file = common.join("names.txt");
    let original = "names = {\n\t\"Falke\n\tAdler\n}\n";
    std::fs::write(&file, original).unwrap();
    cwtools()
        .args([
            "format",
            "--directory",
            tmp.path().to_str().unwrap(),
            "--apply",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "names.txt: unclosed quote on line 2; skipping",
        ))
        .stderr(predicate::str::contains(
            "skipped 1 file(s) (unreadable, parse errors or unclosed quotes)",
        ))
        .stderr(predicate::str::contains("failed to parse").not());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
}

#[test]
fn test_format_apply_read_failure_exits_two() {
    let tmp = tempfile::tempdir().unwrap();
    let common = tmp.path().join("common");
    std::fs::create_dir_all(&common).unwrap();
    let file = common.join("bad.txt");
    let original = b"root={\n \xff\n}\n";
    std::fs::write(&file, original).unwrap();

    cwtools()
        .args([
            "format",
            "--directory",
            tmp.path().to_str().unwrap(),
            "--apply",
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("could not read"));
    assert_eq!(std::fs::read(&file).unwrap(), original);
}

// Windows MAX_PATH rejects this fixture during setup before the temp-name failure.
#[cfg(unix)]
#[test]
fn test_format_apply_write_failure_preserves_file() {
    let tmp = tempfile::tempdir().unwrap();
    let common = tmp.path().join("common");
    std::fs::create_dir_all(&common).unwrap();
    // The target fits a filename component; its appended temporary suffix does not.
    let filename = format!("{}.txt", "a".repeat(251));
    let file = common.join(&filename);
    let original = b"root={\n a=1\n}\n";
    std::fs::write(&file, original).unwrap();

    cwtools()
        .args([
            "format",
            "--directory",
            tmp.path().to_str().unwrap(),
            "--apply",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Error writing"))
        .stderr(predicate::str::contains(&filename))
        .stdout(predicate::str::contains("Formatted 0 file(s)"));

    assert_eq!(std::fs::read(&file).unwrap(), original);
    assert_eq!(std::fs::read_dir(common).unwrap().count(), 1);
}

#[test]
fn test_format_rejects_an_unknown_indent_style() {
    let tmp = format_mod_dir();
    cwtools()
        .args([
            "format",
            "--directory",
            tmp.path().to_str().unwrap(),
            "--indent-style",
            "banana",
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("indent-style"));
}
