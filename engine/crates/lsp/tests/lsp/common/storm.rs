use serde_json::{Value, json};

use super::fixtures::GOTO_RULES;
use super::scan::wait_for_scan_release;
use super::server::{ServerBuilder, TestServer};
use super::wire::{jsonrpc_notification, path_uri};

/// A builder for a server whose `[validate]` log lines can be read back with
/// [`TestServer::profiling_log`]: GOTO_RULES, an empty base-game root, and scan
/// guard releases signalled through a marker file.
pub fn storm_builder() -> ServerBuilder {
    storm_base().rules_file("r.cwt", GOTO_RULES)
}

/// [`storm_builder`] over an empty rules directory.
pub fn storm_base() -> ServerBuilder {
    TestServer::builder()
        .empty_rules()
        .vanilla()
        // The `[validate]` line is a tracing event, read back via
        // exportProfilingLog. Removing RUST_LOG lets init_tracing install the
        // `info` filter instead of an empty RUST_LOG="" that captures nothing.
        .env_remove("RUST_LOG")
        .env("CWTOOLS_PROFILE", "1")
        .scan_release_marker()
}

/// Run the init handshake on `builder` and wait for the startup scan, so
/// `index_ready` is set and per-file validation publishes (and logs).
pub fn storm_start(builder: ServerBuilder) -> TestServer {
    let mut server = builder.spawn();
    server.initialized();
    server.wait_for_scan_done();
    // The guard releases just after bar-off. Consume the startup scan's marker
    // here, so a test waiting on its own scan's release never sees this one.
    wait_for_scan_release(&server.scan_release_path());
    server
}

/// [`storm_start`] on [`storm_builder`] as it stands.
pub fn storm_server() -> TestServer {
    storm_start(storm_builder())
}

impl TestServer {
    /// Fetch the server's in-memory profiling log (requires `CWTOOLS_PROFILE`,
    /// which [`storm_builder`] sets). The buffer is cumulative and non-draining,
    /// so callers compare CUMULATIVE `[validate]` counts across successive
    /// windows.
    pub fn profiling_log(&mut self) -> String {
        let log = self.request(
            "workspace/executeCommand",
            json!({ "command": "exportProfilingLog", "arguments": [] }),
        );
        log.as_str()
            .unwrap_or_else(|| panic!("exportProfilingLog returned a non-string: {log}"))
            .to_string()
    }

    /// Write a file under the workspace (not opened) and return its URI.
    pub fn write_file_uri(&self, rel: &str, content: &str) -> String {
        path_uri(self.write_file(rel, content))
    }
}

/// Count `[validate] (trigger)` lines in a fetched profiling log.
pub fn count_validate_log(log: &str, trigger: &str) -> usize {
    log.matches(&format!("[validate] ({trigger})")).count()
}

pub fn count_publishes(frames: &[Value], suffix: &str) -> usize {
    frames
        .iter()
        .filter(|v| {
            v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with(suffix))
        })
        .count()
}

pub fn watched_changes(uris: &[String]) -> String {
    let changes: Vec<Value> = uris
        .iter()
        .map(|u| json!({ "uri": u, "type": 2 }))
        .collect();
    jsonrpc_notification(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": changes }),
    )
}

/// A `workspace/didChangeWatchedFiles` notification marking the URIs as newly
/// created (type 1). Distinct from `watched_changes` (type 2, CHANGED): the
/// loc-discovery cache is invalidated on create/delete but not on change.
pub fn watched_created(uris: &[String]) -> String {
    let changes: Vec<Value> = uris
        .iter()
        .map(|u| json!({ "uri": u, "type": 1 }))
        .collect();
    jsonrpc_notification(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": changes }),
    )
}

/// A `workspace/didChangeWatchedFiles` notification marking the URIs as deleted
/// (type 3). A deleted folder is one URI here, the way the client reports it.
pub fn watched_deleted(uris: &[String]) -> String {
    let changes: Vec<Value> = uris
        .iter()
        .map(|u| json!({ "uri": u, "type": 3 }))
        .collect();
    jsonrpc_notification(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": changes }),
    )
}
