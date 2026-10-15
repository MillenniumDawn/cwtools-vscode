use std::time::Duration;

use serde_json::{Value, json};

use super::poll::poll_until_attempts;
use super::server::{ServerBuilder, TestServer};

/// A builder for a server over `rules` (as `r.cwt`) and `files`, with an empty
/// base game so the index counts as complete, and the client's `caps`.
pub fn feature_builder(rules: &str, files: &[(&str, &str)], caps: Value) -> ServerBuilder {
    let mut builder = TestServer::builder()
        .rules_file("r.cwt", rules)
        .vanilla()
        .capabilities(caps);
    for (rel, content) in files {
        builder = builder.file(rel, *content);
    }
    builder
}

/// Start `builder`'s server, run the workspace scan (which indexes every file,
/// open or not), then didOpen the `open` files, taken from `files`, waiting for
/// each one's diagnostics.
pub fn feature_start(builder: ServerBuilder, files: &[(&str, &str)], open: &[&str]) -> TestServer {
    let mut server = builder.spawn();
    server.initialized();
    server.wait_for_scan_done();
    for &rel in open {
        let content = files
            .iter()
            .find(|(r, _)| *r == rel)
            .map(|(_, c)| *c)
            .expect("an open file must be one of the written files");
        server.open_and_wait(rel, content);
    }
    server
}

/// Spawn a server with `rules`, write `files` to disk, initialize with
/// `client_caps`, run the workspace scan (which indexes every file, open or
/// not), didOpen the `open` files, then issue `method` against `doc_rel` with
/// `extra` merged into the request params. Positive expectations poll until a
/// non-empty result; negative expectations query once after readiness. Returns
/// the JSON `result`.
pub fn feature_request(
    rules: &str,
    files: &[(&str, &str)],
    open: &[&str],
    client_caps: Value,
    doc_rel: &str,
    method: &str,
    extra: Value,
) -> Value {
    feature_request_impl(
        rules,
        files,
        open,
        client_caps,
        doc_rel,
        FeatureRequest {
            method,
            extra,
            once_after_ready: false,
        },
    )
}

/// Query exactly once after the workspace scan and didOpen diagnostics have
/// signaled readiness. Negative expectations use this form so a delayed
/// positive result cannot be hidden by retrying until the server changes state.
pub fn feature_request_once(
    rules: &str,
    files: &[(&str, &str)],
    open: &[&str],
    client_caps: Value,
    doc_rel: &str,
    method: &str,
    extra: Value,
) -> Value {
    feature_request_impl(
        rules,
        files,
        open,
        client_caps,
        doc_rel,
        FeatureRequest {
            method,
            extra,
            once_after_ready: true,
        },
    )
}

struct FeatureRequest<'a> {
    method: &'a str,
    extra: Value,
    once_after_ready: bool,
}

fn feature_request_impl(
    rules: &str,
    files: &[(&str, &str)],
    open: &[&str],
    client_caps: Value,
    doc_rel: &str,
    request: FeatureRequest<'_>,
) -> Value {
    let mut server = feature_start(feature_builder(rules, files, client_caps), files, open);

    let doc_uri = server.uri(doc_rel);
    let params = || {
        let mut params = json!({ "textDocument": { "uri": doc_uri } });
        if let Some(obj) = request.extra.as_object() {
            for (k, v) in obj {
                params[k.as_str()] = v.clone();
            }
        }
        params
    };
    let mut result = Value::Null;
    if request.once_after_ready {
        result = server.request(request.method, params());
    } else {
        let _ = poll_until_attempts(40, Duration::from_millis(200), || {
            result = server.request(request.method, params());
            let empty = result.is_null() || result.as_array().is_some_and(Vec::is_empty);
            (!empty).then_some(())
        });
    }
    server.stop();
    result
}
