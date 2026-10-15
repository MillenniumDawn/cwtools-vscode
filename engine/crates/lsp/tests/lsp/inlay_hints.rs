use std::time::Duration;

use serde_json::{Value, json};

use crate::common::{TestServer, bom, poll_until_attempts};

#[test]
fn test_inlay_hint_shows_loc_title_after_known_id() {
    // End-to-end: the server advertises the inlayHint capability, and a request
    // over a script that references a known decision id (with a localised title)
    // returns one hint carrying that title, positioned just past the id.
    const RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
"#;
    // The opened script referencing the id as a value on line 1.
    let rel_path = "common/decisions/use.txt";
    let text = "wrapper = {\n    ref = my_decision\n}\n";
    let mut server = TestServer::builder()
        .rules_file("r.cwt", RULES)
        .vanilla() // empty dir -> index marked complete
        // The definition (indexed as a `decision` instance named `my_decision`).
        .file("common/decisions/defs.txt", "my_decision = { cost = 1 }\n")
        // Its localised title.
        .file(
            "localisation/test_l_english.yml",
            bom("l_english:\n my_decision:0 \"My Decision\"\n"),
        )
        .file(rel_path, text)
        .spawn();
    // Capability advertised.
    assert_eq!(
        server.init_response()["result"]["capabilities"]["inlayHintProvider"],
        true,
        "inlayHint capability must be advertised: {}",
        server.init_response()
    );

    server.initialized();
    server.wait_for_scan_done();
    server.open(rel_path, text);

    // Poll the inlay request until the loc map + type index are populated.
    let uri = server.uri(rel_path);
    let mut hint = Value::Null;
    let _ = poll_until_attempts(30, Duration::from_millis(500), || {
        let result = server.request(
            "textDocument/inlayHint",
            json!({
                "textDocument": { "uri": uri },
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 100, "character": 0 },
                },
            }),
        );
        let first = result.as_array()?.first()?;
        hint = first.clone();
        Some(())
    });
    server.stop();

    assert_eq!(hint["label"], "My Decision", "got: {hint}");
    // Anchored just past `my_decision` on line 1: "    ref = my_decision" ends at col 21.
    assert_eq!(hint["position"]["line"], 1, "got: {hint}");
    assert_eq!(hint["position"]["character"], 21, "got: {hint}");
    assert_eq!(hint["paddingLeft"], true, "got: {hint}");
}

#[test]
fn test_inlay_hint_shows_rule_aware_scope() {
    const RULES: &str = r#"
types = {
    type[decision] = { path = "common/decisions" }
}
scopes = {
    Country = { aliases = { country } }
    Character = { aliases = { character } }
}
decision = {
    ## push_scope = character
    custom = {
        add = int
    }
}
"#;
    let rel_path = "common/decisions/scope.txt";
    let text = "my_decision = {\n    custom = {\n        add = 1\n    }\n}\n";
    let mut server = TestServer::builder()
        .rules(RULES)
        .file(rel_path, text)
        .init_opts(json!({ "inlayHintsScopes": true, "inlayHintsLocTitles": false }))
        .spawn();
    server.initialized();
    server.wait_for_scan_done();
    server.open_and_wait(rel_path, text);
    let value = server.request(
        "textDocument/inlayHint",
        json!({
            "textDocument": { "uri": server.uri(rel_path) },
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 10, "character": 0 } }
        }),
    );
    server.stop();
    let hints = value
        .as_array()
        .unwrap_or_else(|| panic!("inlay result array: {value}"));
    assert_eq!(hints.len(), 1, "got: {value}");
    assert_eq!(hints[0]["label"], "→ character", "got: {value}");
    assert_eq!(hints[0]["position"]["line"], 1, "got: {value}");
    assert_eq!(hints[0]["position"]["character"], 13, "got: {value}");
    assert_eq!(hints[0]["tooltip"], Value::Null, "got: {value}");
    assert_eq!(hints[0]["textEdits"], Value::Null, "got: {value}");
}
