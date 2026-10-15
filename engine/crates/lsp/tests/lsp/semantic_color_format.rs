use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::common::{TestServer, editor_server, poll_until};

/// Decode the delta-encoded `data` array back into absolute
/// `(line, start, length, type, modifiers)` tuples — exactly what a client does.
fn decode_semantic_tokens(data: &[Value]) -> Vec<(u32, u32, u32, u32, u32)> {
    let nums: Vec<u32> = data.iter().map(|v| v.as_u64().unwrap() as u32).collect();
    assert_eq!(nums.len() % 5, 0, "the stream must be quintuples");
    let mut out = Vec::new();
    let (mut line, mut start) = (0u32, 0u32);
    for c in nums.chunks(5) {
        line += c[0];
        start = if c[0] == 0 { start + c[1] } else { c[1] };
        out.push((line, start, c[2], c[3], c[4]));
    }
    out
}

#[test]
fn test_semantic_tokens_full_returns_a_decodable_classified_stream() {
    let text = "\
# a focus tree
my_focus = {
    id = my_focus
    cost = 10
    mood = calm
    text = focus_title_key
    available = {
        has_completed_focus = other_focus
        always = yes
    }
}
";
    let rel = "common/national_focus/tree.txt";
    let mut server = editor_server(&[(rel, text)]);

    let resp = server.call_with_id(
        2,
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": server.uri(rel) } }),
    );
    server.stop();

    assert_eq!(resp["id"], 2, "got: {resp}");
    let data = resp["result"]["data"].as_array().expect("token data");
    let tokens = decode_semantic_tokens(data);
    println!("semanticTokens/full data = {}", resp["result"]["data"]);
    println!("decoded (line, start, len, type, mods) = {tokens:#?}");

    // Legend indices, from `semantic::TOKEN_TYPES`.
    const COMMENT: u32 = 0;
    const PROPERTY: u32 = 1;
    const OPERATOR: u32 = 2;
    const NUMBER: u32 = 3;
    const STRING: u32 = 4;
    const KEYWORD: u32 = 5;
    const TYPE: u32 = 6;
    const ENUM_MEMBER: u32 = 7;
    const FUNCTION: u32 = 10;

    let find = |line: u32, start: u32| {
        tokens
            .iter()
            .find(|t| t.0 == line && t.1 == start)
            .copied()
            .unwrap_or_else(|| panic!("no token at {line}:{start} in {tokens:#?}"))
    };

    // Every token must be within its line and non-empty — the drift symptom.
    let lines: Vec<&str> = text.lines().collect();
    for (line, start, length, ..) in &tokens {
        let width = lines[*line as usize].chars().count() as u32;
        assert!(
            *length > 0 && start + length <= width,
            "token ({line},{start},{length}) runs past line {line} ({width} chars)"
        );
    }

    assert_eq!(find(0, 0).3, COMMENT, "the leading comment");
    assert_eq!(find(0, 0).2, 14, "comment length");
    assert_eq!(find(1, 0).3, PROPERTY, "the root key");
    assert_eq!(find(1, 9).3, OPERATOR);
    assert_eq!(find(3, 11).3, NUMBER, "cost = 10");
    assert_eq!(find(4, 11).3, ENUM_MEMBER, "mood = calm resolves the enum");
    assert_eq!(
        find(5, 11).3,
        STRING,
        "text = <localisation> stays a string"
    );
    assert_eq!(
        find(7, 8).3,
        FUNCTION,
        "has_completed_focus resolves through alias[trigger:…]"
    );
    assert_eq!(
        find(7, 30).3,
        TYPE,
        "its value resolves to a <focus> instance"
    );
    assert_eq!(find(8, 17).3, KEYWORD, "always = yes");
}

