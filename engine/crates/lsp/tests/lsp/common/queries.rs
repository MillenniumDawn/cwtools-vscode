use std::time::Duration;

use serde_json::{Value, json};

use super::poll::poll_until_attempts;
use super::server::TestServer;

/// Poll `textDocument/definition` at (line0, char0) in `doc_rel` until it names
/// a location, or the attempts run out. An empty answer means the index has not
/// landed yet, so it is retried rather than returned.
pub fn definition_locations(
    server: &mut TestServer,
    doc_rel: &str,
    line0: u32,
    char0: u32,
    attempts: usize,
) -> Vec<(String, u32)> {
    let uri = server.uri(doc_rel);
    let mut out = Vec::new();
    let _ = poll_until_attempts(attempts, Duration::from_millis(300), || {
        let result = server.request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line0, "character": char0 },
            }),
        );
        out = locations(&result);
        (!out.is_empty()).then_some(())
    });
    out
}

/// The `(uri, start_line)` of each location in a definition result, which is a
/// list, a single location, or `null` for none.
pub fn locations(result: &Value) -> Vec<(String, u32)> {
    let list = match result {
        Value::Array(list) => list.as_slice(),
        Value::Object(_) => std::slice::from_ref(result),
        _ => &[],
    };
    list.iter()
        .filter_map(|location| {
            Some((
                location["uri"].as_str()?.to_string(),
                location["range"]["start"]["line"].as_u64()? as u32,
            ))
        })
        .collect()
}
