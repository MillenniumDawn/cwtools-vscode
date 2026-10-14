use crate::common::{broken_rules_dir, cwtools, fixtures_dir};
use predicates::prelude::*;

// ── Parse ────────────────────────────────────────────────────────────────────

#[test]
fn test_parse_single_file() {
    let simple = fixtures_dir().join("simple.txt");
    cwtools()
        .args(["parse", simple.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Parsed"))
        .stdout(predicate::str::contains("Leaves"))
        .stdout(predicate::str::contains("Leaves"));
}

#[test]
fn test_parse_rules_directory() {
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args(["parse", rules_dir.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Parsed rule directory"))
        .stdout(predicate::str::contains("Types"))
        .stdout(predicate::str::contains("Enums"));
}

#[test]
fn test_parse_missing_file_fails() {
    cwtools()
        .args(["parse", "/nonexistent/path/file.txt"])
        .assert()
        .failure();
}

// ── Parse errors ─────────────────────────────────────────────────────────────

#[test]
fn test_parse_reports_errors_and_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("bad.txt");
    std::fs::write(&file, "a = { b = ").unwrap();
    cwtools()
        .args(["parse", file.to_str().unwrap()])
        .assert()
        .failure()
        .code(2)
        // The summary still goes to stdout unchanged.
        .stdout(predicate::str::contains("Parsed:"))
        .stderr(predicate::str::contains("parse error(s)"))
        .stderr(predicate::str::contains("has no value after '='"))
        .stderr(predicate::str::contains("unclosed clause"));
}

#[test]
fn test_parse_reports_the_clause_depth_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("deep.txt");
    let body = format!("root = {}1{}\n", "{ a = ".repeat(300), " }".repeat(300));
    std::fs::write(&file, body).unwrap();
    cwtools()
        .args(["parse", file.to_str().unwrap()])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("clause nesting deeper than"));
}

#[test]
fn test_parse_clean_file_reports_nothing_extra() {
    let simple = fixtures_dir().join("simple.txt");
    let out = cwtools()
        .args(["parse", simple.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(
        !String::from_utf8(out.stderr)
            .unwrap()
            .contains("parse error"),
        "a clean parse must stay quiet"
    );
}

// ── Discover ─────────────────────────────────────────────────────────────────

#[test]
fn test_discover_mod_directory() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    cwtools()
        .args(["discover", discover_dir.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Discovered and parsed"))
        .stdout(predicate::str::contains("2 files"));
}

#[test]
fn test_discover_empty_directory() {
    let tmp = tempfile::tempdir().unwrap();
    cwtools()
        .args(["discover", tmp.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Discovered and parsed 0 files"));
}

#[test]
fn test_discover_missing_directory_fails_with_discovery_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no_such_mod");
    cwtools()
        .args(["discover", missing.to_str().unwrap()])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains(missing.to_str().unwrap()));
}

#[test]
fn test_discover_multi_mod_workspace_uses_the_layered_file_set() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for (name, path) in [("a", "mods/a"), ("b", "mods/b")] {
        let descriptor = root.join("mod").join(format!("{name}.mod"));
        std::fs::create_dir_all(descriptor.parent().unwrap()).unwrap();
        std::fs::write(
            descriptor,
            format!("name = \"{name}\"\npath = \"{path}\"\n"),
        )
        .unwrap();
    }
    for (path, content) in [
        ("mods/a/common/shared.txt", "value = a\n"),
        ("mods/b/common/shared.txt", "value = b\n"),
        ("mods/b/events/only_b.txt", "value = b\n"),
    ] {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    let winner_path = r"mods/b[/\\]common[/\\]shared\.txt";

    cwtools()
        .args(["discover", root.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Discovered and parsed 2 files"))
        .stdout(predicate::str::is_match(winner_path).unwrap());
}

// ── Rules ────────────────────────────────────────────────────────────────────

#[test]
fn test_rules_single_file() {
    let rules_file = fixtures_dir().join("rules").join("test.cwt");
    cwtools()
        .args(["rules", rules_file.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Parsed rules file"))
        .stdout(predicate::str::contains("test_type"))
        .stdout(predicate::str::contains("test_enum"));
}

#[test]
fn test_rules_directory() {
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args(["rules", rules_dir.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Parsed"));
}

#[test]
fn test_rules_missing_file_fails() {
    cwtools()
        .args(["rules", "/nonexistent/rules.cwt"])
        .assert()
        .failure();
}

#[test]
fn test_rules_clean_directory_reports_no_problems() {
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args(["rules", rules_dir.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Rules check complete: 0 errors, 0 warnings",
        ));
}

#[test]
fn test_rules_reports_coded_problems_and_fails() {
    let rules = broken_rules_dir();
    cwtools()
        .args(["rules", rules.path().to_str().unwrap()])
        .assert()
        .failure()
        .stdout(predicate::str::contains("CW601"))
        .stdout(predicate::str::contains("CW603"))
        .stdout(predicate::str::contains(
            "Rules check complete: 1 errors, 1 warnings",
        ));
}

#[test]
fn test_rules_reports_a_syntax_error_and_fails() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("broken.cwt"), "types = {\n").unwrap();
    cwtools()
        .args(["rules", tmp.path().to_str().unwrap()])
        .assert()
        .failure()
        .stdout(predicate::str::contains("CW604"))
        .stdout(predicate::str::contains("unclosed clause"));
}

#[test]
fn test_single_rules_file_reports_syntax_and_directive_errors() {
    for (source, code) in [
        ("types = {\n", "CW604"),
        ("## cardinality = 0..x\nr = { a = bool }\n", "CW603"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("broken.cwt");
        std::fs::write(&file, source).unwrap();
        for input in [&file, &tmp.path().to_path_buf()] {
            cwtools()
                .args(["rules", input.to_str().unwrap(), "--fail-on", "warning"])
                .assert()
                .failure()
                .stdout(predicate::str::contains(code))
                .stdout(predicate::str::contains("broken.cwt"));
            cwtools()
                .args([
                    "validate",
                    "--game",
                    "stellaris",
                    "--directory",
                    fixtures_dir().join("discover/mod_a").to_str().unwrap(),
                    "--rules",
                    input.to_str().unwrap(),
                    "--fail-on",
                    "warning",
                    "--allow-empty",
                    "--report-type",
                    "csv",
                ])
                .assert()
                .failure()
                .stdout(predicate::str::contains(code))
                .stdout(predicate::str::contains("broken.cwt"));
        }
    }
}

/// A machine-readable report owns stdout: the ruleset summary moves to stderr
/// so `cwtools rules --report-type csv > out.csv` is a usable CSV.
#[test]
fn test_rules_csv_report_keeps_the_summary_off_stdout() {
    let rules = broken_rules_dir();
    cwtools()
        .args([
            "rules",
            rules.path().to_str().unwrap(),
            "--report-type",
            "csv",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::starts_with(
            "file,line,severity,code,message,hash\n",
        ))
        .stdout(predicate::str::contains("CW601"))
        .stdout(predicate::str::contains("Types:").not())
        .stderr(predicate::str::contains("Types:"));
}

#[test]
fn test_rules_malformed_alias_directive_reports_cw605() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("malformed.cwt");
    std::fs::write(
        &file,
        "# comment\nalias[effect] = { field = scalar }\nordinary_type = { field = scalar }\n",
    )
    .unwrap();
    cwtools()
        .arg("rules")
        .arg(&file)
        .assert()
        .failure()
        .stdout(predicate::str::contains("CW605"))
        .stdout(predicate::str::contains("alias[effect]"));
}

// ── Serialize / Deserialize ──────────────────────────────────────────────────

#[test]
fn test_serialize_and_deserialize_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let cwb = tmp.path().join("test.cwb");
    let simple = fixtures_dir().join("simple.txt");

    // Serialize
    cwtools()
        .args(["serialize", simple.to_str().unwrap(), cwb.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Serialized"));

    // Deserialize
    cwtools()
        .args(["deserialize", cwb.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Deserialized"))
        .stdout(predicate::str::contains("Leaves"));
}

#[test]
fn test_serialize_missing_input_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let cwb = tmp.path().join("out.cwb");
    cwtools()
        .args(["serialize", "/nonexistent/file.txt", cwb.to_str().unwrap()])
        .assert()
        .failure();
}