#[test]
fn test_semantic_tokens_delta_applies_wire_edit_and_falls_back_for_stale_base() {
    let original = "my_focus = {\n    cost = 10\n}\n";
    let updated = "my_focus = {\n    cost = 200\n}\n";
    let rel = "common/national_focus/tree.txt";
    let mut server = editor_server(&[(rel, original)]);
    let uri = server.uri(rel);

    let full = server.call_with_id(
        20,
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    );
    assert_eq!(full["id"], 20, "got: {full}");
    let original_data = full["result"]["data"]
        .as_array()
        .expect("full-token data")
        .clone();
    let original_id = full["result"]["resultId"]
        .as_str()
        .expect("full-token resultId")
        .to_string();

    let unchanged = server.call_with_id(
        21,
        "textDocument/semanticTokens/full/delta",
        json!({
            "textDocument": { "uri": uri },
            "previousResultId": original_id.clone(),
        }),
    );
    assert_eq!(unchanged["id"], 21, "got: {unchanged}");
    assert!(
        unchanged["result"]["resultId"].as_str().is_some(),
        "delta must advance its resultId: {unchanged}"
    );
    let unchanged_edits = unchanged["result"]["edits"]
        .as_array()
        .expect("delta edits");
    assert!(unchanged_edits.is_empty(), "got: {unchanged}");
    assert_eq!(
        original_data.len() % 5,
        0,
        "token quintets: {original_data:?}"
    );
    let unchanged_id = unchanged["result"]["resultId"]
        .as_str()
        .expect("unchanged delta resultId")
        .to_string();

    server.change(rel, 2, updated);
    // Wait until the change notification has been applied and its current
    // document diagnostics arrive before asking for the delta.
    server.wait_for_diagnostics(rel);
    let changed = server.call_with_id(
        22,
        "textDocument/semanticTokens/full/delta",
        json!({
            "textDocument": { "uri": uri },
            "previousResultId": unchanged_id.clone(),
        }),
    );
    assert_eq!(changed["id"], 22, "got: {changed}");
    let changed_id = changed["result"]["resultId"]
        .as_str()
        .expect("changed delta resultId")
        .to_string();
    assert_ne!(changed_id, unchanged_id, "delta must advance resultId");
    let edits = changed["result"]["edits"]
        .as_array()
        .expect("a current base must return semantic-token edits");
    assert_eq!(edits.len(), 1, "expected one changed-token edit: {changed}");
    let edit = &edits[0];
    let start = edit["start"].as_u64().expect("edit start") as usize;
    let delete_count = edit["deleteCount"].as_u64().expect("edit deleteCount") as usize;
    assert_eq!(start % 5, 0, "edit start must address token quintets");
    assert_eq!(delete_count % 5, 0, "deleteCount must cover token quintets");
    assert!(
        start + delete_count <= original_data.len(),
        "edit exceeds base stream"
    );
    let inserted_data = edit["data"].as_array().cloned().unwrap_or_default();
    assert_eq!(inserted_data.len() % 5, 0, "inserted token quintets");
    let mut changed_data = original_data[..start].to_vec();
    changed_data.extend(inserted_data);
    changed_data.extend_from_slice(&original_data[start + delete_count..]);
    assert!(
        changed["result"].get("data").is_none(),
        "delta must not return a full stream: {changed}"
    );

    let changed_tokens = decode_semantic_tokens(&changed_data);
    assert!(
        changed_tokens.contains(&(1, 11, 3, 3, 0)),
        "the new `200` number token must reach the client: {changed_tokens:?}"
    );
    assert!(
        !changed_tokens.contains(&(1, 11, 2, 3, 0)),
        "the old `10` number token must be removed: {changed_tokens:?}"
    );

    // A result ID the server no longer retains must fall back to a full
    // response, and that response must carry the current edited content.
    let stale = server.call_with_id(
        23,
        "textDocument/semanticTokens/full/delta",
        json!({
            "textDocument": { "uri": uri },
            "previousResultId": original_id,
        }),
    );
    server.stop();
    assert_eq!(stale["id"], 23, "got: {stale}");
    assert_eq!(stale["result"]["data"], json!(changed_data));
    assert!(
        stale["result"]["edits"].is_null(),
        "an evicted base must not return an edit against stale data: {stale}"
    );

    assert_ne!(
        original_data, changed_data,
        "the edit must change the wire token stream"
    );
}

