//! End-to-end standalone stdio client contract using the bounded server harness.
#[path = "lsp/common/mod.rs"]
pub mod common;

use common::{TestServer, diagnostics_of};
use serde_json::{Value, json};
use tempfile::TempDir;

const RULES: &str = include_str!("../../../tests/standalone_fixture/Config/fixture.cwt");
const INVALID: &str = include_str!("../../../tests/standalone_fixture/invalid.txt");
const VALID: &str = include_str!("../../../tests/standalone_fixture/valid.txt");
const CONTROL: &str = include_str!("../../../tests/standalone_fixture/control.txt");
const EXPECTED: &str = include_str!("../../../tests/standalone_fixture/expected.json");

fn standalone_server() -> TestServer {
    TestServer::builder()
        .rules(RULES)
        .init_opts(json!({ "localisationLanguages": ["English"] }))
        .file("common/foo/example.txt", INVALID)
        .file("common/foo/control.txt", CONTROL)
        .spawn_scanned()
}

fn normalized_lsp_records(publishes: &[Value]) -> Vec<Value> {
    let mut records = Vec::new();
    for publish in publishes {
        let uri = publish["params"]["uri"].as_str().unwrap();
        let file = ["common/foo/control.txt", "common/foo/example.txt"]
            .into_iter()
            .find(|suffix| uri.ends_with(suffix))
            .unwrap_or_else(|| panic!("unexpected fixture URI: {uri}"));
        for item in diagnostics_of(publish) {
            let severity = match item["severity"].as_u64().unwrap() {
                1 => "Error",
                2 => "Warning",
                3 => "Information",
                4 => "Hint",
                other => panic!("unexpected LSP severity {other}"),
            };
            records.push(json!({
                "file": file,
                "line": item["range"]["start"]["line"].as_u64().unwrap() + 1,
                "code": item["code"],
                "severity": severity,
                "message": item["message"],
            }));
        }
    }
    records.sort_by_key(record_sort_key);
    records
}

fn record_sort_key(record: &Value) -> (String, u64, String, String, String) {
    (
        record["file"].as_str().unwrap().replace('\\', "/"),
        record["line"].as_u64().unwrap(),
        record["code"].as_str().unwrap().to_string(),
        record["severity"].as_str().unwrap().to_string(),
        record["message"].as_str().unwrap().to_string(),
    )
}

fn expected_records() -> Vec<Value> {
    let mut records: Vec<Value> = serde_json::from_str(EXPECTED).unwrap();
    records.sort_by_key(record_sort_key);
    records
}

fn diagnostic<'a>(diagnostics: &'a [Value], code: &str) -> &'a Value {
    diagnostics
        .iter()
        .find(|item| item["code"] == code)
        .unwrap_or_else(|| panic!("expected {code}; diagnostics were {diagnostics:?}"))
}

