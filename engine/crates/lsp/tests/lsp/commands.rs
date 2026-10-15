use std::time::{Duration, Instant};

use serde_json::json;

use crate::common::{STORM_FILE, storm_builder, storm_server, storm_start, watched_changes};

#[test]
fn test_get_file_types_answers_during_watched_flood() {
    // The direct #90 regression: a large watched flood must not starve a cheap
    // getFileTypes request. With validation off the message future, the request
    // answers well under 2s.
    let mut server = storm_server();
    let uris: Vec<String> = (0..80)
        .map(|i| server.write_file_uri(&format!("common/decisions/g{i}.txt"), STORM_FILE))
        .collect();

    // Fire the flood, then immediately ask for file types.
    server.send(&watched_changes(&uris));
    let sent = Instant::now();
    server.send_request(
        777,
        "workspace/executeCommand",
        json!({ "command": "getFileTypes", "arguments": [uris[0]] }),
    );
    server.read_until_within(Duration::from_secs(5), "the getFileTypes response", |v| {
        v["id"] == 777
    });
    let elapsed = sent.elapsed();
    server.stop();

    assert!(
        elapsed < Duration::from_secs(2),
        "getFileTypes took {elapsed:?} during a watched flood (should be < 2s)"
    );
}

#[test]
fn test_unknown_execute_command_returns_error() {
    // An unrecognized workspace/executeCommand must surface a JSON-RPC error
    // naming the command — a silent `null` success masks client/engine
    // version drift (a newer client invoking a command this server lacks).
    let mut server = storm_server();
    let v = server.call(
        "workspace/executeCommand",
        json!({ "command": "notARealCommand", "arguments": [] }),
    );
    server.stop();

    assert!(
        !v["error"].is_null(),
        "unknown command must produce an error response, got: {v}"
    );
    assert!(
        v["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("notARealCommand")),
        "the error should name the offending command, got: {}",
        v["error"]
    );
}

#[test]
fn test_validate_workspace_command_returns_summary() {
    // `validateWorkspace` runs a full scan and returns aggregate counts. One
    // malformed closed file is enough to produce a non-zero error count.
    let mut server = storm_start(storm_builder().file("common/decisions/bad.txt", "bad_dec = {\n"));
    let summary = server.request(
        "workspace/executeCommand",
        json!({ "command": "validateWorkspace", "arguments": [] }),
    );
    server.stop();

    let result = summary
        .as_object()
        .expect("validateWorkspace returned an object");
    assert_eq!(
        result["totalFiles"].as_u64(),
        Some(1),
        "summary must count the one workspace file, got: {result:?}"
    );
    assert_eq!(
        result["validatedFiles"].as_u64(),
        Some(1),
        "summary must count the one validated file, got: {result:?}"
    );
    assert_eq!(
        result["heldBackFiles"].as_u64(),
        Some(0),
        "one file is inside the diagnostics budget, got: {result:?}"
    );
    assert_eq!(
        result["filesWithErrors"].as_u64(),
        Some(1),
        "the malformed file must carry an error, got: {result:?}"
    );
    assert!(
        result["totalErrors"].as_u64().unwrap_or(0) > 0,
        "summary must report a positive error count, got: {result:?}"
    );
}

/// #223: `validateWorkspace` retries while another scan holds the guard, then
/// answers `{ "busy": true }` if it never wins. A regression in the deadline,
/// the CAS `Busy` result, or the JSON mapping could hang the command or return
/// a stale summary.
#[test]
fn test_validate_workspace_reports_busy_when_scan_never_releases() {
    let signals = tempfile::tempdir().unwrap();
    let gate = signals.path().join("scan-hold");
    let ready = signals.path().join("scan-hold-ready");

    // Hold the competing scan until validateWorkspace gives up on its original
    // 1s retry deadline and reports busy.
    let mut server = storm_start(
        storm_builder()
            .env("CWTOOLS_SCAN_HOLD_FILE", &gate)
            .env("CWTOOLS_SCAN_HOLD_READY_FILE", &ready)
            .env("CWTOOLS_RETRY_DEADLINE_MS", "1000"),
    );

    // Start a competing scan and wait for the scan-started signal, so the hold
    // is definitely active for the command's whole deadline.
    std::fs::write(&gate, b"hold").unwrap();
    server.reindex_until_scan_starts();

    server.send_request(
        903,
        "workspace/executeCommand",
        json!({ "command": "validateWorkspace", "arguments": [] }),
    );
    // The file gate remains closed, so any answer inside this window must be
    // the command's own give-up response.
    let (response, saw_revalidation) = server.wait_for_response_watching_scans(
        903,
        Duration::from_secs(5),
        "validateWorkspace to give up on its 1s deadline",
    );
    assert!(ready.exists(), "competing scan did not enter its file gate");
    server.stop();

    assert!(
        !saw_revalidation,
        "a revalidation ran after validateWorkspace gave up"
    );
    assert_eq!(
        response["result"],
        json!({ "busy": true }),
        "validateWorkspace must report busy when the guard never releases, got: {response}"
    );
}

#[test]
fn test_get_file_types_returns_rule_types_and_path_fallbacks() {
    let cases = [
        ("common/national_focus/empty.txt", vec!["focus"]),
        ("common/untyped/a.txt", vec!["script", "txt"]),
        ("events/a.txt", vec!["event", "txt"]),
        (
            "common/scripted_effects/a.txt",
            vec!["script", "scripted_effect", "txt"],
        ),
        (
            "common/scripted_triggers/a.txt",
            vec!["script", "scripted_trigger", "txt"],
        ),
        ("notes/readme.yml", vec![]),
    ];
    let mut builder = storm_builder().rules_file(
        "r.cwt",
        r#"
        types = { type[focus] = { path = "game/common/national_focus" } }
        focus = { x = bool }
    "#,
    );
    for (rel, _) in &cases {
        builder = builder.file(rel, "");
    }
    let mut server = storm_start(builder);
    for (rel, expected) in &cases {
        let uri = server.uri(rel);
        let response = server.call(
            "workspace/executeCommand",
            json!({ "command": "getFileTypes", "arguments": [uri] }),
        );
        assert!(response["error"].is_null(), "{rel}: {response}");
        assert_eq!(response["result"], json!(expected), "{rel}: {response}");
    }
}