#[test]
fn test_did_rename_files_moves_the_open_semantic_token_cache() {
    let text = "buffer_value = 9\n";
    let old_rel = "common/national_focus/tree.txt";
    let new_rel = "common/national_focus/renamed.txt";
    let mut server = editor_server(&[(old_rel, text)]);
    let old_path = server.path(old_rel);
    let new_path = server.path(new_rel);
    let old_uri = server.uri(old_rel);
    let new_uri = server.uri(new_rel);

    let full = server.call_with_id(
        30,
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": old_uri } }),
    );
    assert_eq!(full["id"], 30, "got: {full}");
    let original_data = full["result"]["data"]
        .as_array()
        .expect("semantic-token data")
        .clone();
    let tokens = decode_semantic_tokens(&original_data);
    assert!(
        tokens.contains(&(0, 15, 1, 3, 0)),
        "the cached open buffer contains its number token: {tokens:?}"
    );
    let previous_result_id = full["result"]["resultId"]
        .as_str()
        .expect("resultId")
        .to_string();

    std::fs::write(&old_path, "disk_value = 1\n").unwrap();
    std::fs::rename(&old_path, &new_path).unwrap();
    server.notify(
        "workspace/didRenameFiles",
        json!({
            "files": [{ "oldUri": old_uri, "newUri": new_uri }],
        }),
    );
    let delta = server.call_with_id(
        31,
        "textDocument/semanticTokens/full/delta",
        json!({
            "textDocument": { "uri": new_uri },
            "previousResultId": previous_result_id,
        }),
    );
    server.stop();
    assert_eq!(delta["id"], 31, "got: {delta}");
    assert_eq!(delta["result"]["edits"], json!([]), "got: {delta}");
    assert!(
        delta["result"].get("data").is_none(),
        "the moved cache must answer with a delta, not unrelated disk tokens: {delta}"
    );
    assert_ne!(
        delta["result"]["resultId"], full["result"]["resultId"],
        "the delta response must advance the client-visible resultId"
    );
    assert_eq!(
        original_data.len() % 5,
        0,
        "token quintets: {original_data:?}"
    );
}

#[test]
fn test_semantic_tokens_range_returns_only_the_requested_lines() {
    // Two entities; the request covers the second one's body only.
    let text = "\
first_focus = {
    id = first_focus
    cost = 10
}
second_focus = {
    id = second_focus
    cost = 20
}
";
    let rel = "common/national_focus/tree.txt";
    let mut server = editor_server(&[(rel, text)]);

    let resp = server.call_with_id(
        2,
        "textDocument/semanticTokens/range",
        json!({
            "textDocument": { "uri": server.uri(rel) },
            // Exclusive end on column 0: lines 5 and 6, not 7.
            "range": {
                "start": { "line": 5, "character": 0 },
                "end": { "line": 7, "character": 0 },
            },
        }),
    );
    server.stop();

    assert_eq!(resp["id"], 2, "got: {resp}");
    let data = resp["result"]["data"].as_array().expect("token data");
    let tokens = decode_semantic_tokens(data);
    println!("decoded (line, start, len, type, mods) = {tokens:#?}");

    assert!(!tokens.is_empty(), "range should still classify: {resp}");
    assert!(
        tokens.iter().all(|t| (5..=6).contains(&t.0)),
        "tokens outside the requested range: {tokens:#?}"
    );
    // `id`, `=`, `second_focus`, `cost`, `=`, `20`.
    assert_eq!(tokens.len(), 6, "{tokens:#?}");
}

#[test]
fn test_semantic_tokens_range_end_column_zero_is_exclusive() {
    // `first = 1` on line 0, `second = 2` on line 1. A range ending at 1:0
    // covers line 0 only.
    let text = "first = 1\nsecond = 2\n";
    let rel = "common/national_focus/tree.txt";
    let mut server = editor_server(&[(rel, text)]);

    let resp = server.call(
        "textDocument/semanticTokens/range",
        json!({
            "textDocument": { "uri": server.uri(rel) },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 1, "character": 0 },
            },
        }),
    );
    server.stop();

    let data = resp["result"]["data"].as_array().expect("token data");
    let tokens = decode_semantic_tokens(data);
    assert!(
        tokens.iter().all(|t| t.0 == 0),
        "line 1 is past the exclusive end: {tokens:#?}"
    );
    assert_eq!(tokens.len(), 3, "{tokens:#?}");
}