#[test]
fn standalone_hoi4_diagnostics_versioned_edit_and_completion() {
    let mut server = standalone_server();
    server.open("common/foo/example.txt", INVALID);
    let first = server.read_until("version-1 diagnostics", |frame| {
        frame["method"] == "textDocument/publishDiagnostics"
            && frame["params"]["uri"]
                .as_str()
                .is_some_and(|uri| uri.ends_with("common/foo/example.txt"))
            && frame["params"]["version"] == 1
    });
    let initial = diagnostics_of(&first);
    server.open("common/foo/control.txt", CONTROL);
    let control = server.read_until("control version-1 diagnostics", |frame| {
        frame["method"] == "textDocument/publishDiagnostics"
            && frame["params"]["uri"]
                .as_str()
                .is_some_and(|uri| uri.ends_with("common/foo/control.txt"))
            && frame["params"]["version"] == 1
    });
    assert_eq!(
        normalized_lsp_records(&[first.clone(), control.clone()]),
        expected_records(),
        "all initial LSP records must match the shared CLI/LSP expectation"
    );

    let invalid = diagnostic(initial, "CW263");
    assert_eq!(invalid["severity"], 1);
    assert_eq!(invalid["message"], "Unexpected field 'unexpected'");
    let start = INVALID.find("unexpected").unwrap() as u64;
    assert_eq!(invalid["range"]["start"]["line"], 0);
    assert_eq!(invalid["range"]["start"]["character"], start);
    assert_eq!(invalid["range"]["end"]["line"], 0);
    assert_eq!(
        invalid["range"]["end"]["character"],
        start + "unexpected = 1".len() as u64
    );

    let hint = diagnostic(initial, "CW282");
    assert_eq!(
        hint["severity"], 3,
        "Information diagnostics must be retained"
    );
    assert_eq!(
        hint["message"],
        "This is the default value (true) and can be omitted"
    );
    assert!(
        first["params"]["uri"]
            .as_str()
            .unwrap()
            .ends_with("common/foo/example.txt")
    );
    let hint_start = INVALID.find("redundant").unwrap() as u64;
    assert_eq!(hint["range"]["start"]["line"], 0);
    assert_eq!(hint["range"]["end"]["line"], 0);
    assert_eq!(hint["range"]["start"]["character"], hint_start);
    assert_eq!(
        hint["range"]["end"]["character"],
        hint_start + "redundant = yes".len() as u64
    );

    let uri = server.uri("common/foo/example.txt");
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{ "text": VALID }]
        }),
    );
    // Discard a queued version-1 notification: only a publish for version 2
    // is evidence that the corrected snapshot was validated.
    let corrected = server.read_until("version-2 diagnostics", |frame| {
        frame["method"] == "textDocument/publishDiagnostics"
            && frame["params"]["uri"] == uri
            && frame["params"]["version"] == 2
    });
    let after = diagnostics_of(&corrected);
    assert!(!after.iter().any(|item| item["code"] == "CW263"));
    assert!(after.iter().any(|item| item["code"] == "CW282"));

    assert!(
        !diagnostics_of(&control)
            .iter()
            .any(|item| item["code"] == "CW282")
    );

    let completion = server.call(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 36 }
        }),
    );
    assert!(
        completion.get("error").is_none(),
        "completion failed: {completion}"
    );
    let items = completion["result"]["items"]
        .as_array()
        .or_else(|| completion["result"].as_array())
        .unwrap_or_else(|| panic!("completion returned no list: {completion}"));
    assert!(
        !items.is_empty(),
        "completion list must be positive evidence: {completion}"
    );
    let shutdown = server.shutdown();
    assert!(shutdown.responded, "server did not answer shutdown");
    assert!(
        shutdown.status.unwrap().success(),
        "server did not exit successfully"
    );
}

#[test]
fn standalone_setup_failures_report_actionable_rules_errors() {
    let scratch = TempDir::new().unwrap();
    let missing_rules = scratch.path().join("missing-rules");
    let mut missing = TestServer::builder()
        .init_opts(json!({
            "language": "hoi4",
            "rulesCache": missing_rules.to_string_lossy(),
            "localisationLanguages": ["English"]
        }))
        .spawn();
    missing.initialized();
    missing.send_request(
        17,
        "workspace/executeCommand",
        json!({"command": "reloadrulesconfig", "arguments": []}),
    );
    let missing_log = missing.read_until("missing rules reload warning", |frame| {
        frame["method"] == "window/logMessage"
            && frame["params"]["message"].as_str().is_some_and(|message| {
                message.contains("rulesCache") && message.contains("missing-rules")
            })
    });
    assert!(
        missing_log["params"]["message"]
            .as_str()
            .unwrap()
            .contains("does not exist")
    );
    let response = missing.read_response_for_id(17);
    assert!(
        response.get("error").is_none(),
        "reload request failed: {response}"
    );
    let missing_shutdown = missing.shutdown();
    assert!(missing_shutdown.responded);
    assert!(missing_shutdown.status.unwrap().success());

    let broken =
        "types = { type[foo] = { path = \"common/foo\" } }\nr = { a = <undefined_thing> }\n";
    let mut malformed = TestServer::builder()
        .rules_file("broken.cwt", broken)
        .spawn();
    malformed.initialized();
    let publish = malformed.read_until("diagnostic for malformed CWT", |frame| {
        frame["method"] == "textDocument/publishDiagnostics"
            && frame["params"]["uri"]
                .as_str()
                .is_some_and(|uri| uri.ends_with("broken.cwt"))
            && frame["params"]["diagnostics"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
    });
    let errors = diagnostics_of(&publish);
    let error = diagnostic(errors, "CW601");
    assert_eq!(error["severity"], 1);
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("undefined_thing")
    );
    assert!(
        publish["params"]["uri"]
            .as_str()
            .unwrap()
            .ends_with("broken.cwt")
    );
    let malformed_shutdown = malformed.shutdown();
    assert!(malformed_shutdown.responded);
    assert!(malformed_shutdown.status.unwrap().success());
}
