use serde_json::json;

use crate::common::{GOTO_RULES, TestServer};

/// A `complete_effect` block whose only member is a call to the
/// `common/inline_scripts/my_script` body — reuses [`GOTO_RULES`]'s `decision`
/// type and `effect` alias set, the same shape `test_did_open_definition_clears_open_caller_stale_error`
/// validates a direct scripted_effect reference against.
const INLINE_SCRIPT_CALLER_TEXT: &str = "my_dec = {\n    complete_effect = {\n        inline_script = { script = my_script }\n    }\n}\n";

/// An effect key that matches no `<scripted_effect>` instance — CW263 once expanded.
const INLINE_SCRIPT_BAD_TEXT: &str = "not_a_real_effect = yes\n";

/// A reference to `my_se` (defined by [`inline_script_server`]'s
/// `effects.txt`) — resolves cleanly once expanded.
const INLINE_SCRIPT_CLEAN_TEXT: &str = "my_se = yes\n";

const CALLER_REL: &str = "common/decisions/caller.txt";
const SCRIPT_REL: &str = "common/inline_scripts/my_script.txt";

/// Workspace with a `common/decisions/caller.txt` that calls the inline script
/// `my_script`, and a `common/scripted_effects/effects.txt` defining `my_se`
/// (for tests that need a body reference to resolve cleanly). `script_text`
/// seeds `common/inline_scripts/my_script.txt` before the server starts;
/// `None` leaves the script absent (a call to a script that doesn't exist).
fn inline_script_server(script_text: Option<&str>) -> TestServer {
    let mut builder = TestServer::builder()
        .rules_file("r.cwt", GOTO_RULES)
        .vanilla() // empty dir -> index marked complete
        .file(CALLER_REL, INLINE_SCRIPT_CALLER_TEXT)
        .file(
            "common/scripted_effects/effects.txt",
            "my_se = { log = \"hi\" }\n",
        );
    if let Some(text) = script_text {
        builder = builder.file(SCRIPT_REL, text);
    }
    let mut server = builder.spawn();
    server.initialized();
    server
}

#[test]
fn test_inline_script_call_site_reports_body_diagnostic() {
    // A call site whose expanded body has a bad member must report that
    // body's diagnostic on the call site itself, naming the script line it
    // came from (#259).
    let mut server = inline_script_server(Some(INLINE_SCRIPT_BAD_TEXT));
    server.wait_for_scan_done();

    server.open(CALLER_REL, INLINE_SCRIPT_CALLER_TEXT);
    let diags = server.diag_messages_for("caller.txt", 1);
    server.stop();
    assert!(
        diags.iter().any(|(code, _)| code == "CW263"),
        "expected the expanded body's bad effect key to report CW263 on the call site, got: {diags:?}"
    );
    assert!(
        diags.iter().any(|(code, message)| code == "CW263"
            && message.contains("(in common/inline_scripts/my_script.txt:1)")),
        "expected the CW263 message to name the script line it came from, got: {diags:?}"
    );
}

#[test]
fn test_inline_script_call_to_missing_script_is_cw274() {
    // No `common/inline_scripts/my_script.txt` exists anywhere in the
    // workspace, so the call site can't pull in a body at all — CW274 (#259).
    let mut server = inline_script_server(None);
    server.wait_for_scan_done();

    server.open(CALLER_REL, INLINE_SCRIPT_CALLER_TEXT);
    let diags = server.diag_codes_for("caller.txt", 1);
    server.stop();
    assert!(
        diags.contains(&"CW274".to_string()),
        "a call to a script that doesn't exist should report CW274, got: {diags:?}"
    );
}

#[test]
fn test_editing_inline_script_revalidates_open_caller() {
    // The script doesn't exist at scan time, so the open caller starts out
    // CW274. Authoring the script (opening it with a bad body) must revalidate
    // the caller into CW263 without touching caller.txt itself, and a further
    // edit to a clean body must revalidate it again into no diagnostics at
    // all — the dependent sweep a changed export already gets, extended to a
    // changed inline script (#259).
    let mut server = inline_script_server(None);
    server.wait_for_scan_done();

    server.open(CALLER_REL, INLINE_SCRIPT_CALLER_TEXT);
    let before = server.diag_codes_for("caller.txt", 1);
    assert!(
        before.contains(&"CW274".to_string()),
        "expected CW274 before the script exists, got: {before:?}"
    );

    server.open(SCRIPT_REL, INLINE_SCRIPT_BAD_TEXT);
    // The caller can republish more than once around this edit (its own
    // debounced validate plus the dependent sweep it queues); wait for the
    // settled CW274-gone / CW263-present state instead of assuming exactly
    // one publish.
    let mut after_create = Vec::new();
    for _ in 0..20 {
        after_create = server.diag_codes_for("caller.txt", 1);
        if !after_create.contains(&"CW274".to_string()) {
            break;
        }
    }
    assert!(
        !after_create.contains(&"CW274".to_string()),
        "creating the script should clear the caller's CW274, got: {after_create:?}"
    );
    assert!(
        after_create.contains(&"CW263".to_string()),
        "the new script's bad body should now report CW263 on the caller, got: {after_create:?}"
    );

    // A real edit to the already-open script, to a body that resolves
    // cleanly against the caller's rules and scope.
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": server.uri(SCRIPT_REL), "version": 2 },
            "contentChanges": [{ "text": INLINE_SCRIPT_CLEAN_TEXT }]
        }),
    );
    let mut after_fix = vec!["CW263".to_string()];
    for _ in 0..20 {
        after_fix = server.diag_codes_for("caller.txt", 1);
        if !after_fix.contains(&"CW263".to_string()) {
            break;
        }
    }
    server.stop();
    assert!(
        !after_fix.contains(&"CW263".to_string()),
        "editing the script to a clean body should clear the caller's CW263, got: {after_fix:?}"
    );
}