#[test]
fn test_semantic_tokens_skip_loc_and_cwt_files() {
    let files = [
        (
            "localisation/english/x_l_english.yml",
            "l_english:\n key:0 \"v\"\n",
        ),
        (
            "common/national_focus/tree.txt",
            "my_focus = {\n    id = my_focus\n}\n",
        ),
    ];
    let mut server = editor_server(&files);
    for (i, rel) in ["localisation/english/x_l_english.yml"].iter().enumerate() {
        let resp = server.call_with_id(
            30 + i as i64,
            "textDocument/semanticTokens/full",
            json!({ "textDocument": { "uri": server.uri(rel) } }),
        );
        assert!(
            resp["result"].is_null(),
            "{rel} should get no tokens: {resp}"
        );
    }
    server.stop();
}

#[test]
fn test_document_color_and_presentation_round_trip_both_conventions() {
    let text = "\
communism = {
    color = { 51 102 153 }
    dynamic_faction_names = { 0.2 0.4 0.6 }
}
";
    let rel = "common/ideologies/00_ideologies.txt";
    let mut server = editor_server(&[(rel, text)]);
    let doc_uri = server.uri(rel);

    let resp = server.call_with_id(
        2,
        "textDocument/documentColor",
        json!({ "textDocument": { "uri": doc_uri } }),
    );
    assert_eq!(resp["id"], 2, "got: {resp}");
    println!("documentColor result = {}", resp["result"]);
    let colours = resp["result"].as_array().expect("colour array").clone();
    assert_eq!(colours.len(), 2, "two colour leaves: {}", resp["result"]);

    // Swatches agree: `{ 51 102 153 }` and `{ 0.2 0.4 0.6 }` are the same colour.
    for c in &colours {
        let (r, g, b) = (
            c["color"]["red"].as_f64().unwrap(),
            c["color"]["green"].as_f64().unwrap(),
            c["color"]["blue"].as_f64().unwrap(),
        );
        assert!((r - 0.2).abs() < 0.01, "red {r} in {c}");
        assert!((g - 0.4).abs() < 0.01, "green {g} in {c}");
        assert!((b - 0.6).abs() < 0.01, "blue {b} in {c}");
        assert_eq!(c["color"]["alpha"], 1.0);
    }

    // The ranges must cover exactly the literal, not the key.
    let lines: Vec<&str> = text.lines().collect();
    let slice = |c: &Value| {
        let line = c["range"]["start"]["line"].as_u64().unwrap() as usize;
        let s = c["range"]["start"]["character"].as_u64().unwrap() as usize;
        let e = c["range"]["end"]["character"].as_u64().unwrap() as usize;
        lines[line].chars().skip(s).take(e - s).collect::<String>()
    };
    let mut spans: Vec<String> = colours.iter().map(slice).collect();
    spans.sort();
    assert_eq!(spans, vec!["{ 0.2 0.4 0.6 }", "{ 51 102 153 }"]);

    // colorPresentation must write back the convention it read. Ask each range
    // for the SAME colour it already holds and check the spelling survives.
    for (i, c) in colours.iter().enumerate() {
        let resp = server.call_with_id(
            10 + i as i64,
            "textDocument/colorPresentation",
            json!({
                "textDocument": { "uri": doc_uri },
                "color": c["color"],
                "range": c["range"],
            }),
        );
        println!("colorPresentation for {} = {}", slice(c), resp["result"]);
        let label = resp["result"][0]["label"].as_str().expect("a presentation");
        let edit = resp["result"][0]["textEdit"]["newText"]
            .as_str()
            .expect("a text edit");
        assert_eq!(label, edit);
        assert_eq!(resp["result"][0]["textEdit"]["range"], c["range"]);
        if slice(c).contains('.') {
            assert_eq!(edit, "{ 0.200 0.400 0.600 }", "floats stay floats");
        } else {
            assert_eq!(edit, "{ 51 102 153 }", "bytes stay bytes");
        }
    }
    server.stop();
}

#[test]
fn test_color_presentation_writes_a_new_pick_in_the_source_convention() {
    let text = "communism = {\n    color = { 51 102 153 }\n}\n";
    let rel = "common/ideologies/00_ideologies.txt";
    let mut server = editor_server(&[(rel, text)]);
    let doc_uri = server.uri(rel);

    // Pure orange picked in the editor over an int-convention literal.
    let resp = server.call(
        "textDocument/colorPresentation",
        json!({
            "textDocument": { "uri": doc_uri },
            "color": { "red": 1.0, "green": 0.5, "blue": 0.0, "alpha": 1.0 },
            "range": {
                "start": { "line": 1, "character": 12 },
                "end": { "line": 1, "character": 26 },
            },
        }),
    );
    server.stop();
    println!("colorPresentation (new pick) = {}", resp["result"]);
    assert_eq!(
        resp["result"][0]["textEdit"]["newText"], "{ 255 128 0 }",
        "an int-convention literal must not be rewritten as floats: {resp}"
    );
}

