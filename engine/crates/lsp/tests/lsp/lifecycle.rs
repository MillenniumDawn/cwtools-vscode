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
fn test_workspace_diagnostics_publish_every_closed_file() {
    // #1070: with the per-scan budget gone, a workspace larger than the old cap
    // publishes diagnostics for every closed file.
    const CLOSED_FILE_COUNT: usize = 2_001;
    let mut builder = TestServer::builder();
    for i in 0..CLOSED_FILE_COUNT {
        builder = builder.file(&format!("common/things/f{i}.txt"), "thing = yes\n");
    }
    let mut server = builder.spawn();
    server.initialized();

    let mut published: Vec<String> = Vec::new();
    server.read_until("the workspace validation summary", |frame| {
        if frame["method"] == "textDocument/publishDiagnostics"
            && let Some(uri) = frame["params"]["uri"].as_str()
            && uri.contains("/common/things/f")
        {
            published.push(uri.to_string());
        }
        frame["method"] == "window/logMessage"
            && frame["params"]["message"]
                .as_str()
                .is_some_and(|message| message.starts_with("Workspace validation complete:"))
    });
    published.sort();
    published.dedup();
    assert_eq!(
        published.len(),
        CLOSED_FILE_COUNT,
        "every closed file must receive a publishDiagnostics notification, none held back"
    );
}

#[test]
fn test_workspace_diagnostics_disable_clears_every_closed_file() {
    // #1070: switching workspace-wide diagnostics off must clear every closed
    // file a previous scan published, not a budgeted remainder.
    const CLOSED_FILE_COUNT: usize = 5;
    let mut builder = TestServer::builder();
    for i in 0..CLOSED_FILE_COUNT {
        builder = builder.file(&format!("common/things/f{i}.txt"), "thing = {\n");
    }
    let mut server = builder.spawn();
    server.initialized();

    let rel = |i: usize| format!("common/things/f{i}.txt");
    let publish_counts = |frames: &[Value], rel: &str| -> Vec<usize> {
        frames
            .iter()
            .filter(|frame| {
                frame["method"] == "textDocument/publishDiagnostics"
                    && frame["params"]["uri"]
                        .as_str()
                        .is_some_and(|uri| uri.ends_with(rel))
            })
            .map(|frame| {
                frame["params"]["diagnostics"]
                    .as_array()
                    .map_or(0, |a| a.len())
            })
            .collect()
    };
    let collects_scan_publishes = |server: &mut TestServer| -> Vec<Value> {
        let mut publishes: Vec<Value> = Vec::new();
        // Bounded: the collector stops on the scan's completion log, not on EOF.
        server.read_until("the scan completion of this phase", |frame| {
            if frame["method"] == "textDocument/publishDiagnostics"
                && frame["params"]["uri"]
                    .as_str()
                    .is_some_and(|uri| uri.contains("/common/things/"))
            {
                publishes.push(frame.clone());
            }
            frame["method"] == "window/logMessage"
                && frame["params"]["message"]
                    .as_str()
                    .is_some_and(|m| m.starts_with("Workspace validation complete:"))
        });
        publishes
    };

    let first = collects_scan_publishes(&mut server);
    for i in 0..CLOSED_FILE_COUNT {
        let rel = rel(i);
        let counts = publish_counts(&first, &rel);
        assert!(
            counts.iter().any(|&c| c > 0),
            "the first scan must publish errors for {rel}",
        );
    }

    server.notify(
        "workspace/didChangeConfiguration",
        json!({ "settings": { "workspaceWideDiagnostics": false } }),
    );
    let cleared = collects_scan_publishes(&mut server);
    server.stop();

    for i in 0..CLOSED_FILE_COUNT {
        let rel = rel(i);
        let counts = publish_counts(&cleared, &rel);
        assert!(
            !counts.is_empty(),
            "every previously published closed file must be cleared, not just files inside the old clear budget: {rel}"
        );
        assert!(
            counts.iter().all(|&c| c == 0),
            "the clear publishes must be empty diagnostics for {rel}, got: {counts:?}"
        );
    }
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
