use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::common::{TestServer, path_uri, poll_until, shutdown_request};

#[test]
fn test_shutdown_reports_a_graceful_successful_exit() {
    let mut server = TestServer::builder()
        .deadline(Duration::from_secs(5))
        .spawn();
    let init = server.init_response();
    assert_eq!(init["id"], 1);
    assert!(init["result"]["capabilities"].is_object());
    server.initialized();
    let shutdown = server.shutdown();
    assert!(
        shutdown.responded,
        "server must respond to shutdown before exit is sent"
    );
    assert!(
        shutdown
            .status
            .expect("server exit must be observed before the kill")
            .success(),
        "server must exit successfully after shutdown/exit"
    );
}

#[test]
fn test_read_deadline_fails_a_silent_server_with_its_stderr() {
    let mut server = TestServer::builder()
        .deadline(Duration::from_secs(1))
        .spawn_uninitialized();
    let started = Instant::now();
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| server.read_frame()));
    let payload =
        outcome.expect_err("an uninitialized server sends nothing, so the read must fail");
    let message = payload
        .downcast_ref::<String>()
        .expect("the failure carries its message");
    assert!(
        message.contains("timed out waiting for the next frame"),
        "got: {message}"
    );
    assert!(message.contains("server still running"), "got: {message}");
    assert!(message.contains("--- server stderr ---"), "got: {message}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the read must give up at its deadline"
    );
    let shutdown = server.shutdown();
    assert!(
        !shutdown.responded,
        "an uninitialized server refuses shutdown"
    );
}

#[test]
fn test_lsp_rejects_an_oversized_frame_without_waiting_for_the_body() {
    const MAX_LSP_FRAME_BYTES: usize = 64 * 1024 * 1024;

    let mut server = TestServer::builder().spawn_uninitialized();
    let header = format!("Content-Length: {}\r\n\r\n", MAX_LSP_FRAME_BYTES + 1);
    server.send_raw(header.as_bytes());

    let deadline = Instant::now() + Duration::from_secs(5);
    let exited = poll_until(deadline, Duration::from_millis(25), || {
        server.has_exited().then_some(())
    });
    assert!(
        exited.is_some(),
        "server waited for the oversized frame body"
    );
}

#[test]
fn test_lsp_full_lifecycle() {
    let mut server = TestServer::builder().spawn();
    let resp = server.init_response();
    assert_eq!(resp["id"], 1);
    assert!(resp["result"]["capabilities"].is_object());

    server.initialized();
    server.send(&shutdown_request(2));
    let resp = server.read_response_for_id(2);
    server.stop();

    assert_eq!(resp["id"], 2);
    // `resp["result"]` is Null for a missing key too, which used to pass when
    // the server answered -32602, so reach for the keys themselves.
    assert!(resp.get("error").is_none());
    assert_eq!(resp.get("result"), Some(&Value::Null));
}

#[test]
fn test_workspace_diagnostics_publish_and_clear_every_closed_file() {
    // #1070: past the old 2,000-file budget, a scan publishes every closed
    // file, and switching workspace-wide diagnostics off clears every one.
    const CLOSED_FILE_COUNT: usize = 2_001;
    let mut builder = TestServer::builder();
    for i in 0..CLOSED_FILE_COUNT {
        builder = builder.file(&format!("common/things/f{i}.txt"), "thing = {\n");
    }
    let mut server = builder.spawn();
    server.initialized();

    let scan_publishes = |server: &mut TestServer| -> HashMap<String, Vec<usize>> {
        let mut publishes: HashMap<String, Vec<usize>> = HashMap::new();
        // Bounded: the collector stops on the scan's completion log, not on EOF.
        server.read_until("the workspace validation summary", |frame| {
            if frame["method"] == "textDocument/publishDiagnostics"
                && let Some(uri) = frame["params"]["uri"].as_str()
                && uri.contains("/common/things/f")
            {
                let count = frame["params"]["diagnostics"]
                    .as_array()
                    .map_or(0, Vec::len);
                publishes.entry(uri.to_string()).or_default().push(count);
            }
            frame["method"] == "window/logMessage"
                && frame["params"]["message"]
                    .as_str()
                    .is_some_and(|m| m.starts_with("Workspace validation complete:"))
        });
        publishes
    };

    let first = scan_publishes(&mut server);
    assert_eq!(
        first.len(),
        CLOSED_FILE_COUNT,
        "every closed file must be published, none held back"
    );
    assert!(
        first.values().all(|counts| counts.iter().any(|&c| c > 0)),
        "the first scan must publish each closed file's errors"
    );

    server.notify(
        "workspace/didChangeConfiguration",
        json!({ "settings": { "workspaceWideDiagnostics": false } }),
    );
    let cleared = scan_publishes(&mut server);
    server.stop();

    assert_eq!(
        cleared.len(),
        CLOSED_FILE_COUNT,
        "every previously published closed file must be cleared"
    );
    let not_one_empty_clear: Vec<_> = cleared
        .iter()
        .filter(|(_, counts)| counts.as_slice() != [0])
        .collect();
    assert!(
        not_one_empty_clear.is_empty(),
        "each closed file must get exactly one empty clear; {} did not, e.g. {:?}",
        not_one_empty_clear.len(),
        not_one_empty_clear.first()
    );
}