/// A server over `rel` (written to disk, no rules), told `initialized`.
fn format_server(rel: &str, text: &str) -> TestServer {
    let mut server = TestServer::builder().file(rel, text).spawn();
    server.initialized();
    server
}

fn formatting_params(uri: &str) -> Value {
    json!({
        "textDocument": { "uri": uri },
        "options": { "tabSize": 4, "insertSpaces": true },
    })
}

#[test]
fn test_formatting_rewrites_a_badly_indented_open_file() {
    let rel = "common/a.txt";
    let text = "root={\n a=1\n}\n";
    let mut server = format_server(rel, text);
    server.open(rel, text);
    let resp = server.call(
        "textDocument/formatting",
        formatting_params(&server.uri(rel)),
    );
    server.stop();
    assert!(resp.get("error").is_none(), "got: {resp}");
    let edits = resp["result"].as_array().expect("edits");
    assert!(!edits.is_empty(), "badly indented file must produce edits");
    let new_text = edits[0]["newText"].as_str().unwrap_or("");
    assert!(new_text.contains("root = {"), "{new_text}");
    assert!(new_text.contains("    a = 1"), "{new_text}");
}

#[test]
fn test_formatting_parse_error_returns_no_edits() {
    let rel = "common/bad.txt";
    let text = "root = {\n";
    let mut server = format_server(rel, text);
    server.open(rel, text);
    let resp = server.call(
        "textDocument/formatting",
        formatting_params(&server.uri(rel)),
    );
    server.stop();
    assert!(resp.get("error").is_none(), "got: {resp}");
    let empty = resp["result"].is_null() || resp["result"].as_array().is_some_and(|a| a.is_empty());
    assert!(
        empty,
        "parse error must not rewrite, got: {}",
        resp["result"]
    );
}

/// Frames up to the response for `id`, as (messages of `method` notifications
/// that mention an unclosed quote, the response).
fn unclosed_quote_notices_until_response(
    server: &mut TestServer,
    method: &str,
    id: i64,
) -> (Vec<String>, Value) {
    let mut notices = Vec::new();
    let response = server.read_until(&format!("the response to request {id}"), |frame| {
        notices.extend(unclosed_quote_notice(frame, method));
        frame["id"] == id && frame.get("method").is_none()
    });
    (notices, response)
}

fn unclosed_quote_notice(frame: &Value, method: &str) -> Option<String> {
    let message = frame["params"]["message"].as_str()?;
    (frame["method"] == method && message.contains("unclosed quote")).then(|| message.to_owned())
}

/// The matching notices still unread once the server has exited. The server
/// queues a notice before its response, but the two travel on separate
/// streams and can swap on the wire, so a count is only final at EOF.
fn unclosed_quote_notices_after_exit(server: &mut TestServer, method: &str) -> Vec<String> {
    server
        .read_to_end()
        .iter()
        .filter_map(|frame| unclosed_quote_notice(frame, method))
        .collect()
}

#[test]
fn test_formatting_names_the_unclosed_quote_it_refuses() {
    let rel = "common/names.txt";
    let text = "names = {\n\t\"Falke\n\tAdler\n}\n";
    let mut server = format_server(rel, text);
    server.open(rel, text);
    let uri = server.uri(rel);
    // Format on save repeats the request; the toast must not repeat with it.
    let mut rounds = Vec::new();
    for id in [2, 3] {
        server.send_request(id, "textDocument/formatting", formatting_params(&uri));
        rounds.push(unclosed_quote_notices_until_response(
            &mut server,
            "window/showMessage",
            id,
        ));
    }
    server.stop();
    let late = unclosed_quote_notices_after_exit(&mut server, "window/showMessage");
    for (_, resp) in &rounds {
        assert!(resp.get("error").is_none(), "got: {resp}");
        assert!(
            resp["result"].is_null(),
            "an unclosed leaf-value quote must not rewrite, got: {}",
            resp["result"]
        );
    }
    let toasts: Vec<&String> = rounds.iter().flat_map(|(n, _)| n).chain(&late).collect();
    assert_eq!(
        toasts.len(),
        1,
        "one toast for the refusal, not one per request: {toasts:?}"
    );
    assert!(
        toasts[0].starts_with("CWTools: did not format ")
            && toasts[0].ends_with("names.txt: unclosed quote on line 2."),
        "{toasts:?}"
    );
}

