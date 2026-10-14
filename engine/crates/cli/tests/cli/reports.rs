use crate::common::{cwtools, fix_mod, fixtures_dir, validate_dir};
use predicates::prelude::*;

// ── Report formats: github / sarif ───────────────────────────────────────────

#[test]
fn test_validate_github_report() {
    let mod_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    let out = cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            mod_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--report-type",
            "github",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let mut lines = stdout.lines();
    // No --vanilla, so the run leads with the notice naming the families it
    // could not check. It carries no file, so it is a bare `::notice::`.
    let notice = lines.next().unwrap_or_default();
    assert!(notice.starts_with("::notice::"), "{stdout:?}");
    assert!(notice.contains("CW500"), "{stdout:?}");
    // CW107 is Information-severity, which GitHub renders as a notice.
    let row = lines.next().unwrap_or_default();
    assert!(row.starts_with("::notice file="), "{stdout:?}");
    assert!(row.contains(",line=3,col="), "{stdout:?}");
    assert!(row.contains(",title=CW107::"), "{stdout:?}");
    // One workflow command per diagnostic, on one physical line each.
    assert_eq!(stdout.lines().count(), 2, "{stdout:?}");
}

/// GitHub resolves annotation paths against the checkout root, which a step's
/// `working-directory:` does not move.
#[test]
fn test_github_report_paths_are_relative_to_the_workspace() {
    let mod_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    let out = cwtools()
        .env("GITHUB_WORKSPACE", mod_dir.to_str().unwrap())
        .current_dir(std::env::temp_dir())
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            mod_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--report-type",
            "github",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("file=events/test.txt,"), "{stdout:?}");
}

#[test]
fn test_validate_sarif_report_is_well_formed() {
    let mod_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    let out = cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            mod_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--report-type",
            "sarif",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("\"version\": \"2.1.0\""), "{stdout:?}");
    assert!(stdout.contains("sarif-schema-2.1.0.json"), "{stdout:?}");
    assert!(stdout.contains("\"name\": \"cwtools\""), "{stdout:?}");
    // The rule definition comes from the shared error-code catalog.
    assert!(stdout.contains("\"id\": \"CW107\""), "{stdout:?}");
    assert!(
        stdout.contains("\"name\": \"EventEveryTick\""),
        "{stdout:?}"
    );
    assert!(stdout.contains("\"defaultConfiguration\""), "{stdout:?}");
    assert!(stdout.contains("\"ruleIndex\": 0"), "{stdout:?}");
    assert!(stdout.contains("\"level\": \"note\""), "{stdout:?}");
    assert!(stdout.contains("\"uriBaseId\": \"SRCROOT\""), "{stdout:?}");
    assert!(stdout.contains("\"partialFingerprints\""), "{stdout:?}");
    assert_balanced_json(&stdout);
}

/// The end position reaches the report from a real run, not just from a
/// hand-built `Diag`: CW107 underlines the event key, so the region is a span.
#[test]
fn test_validate_sarif_regions_carry_the_end_position() {
    let mod_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    let out = cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            mod_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--report-type",
            "sarif",
        ])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let region = &v["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"];
    assert_eq!(region["startLine"], 3);
    assert_eq!(region["startColumn"], 1);
    assert_eq!(region["endLine"], 3);
    assert_eq!(region["endColumn"], 14, "the `country_event` key");
}

/// Cheap structural check that the hand-built JSON closes everything it opens
/// and quotes are balanced outside strings.
fn assert_balanced_json(text: &str) {
    let (mut depth, mut in_str, mut escaped) = (0i32, false, false);
    for c in text.chars() {
        match c {
            _ if escaped => escaped = false,
            '\\' if in_str => escaped = true,
            '"' => in_str = !in_str,
            '{' | '[' if !in_str => depth += 1,
            '}' | ']' if !in_str => depth -= 1,
            _ => {}
        }
        assert!(depth >= 0, "unbalanced json: {text}");
    }
    assert_eq!(depth, 0, "unbalanced json: {text}");
    assert!(!in_str, "unterminated string: {text}");
}

#[test]
fn test_loc_github_and_sarif_reports() {
    let loc_dir = fixtures_dir().join("loc_invalid");
    cwtools()
        .args(["loc", loc_dir.to_str().unwrap(), "--report-type", "github"])
        .assert()
        .success()
        .stdout(predicate::str::contains("::warning file="))
        .stdout(predicate::str::contains("title=CW268::"));

    let out = cwtools()
        .args(["loc", loc_dir.to_str().unwrap(), "--report-type", "sarif"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("\"ruleId\": \"CW268\""), "{stdout:?}");
    // A redirected machine report must be the whole of stdout: the "Scanning
    // localisation in …" banner moves to stderr so `> out.sarif` stays valid.
    assert!(stdout.starts_with("{\n"), "{stdout:?}");
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("Scanning localisation")
    );
    assert_balanced_json(&stdout);
}