#[test]
fn test_lsp_unknown_notification_does_not_crash() {
    let mut server = TestServer::builder().spawn();
    server.notify("nonexistent/method", json!({}));
    server.send(&shutdown_request(99));
    let resp = server.read_response_for_id(99);
    server.stop();

    assert_eq!(resp["id"], 99);
}

#[test]
fn test_initialize_advertises_the_new_capabilities() {
    let mut server = TestServer::builder()
        .capabilities(json!({ "window": { "workDoneProgress": true } }))
        .spawn();
    let resp = server.init_response().clone();
    server.stop();
    let caps = &resp["result"]["capabilities"];
    println!(
        "semanticTokensProvider = {}",
        caps["semanticTokensProvider"]
    );
    println!("colorProvider = {}", caps["colorProvider"]);
    println!("codeActionProvider = {}", caps["codeActionProvider"]);
    println!("workspace = {}", caps["workspace"]);

    assert_eq!(caps["semanticTokensProvider"]["full"]["delta"], true);
    let types = caps["semanticTokensProvider"]["legend"]["tokenTypes"]
        .as_array()
        .expect("a token legend");
    assert_eq!(
        types[0], "comment",
        "legend order is the wire encoding; index 0 must stay `comment`"
    );
    assert_eq!(types.len(), 11);
    assert_eq!(
        caps["semanticTokensProvider"]["legend"]["tokenModifiers"][0],
        "declaration"
    );
    assert_eq!(caps["colorProvider"], true);
    assert_eq!(caps["codeLensProvider"]["resolveProvider"], true);
    let kinds = caps["codeActionProvider"]["codeActionKinds"]
        .as_array()
        .expect("code action kinds");
    assert!(
        kinds.iter().any(|k| k == "source.fixAll"),
        "editor.codeActionsOnSave needs source.fixAll advertised: {kinds:?}"
    );
    assert_eq!(caps["workspace"]["workspaceFolders"]["supported"], true);
    assert_eq!(
        caps["workspace"]["workspaceFolders"]["changeNotifications"],
        true
    );
    assert_eq!(caps["documentFormattingProvider"], true);
    assert_eq!(caps["documentRangeFormattingProvider"], true);
    let commands = caps["executeCommandProvider"]["commands"]
        .as_array()
        .expect("execute commands");
    assert!(
        commands.iter().any(|c| c == "formatWorkspace"),
        "formatWorkspace must be advertised: {commands:?}"
    );
    assert!(
        commands.iter().any(|c| c == "genlocall"),
        "genlocall must be advertised: {commands:?}"
    );
}

#[test]
fn test_missing_workspace_root_is_reported_as_error_log_message() {
    let mut server = TestServer::builder()
        .deadline(Duration::from_secs(5))
        .spawn_uninitialized();
    let missing = server.path("no_such_workspace");
    let missing_path = missing.to_string_lossy().to_string();
    server.initialize_with(json!({
        "processId": std::process::id(),
        "rootUri": path_uri(&missing),
        "capabilities": {},
    }));
    server.initialized();

    server.read_until("an error log message for the missing root", |frame| {
        frame["method"] == "window/logMessage"
            && frame["params"]["type"].as_i64() == Some(1)
            && frame["params"]["message"].as_str().is_some_and(|m| {
                m.starts_with("error: discovery failed for")
                    && m.contains(&missing_path)
                    && m.contains("directory does not exist")
            })
    });
}