#[test]
fn test_format_workspace_counts_and_logs_an_unclosed_quote() {
    let rel = "common/names.txt";
    let original = "names = {\n\t\"Falke\n\tAdler\n}\n";
    let mut server = format_server(rel, original);
    server.send_request(
        2,
        "workspace/executeCommand",
        json!({ "command": "formatWorkspace", "arguments": [] }),
    );
    let (mut notices, resp) =
        unclosed_quote_notices_until_response(&mut server, "window/logMessage", 2);
    server.stop();
    notices.extend(unclosed_quote_notices_after_exit(
        &mut server,
        "window/logMessage",
    ));
    assert_eq!(
        resp["result"],
        "No files needed formatting; skipped 1 (unclosed quotes, listed in the output).",
        "got: {resp}"
    );
    assert_eq!(notices.len(), 1, "one log line for the file: {notices:?}");
    assert!(
        notices[0].ends_with("names.txt: unclosed quote on line 2"),
        "{notices:?}"
    );
    assert_eq!(std::fs::read_to_string(server.path(rel)).unwrap(), original);
}

#[test]
fn test_format_workspace_applies_one_edit() {
    let rel = "common/a.txt";
    let original = "root={\r\n name=\"café 😀\"\r\n}\r\n";
    let expected = "root = {\r\n    name = \"café 😀\"\r\n}\r\n";
    let mut server = format_server(rel, original);
    let uri = server.uri(rel);

    server.send_request(
        2,
        "workspace/executeCommand",
        json!({ "command": "formatWorkspace", "arguments": [] }),
    );
    let (resp, applied_edit) = server.read_response_answering_apply_edit(2);
    server.stop();
    assert!(
        resp["result"]
            .as_str()
            .is_some_and(|s| s.starts_with("Formatted ")),
        "got: {resp}"
    );
    let edit = applied_edit.expect("the server must call workspace/applyEdit");
    let changes = edit["changes"].as_object().expect("changes map");
    assert_eq!(changes.len(), 1, "one file touched: {edit}");
    let edits = changes
        .get(&uri)
        .unwrap_or_else(|| panic!("no edit for {uri}: {edit}"))
        .as_array()
        .unwrap_or_else(|| panic!("file edits must be an array: {edit}"));
    assert_eq!(edits.len(), 1, "one edit for the file: {edit}");
    let text_edit = &edits[0];
    assert_eq!(
        text_edit["range"],
        json!({
            "start": { "line": 0, "character": 0 },
            "end": { "line": 3, "character": 0 },
        }),
        "the edit must cover the whole CRLF-terminated document: {edit}"
    );
    let new_text = text_edit["newText"].as_str().expect("edit newText");
    assert_eq!(new_text, expected);
}