/// Only the two new formats divert status lines. The report types that existed
/// before keep every line exactly where it was, including the ones whose stdout
/// was already a mix of banner and report.
#[test]
fn test_only_the_new_formats_divert_status_lines() {
    let loc_dir = fixtures_dir().join("loc_invalid");
    for fmt in ["cli", "csv", "json"] {
        cwtools()
            .args(["loc", loc_dir.to_str().unwrap(), "--report-type", fmt])
            .assert()
            .success()
            .stdout(predicate::str::starts_with("Scanning localisation in "));
    }
    // A machine report written to a file doesn't own stdout either.
    let tmp = tempfile::tempdir().unwrap();
    cwtools()
        .args([
            "loc",
            loc_dir.to_str().unwrap(),
            "--report-type",
            "sarif",
            "--output-file",
            tmp.path().join("r.sarif").to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("Scanning localisation in "));
}

#[test]
fn test_unknown_report_type_fails() {
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
            "sarrif",
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("invalid report type 'sarrif'"))
        .stderr(predicate::str::contains("cli, csv, json, github, sarif"));
}

#[test]
fn test_validate_json_report() {
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
            "--report-type",
            "json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("["));
}

#[test]
fn test_validate_csv_report() {
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
            "--report-type",
            "csv",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("file,line,severity"));
}

#[test]
fn test_validate_output_file() {
    let tmp = tempfile::tempdir().unwrap();
    let report = tmp.path().join("report.txt");
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
            "--output-file",
            report.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(report.exists());
}

#[test]
fn test_validate_missing_ignore_hashes_is_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("missing-hashes.txt");
    let mod_dir = fixtures_dir().join("discover").join("mod_a");

    validate_dir(&mod_dir, &["--ignore-hashes", missing.to_str().unwrap()])
        .code(2)
        .stderr(predicate::str::contains("could not read --ignore-hashes"))
        .stderr(predicate::str::contains(missing.to_str().unwrap()));
}

#[test]
fn test_validate_output_hashes_write_failure_exits_two() {
    let tmp = tempfile::tempdir().unwrap();
    let mod_dir = fixtures_dir().join("discover").join("mod_a");

    validate_dir(&mod_dir, &["--output-hashes", tmp.path().to_str().unwrap()])
        .code(2)
        .stderr(predicate::str::contains("Error writing hashes"))
        .stderr(predicate::str::contains(tmp.path().to_str().unwrap()));
}

#[test]
fn test_validate_hash_baseline_survives_inserted_line() {
    // The digest is content-derived, so inserting a line above a baselined
    // diagnostic must not resurface it as new.
    let tmp = fix_mod();
    let rules_dir = fixtures_dir().join("rules");
    let file = tmp.path().join("common").join("hint.txt");
    let hashes = tmp.path().join("hashes.txt");
    let validate = |extra: [&str; 2]| {
        let mut cmd = cwtools();
        cmd.args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            tmp.path().to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
        ])
        .args(extra);
        cmd
    };

    validate(["--output-hashes", hashes.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW121"));

    // Push the diagnostic down a line; its text is untouched.
    std::fs::write(&file, "# a new comment\nx = { if = { } }\n").unwrap();

    validate(["--ignore-hashes", hashes.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW121").not());
}

#[test]
fn test_raw_equipment_icons_reach_cli_sarif_and_suppression() {
    let tmp = tempfile::tempdir().unwrap();
    let rules = tmp.path().join("rules");
    let mod_dir = tmp.path().join("mod");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(
        rules.join("r.cwt"),
        "types = { type[variant] = { path = \"game/history/countries\" } }",
    )
    .unwrap();
    let variant = mod_dir.join("history/countries/USA.txt");
    let pool = mod_dir.join("gfx/interface/equipmentdesigner/graphic_db/planes.txt");
    for file in [&variant, &pool] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    }
    let variant_source = "create_equipment_variant = {\n icon = \"gfx/plane.dds\"\n}\n";
    let pool_source = "USA = { plane = { pool = { icons = {\n \"gfx/plane.dds\"\n} } } }\n";
    std::fs::write(&variant, variant_source).unwrap();
    std::fs::write(&pool, pool_source).unwrap();
    let command = |report: &str| {
        let mut cmd = cwtools();
        cmd.args(["validate", "--game", "hoi4", "--directory"])
            .arg(&mod_dir)
            .arg("--rules")
            .arg(&rules)
            .args(["--only-code", "CW284", "--report-type", report]);
        cmd
    };
    command("cli")
        .assert()
        .success()
        .stdout(predicate::str::contains("CW284"))
        .stdout(predicate::str::contains("macOS and Linux"))
        .stdout(predicate::str::contains("2 warnings"));
    let out = command("sarif").output().unwrap();
    assert!(out.status.success());
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let results = report["runs"][0]["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    for result in results {
        assert_eq!(result["ruleId"], "CW284");
        assert_eq!(result["level"], "warning");
        assert_eq!(
            result["locations"][0]["physicalLocation"]["region"]["startLine"],
            2
        );
    }
    assert!(
        report["runs"][0]["tool"]["driver"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|rule| rule["id"] == "CW284"
                && rule["helpUri"].as_str().unwrap().ends_with("#cw284"))
    );
    command("cli")
        .args(["--ignore-code", "cw284"])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW284").not());
    std::fs::write(
        &variant,
        variant_source.replace("gfx/plane.dds\"", "gfx/plane.dds\" # cwtools-ignore CW284"),
    )
    .unwrap();
    std::fs::write(
        &pool,
        pool_source.replace("gfx/plane.dds\"", "gfx/plane.dds\" # cwtools-ignore CW284"),
    )
    .unwrap();
    command("cli")
        .assert()
        .success()
        .stdout(predicate::str::contains("CW284").not());
}
