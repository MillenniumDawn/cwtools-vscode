//! CLI comparison for the exact rules and fixture shared with the standalone LSP test.
use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

const RULES: &str = include_str!("../../../tests/standalone_fixture/Config/fixture.cwt");
const INVALID: &str = include_str!("../../../tests/standalone_fixture/invalid.txt");
const CONTROL: &str = include_str!("../../../tests/standalone_fixture/control.txt");
const EXPECTED: &str = include_str!("../../../tests/standalone_fixture/expected.json");

fn run_fixture() -> Value {
    let workspace = TempDir::new().unwrap();
    let rules = TempDir::new().unwrap();
    std::fs::create_dir_all(rules.path()).unwrap();
    std::fs::create_dir_all(workspace.path().join("common/foo")).unwrap();
    std::fs::write(rules.path().join("fixture.cwt"), RULES).unwrap();
    std::fs::write(workspace.path().join("common/foo/example.txt"), INVALID).unwrap();
    std::fs::write(workspace.path().join("common/foo/control.txt"), CONTROL).unwrap();

    let output = Command::cargo_bin("cwtools")
        .unwrap()
        .args([
            "validate",
            "--game",
            "hoi4",
            "--directory",
            workspace.path().to_str().unwrap(),
            "--rules",
            rules.path().to_str().unwrap(),
            "--report-type",
            "json",
            "--loc-language",
            "english",
            "--fail-on",
            "none",
            "--no-vanilla-cache",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "CLI did not emit JSON: {error}: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    let mut normalized = records
        .into_iter()
        .map(|record| {
            let path = record["file"].as_str().unwrap().replace('\\', "/");
            let file = ["common/foo/control.txt", "common/foo/example.txt"]
                .into_iter()
                .find(|suffix| path.ends_with(suffix))
                .unwrap_or_else(|| panic!("unexpected CLI file path: {path}"));
            serde_json::json!({
                "file": file,
                "line": record["line"],
                "code": record["code"],
                "severity": record["severity"],
                "message": record["message"],
            })
        })
        .collect::<Vec<_>>();
    normalized.sort_by_key(record_sort_key);
    serde_json::Value::Array(normalized)
}

fn record_sort_key(record: &Value) -> (String, u64, String, String, String) {
    (
        record["file"].as_str().unwrap().to_string(),
        record["line"].as_u64().unwrap(),
        record["code"].as_str().unwrap().to_string(),
        record["severity"].as_str().unwrap().to_string(),
        record["message"].as_str().unwrap().to_string(),
    )
}

#[test]
fn cli_uses_same_fixture_rules_scope_and_diagnostic_records() {
    let actual = run_fixture();
    let mut expected: Vec<Value> = serde_json::from_str(EXPECTED).unwrap();
    expected.sort_by_key(record_sort_key);
    assert_eq!(actual, Value::Array(expected));
}