#[test]
fn test_format_workspace_emits_versioned_document_changes_when_supported() {
    // A client advertising workspace.workspaceEdit.documentChanges gets
    // TextDocumentEdits carrying versions: the open file at its buffer
    // version, a closed file with null, and no legacy changes map. The
    // startup scan finishes first so the didOpen publish below is the
    // readiness signal that the versioned buffer is registered; a snapshot
    // taken before it lands would silently fall back to disk (null version)
    // and the test would fail instead of passing by luck of the scheduler.
    let a_text = "root={\n a=1\n}\n";
    let mut server = TestServer::builder()
        .file("common/a.txt", a_text)
        .file("common/b.txt", "other={\n b=2\n}\n")
        .capabilities(json!({
            "workspace": { "workspaceEdit": { "documentChanges": true } }
        }))
        .spawn();
    server.initialized();
    server.wait_for_scan_done();
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": server.uri("common/a.txt"),
                "languageId": "hoi4",
                "version": 7,
                "text": a_text,
            }
        }),
    );
    server.wait_for_diagnostics("a.txt");
    server.send_request(
        2,
        "workspace/executeCommand",
        json!({ "command": "formatWorkspace", "arguments": [] }),
    );
    let (resp, applied_edit) = server.read_response_answering_apply_edit(2);
    server.stop();
    assert!(
        resp["result"]
            .as_str()
            .is_some_and(|s| s.starts_with("Formatted ")),
        "got: {resp}"
    );
    let edit = applied_edit.expect("the server must call workspace/applyEdit");
    assert!(
        edit.get("changes").is_none(),
        "a documentChanges client must not get the legacy changes map, got: {edit}"
    );
    let doc_changes = edit["documentChanges"]
        .as_array()
        .unwrap_or_else(|| panic!("documentChanges must be present, got: {edit}"));
    assert_eq!(doc_changes.len(), 2, "both files must be edited: {edit}");
    let entry_for = |suffix: &str| {
        doc_changes
            .iter()
            .find(|e| {
                e["textDocument"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with(suffix))
            })
            .unwrap_or_else(|| panic!("no documentChanges entry for {suffix}, got: {edit}"))
    };
    assert_eq!(
        entry_for("a.txt")["textDocument"]["version"],
        7,
        "the open file must carry its buffer version, got: {edit}"
    );
    assert_eq!(
        entry_for("b.txt")["textDocument"].get("version"),
        Some(&Value::Null),
        "the closed file must carry a null version, got: {edit}"
    );
}

#[test]
fn test_range_formatting_leaves_unselected_statements() {
    let rel = "common/a.txt";
    let text = "foo=1\nbar=2\n";
    let mut server = format_server(rel, text);
    server.open(rel, text);
    let resp = server.call(
        "textDocument/rangeFormatting",
        json!({
            "textDocument": { "uri": server.uri(rel) },
            "range": {
                "start": { "line": 1, "character": 0 },
                "end": { "line": 1, "character": 5 }
            },
            "options": { "tabSize": 4, "insertSpaces": true },
        }),
    );
    server.stop();
    assert!(resp.get("error").is_none(), "got: {resp}");
    let edits = resp["result"].as_array().expect("edits");
    assert_eq!(edits.len(), 1, "one statement: {resp}");
    let start_line = edits[0]["range"]["start"]["line"].as_u64().unwrap();
    assert_eq!(start_line, 1, "must not touch line 0: {resp}");
    let new_text = edits[0]["newText"].as_str().unwrap_or("");
    assert!(new_text.contains("bar = 2"), "{new_text}");
    assert!(!new_text.contains("foo"), "{new_text}");
}

#[test]
fn test_format_workspace_reports_a_discovery_failure() {
    let mut server = TestServer::builder().spawn_uninitialized();
    let missing = server.uri("no_such_workspace");
    server.initialize_with(json!({
        "processId": std::process::id(),
        "rootUri": missing,
        "capabilities": {},
    }));
    server.initialized();
    server.send_request(
        2,
        "workspace/executeCommand",
        json!({ "command": "formatWorkspace", "arguments": [] }),
    );
    let (resp, applied) = server.read_response_answering_apply_edit(2);
    server.stop();
    let msg = resp["result"].as_str().unwrap_or("");
    assert!(
        msg.contains("discovery failed"),
        "a failed walk must not look like a clean no-op: {resp}"
    );
    assert!(applied.is_none(), "no edit on a failed walk: {applied:?}");
}

#[test]
fn test_format_workspace_cancel_applies_nothing() {
    let signals = tempfile::tempdir().unwrap();
    let gate = signals.path().join("format-hold");
    let ready = signals.path().join("format-hold-ready");
    let mut server = TestServer::builder()
        .file("common/a.txt", "root={\n a=1\n}\n")
        .capabilities(json!({ "window": { "workDoneProgress": true } }))
        .env("CWTOOLS_FORMAT_HOLD_FILE", &gate)
        .env("CWTOOLS_FORMAT_HOLD_READY_FILE", &ready)
        .spawn();
    server.initialized();
    let startup_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(
            Instant::now() < startup_deadline,
            "startup scan never closed its loading bar"
        );
        let Some(v) = server.try_read_frame(Duration::from_millis(200)) else {
            continue;
        };
        if v["method"] == "window/workDoneProgress/create" {
            server.reply(&v);
        }
        if v["method"] == "loadingBar" && v["params"]["enable"] == Value::Bool(false) {
            break;
        }
    }
    std::fs::write(&gate, b"hold").unwrap();
    let token = "cwtools/format/cancel";
    server.send_request(
        2,
        "workspace/executeCommand",
        json!({
            "command": "formatWorkspace",
            "arguments": [],
            "workDoneToken": token,
        }),
    );

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut saw_begin = false;
    let mut result = None;
    let mut applied = false;
    while Instant::now() < deadline && result.is_none() {
        let Some(v) = server.try_read_frame(Duration::from_millis(200)) else {
            continue;
        };
        if v["method"] == "workspace/applyEdit" {
            applied = true;
            let reply = json!({
                "jsonrpc": "2.0",
                "id": v["id"],
                "result": { "applied": true },
            });
            server.send(&reply.to_string());
        }
        if v["method"].as_str() == Some(concat!("$", "/progress"))
            && v["params"]["token"] == token
            && v["params"]["value"]["kind"] == "begin"
            && !saw_begin
        {
            saw_begin = true;
            assert!(
                poll_until(
                    Instant::now() + Duration::from_secs(5),
                    Duration::from_millis(5),
                    || ready.exists().then_some(())
                )
                .is_some(),
                "format command never entered its test gate"
            );
            assert!(
                gate.exists(),
                "format hold gate released before cancellation"
            );
            server.notify("window/workDoneProgress/cancel", json!({ "token": token }));
        }
        if v.get("id") == Some(&json!(2)) {
            result = Some(v);
        }
    }
    server.stop();
    assert!(saw_begin, "progress never began");
    let resp = result.expect("the cancelled command never answered");
    let msg = resp["result"].as_str().unwrap_or("");
    assert!(msg.contains("Cancelled"), "cancel must not format: {resp}");
    assert!(!applied, "cancel must not send workspace/applyEdit");
}

#[test]
fn test_formatting_leaves_diagnostic_codes_unchanged() {
    const RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
"#;
    let rel = "common/decisions/test.txt";
    let text = "a={\nlimit={\n}\n}\n";
    let mut server = TestServer::builder()
        .rules_file("r.cwt", RULES)
        .vanilla()
        .file(rel, text)
        .spawn();
    server.initialized();
    server.wait_for_scan_done();
    server.open(rel, text);
    let before = server.wait_for_diags(rel);
    let before_codes: Vec<String> = before
        .iter()
        .filter_map(|d| d["code"].as_str().map(str::to_string))
        .collect();
    assert!(
        before_codes.iter().any(|c| c == "CW281"),
        "need a real diagnostic to prove format does not drop it: {before:?}"
    );

    let resp = server.call(
        "textDocument/formatting",
        formatting_params(&server.uri(rel)),
    );
    let formatted = resp["result"][0]["newText"]
        .as_str()
        .expect("whole-file newText")
        .to_string();
    assert_ne!(formatted, text, "fixture must actually need formatting");

    server.change(rel, 2, &formatted);
    let after = server.wait_for_diags(rel);
    server.stop();
    let after_codes: Vec<String> = after
        .iter()
        .filter_map(|d| d["code"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        before_codes, after_codes,
        "format must not move diagnostics"
    );
}

#[test]
fn test_semantic_tokens_refresh_waits_for_advertised_client_support() {
    let mut server = TestServer::builder()
        .file(
            "common/national_focus/f.txt",
            "outer = {\n    inner = {\n        x = 1\n    }\n}\n",
        )
        .capabilities(json!({
            "workspace": { "semanticTokens": { "refreshSupport": true } }
        }))
        .init_opts(json!({ "language": "hoi4" }))
        .deadline(Duration::from_secs(5))
        .spawn();
    server.initialized();

    let mut refreshed = false;
    let mut refreshed_by_scan_end = false;
    for _ in 0..5000 {
        let frame = server.read_frame();
        if frame["method"] == "workspace/semanticTokens/refresh" {
            server.reply(&frame);
            refreshed = true;
        }
        if frame["method"] == "loadingBar" && frame["params"]["enable"] == Value::Bool(false) {
            refreshed_by_scan_end = refreshed;
            break;
        }
    }

    assert!(
        refreshed_by_scan_end,
        "scan did not request semantic refresh"
    );
}
