use serde_json::{Value, json};

use crate::common::{COMPLETION_RULES, DYNAMIC_RULES, GOTO_RULES, TestServer, feature_request};

/// Minimal scripted-localisation rules used by the completion regression
/// test below.
const SCRIPTED_LOC_COMPLETION_RULES: &str = r#"
types = {
    type[scripted_loc] = {
        path = "game/common/scripted_loc"
    }
}
scripted_loc = {
    name = scalar
    text = {
        localization_key = scalar
        set_math = {
            value_set[variable] = math_expr
        }
    }
}
"#;

/// A server on `rules` with `extra_files` and the file at `rel_path` on disk and
/// opened, each one indexed before the next opens. The server is initialized
/// but not `initialized`: no workspace scan races the didOpen.
fn completion_server(
    rules: &str,
    rel_path: &str,
    text: &str,
    extra_files: &[(&str, &str)],
) -> TestServer {
    let files: Vec<(&str, &str)> = extra_files
        .iter()
        .copied()
        .chain([(rel_path, text)])
        .collect();
    let mut builder = TestServer::builder().rules(rules);
    for (rel, content) in &files {
        builder = builder.file(rel, *content);
    }
    let mut server = builder.spawn();
    for (rel, content) in &files {
        server.open_and_wait(rel, content);
    }
    server
}

/// The response to a completion request at (line0, char0) in `rel_path`.
fn request_completion(server: &mut TestServer, rel_path: &str, line0: u32, char0: u32) -> Value {
    let uri = server.uri(rel_path);
    let response = server.call(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line0, "character": char0 },
        }),
    );
    assert!(
        response.get("error").is_none(),
        "completion failed: {response}"
    );
    response
}

/// The items of a completion response, whether its result is a bare list or a
/// `CompletionList`. A `null` result is the server's "nothing to offer" reply,
/// which the no-dump tests expect; an error response never gets here, because
/// [`request_completion`] fails on it.
fn completion_items(response: &Value) -> &[Value] {
    let result = &response["result"];
    if result.is_null() {
        return &[];
    }
    result
        .as_array()
        .or_else(|| result["items"].as_array())
        .unwrap_or_else(|| panic!("no completion items in: {response}"))
}

/// Spawn a server with COMPLETION_RULES loaded, open `rel_path` with `text`,
/// request completion at (line0, char0), and return the completion labels.
fn completion_labels(rel_path: &str, text: &str, line0: u32, char0: u32) -> Vec<String> {
    completion_labels_custom_rules(COMPLETION_RULES, rel_path, text, &[], line0, char0)
}

fn completion_response(rel_path: &str, text: &str, line0: u32, char0: u32) -> Value {
    let mut server = completion_server(COMPLETION_RULES, rel_path, text, &[]);
    let response = request_completion(&mut server, rel_path, line0, char0);
    server.stop();
    response
}

/// Open `extra_files` (indexed on didOpen) then request completion in `text`
/// at (line0, char0); returns the labels.
fn completion_labels_with_files(
    rel_path: &str,
    text: &str,
    extra_files: &[(&str, &str)],
    line0: u32,
    char0: u32,
) -> Vec<String> {
    completion_labels_custom_rules(DYNAMIC_RULES, rel_path, text, extra_files, line0, char0)
}

/// Spawn a server with custom `rules` text, open `extra_files` + the main file,
/// and return the completion labels at `(line0, char0)`.
fn completion_labels_custom_rules(
    rules: &str,
    rel_path: &str,
    text: &str,
    extra_files: &[(&str, &str)],
    line0: u32,
    char0: u32,
) -> Vec<String> {
    let mut server = completion_server(rules, rel_path, text, extra_files);
    let response = request_completion(&mut server, rel_path, line0, char0);
    server.stop();
    completion_items(&response)
        .iter()
        .filter_map(|item| item["label"].as_str().map(str::to_string))
        .collect()
}

/// Like `completion_labels_custom_rules` but returns `(label, sortText)` pairs so
/// tests can assert scope-aware ranking, not just membership.
fn completion_items_custom_rules(
    rules: &str,
    rel_path: &str,
    text: &str,
    extra_files: &[(&str, &str)],
    line0: u32,
    char0: u32,
) -> Vec<(String, Option<String>)> {
    let mut server = completion_server(rules, rel_path, text, extra_files);
    let response = request_completion(&mut server, rel_path, line0, char0);
    server.stop();
    completion_items(&response)
        .iter()
        .filter_map(|item| {
            item["label"].as_str().map(|label| {
                (
                    label.to_string(),
                    item["sortText"].as_str().map(str::to_string),
                )
            })
        })
        .collect()
}

/// Like `completion_labels_with_files` but issues a `didChange` to `new_text`
/// (full-sync) before requesting completion, so the test exercises the
/// backspace-into-blank case end to end. `wait_after_change` controls whether
/// the test waits for the debounced validate to republish diagnostics before
/// requesting completion — pass `true` to land on the post-debounce AST,
/// `false` to land on the stale AST (the realistic mid-typing snapshot).
fn completion_labels_after_change(
    rel_path: &str,
    open_text: &str,
    extra_files: &[(&str, &str)],
    new_text: &str,
    line0: u32,
    char0: u32,
    wait_after_change: bool,
) -> Vec<String> {
    let mut server = completion_server(DYNAMIC_RULES, rel_path, open_text, extra_files);
    // Bump the version so the LSP accepts it. Full-sync: server ignores the
    // range and replaces the whole document text.
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": server.uri(rel_path), "version": 2 },
            "contentChanges": [{ "text": new_text }],
        }),
    );
    if wait_after_change {
        server.wait_for_diagnostics(rel_path);
    }
    let response = request_completion(&mut server, rel_path, line0, char0);
    server.stop();
    completion_items(&response)
        .iter()
        .filter_map(|item| item["label"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn test_completion_trigger_alias_in_allowed_block() {
    let text = "my_decision = {\n    allowed = {\n        \n    }\n    cost = 5\n}\n";
    // Cursor on the blank line inside `allowed = { ... }` (line 2, col 8).
    let labels = completion_labels("common/decisions/test.txt", text, 2, 8);
    assert!(
        labels.iter().any(|l| l == "has_completed_focus"),
        "trigger aliases should be offered inside allowed, got: {:?}",
        labels
    );
    assert!(labels.iter().any(|l| l == "always"), "got: {:?}", labels);
    // The sibling decision field `cost` lives one level up, not inside `allowed`.
    // It must not leak in, or context-awareness is broken.
    assert!(
        !labels.iter().any(|l| l == "cost"),
        "out-of-context field `cost` should not appear inside allowed, got: {:?}",
        labels
    );
}

#[test]
fn test_completion_resolve_fills_alias_documentation() {
    // perf/completion-responsiveness: the `### docs` comment on an alias is
    // deferred out of the initial completion response (payload shrink) and
    // recomputed by `completionItem/resolve` — see `completion::resolve`.
    // `always` carries a `### Always evaluates to true.` doc in COMPLETION_RULES.
    let rel_path = "common/decisions/test.txt";
    let text = "my_decision = {\n    allowed = {\n        \n    }\n    cost = 5\n}\n";
    let mut server = completion_server(COMPLETION_RULES, rel_path, text, &[]);
    // The server must advertise resolve support so the client knows to call
    // completionItem/resolve at all.
    assert_eq!(
        server.init_response()["result"]["capabilities"]["completionProvider"]["resolveProvider"],
        true,
        "server must advertise completion resolveProvider, got: {}",
        server.init_response()
    );

    // Blank line inside `allowed = { ... }` (line 2, col 8).
    let response = request_completion(&mut server, rel_path, 2, 8);
    let always = completion_items(&response)
        .iter()
        .find(|i| i["label"] == "always")
        .unwrap_or_else(|| panic!("`always` not offered, got: {}", response));
    // The initial response must NOT carry the deferred documentation — this
    // is the payload shrink the deferral exists for.
    assert!(
        always.get("documentation").is_none() || always["documentation"].is_null(),
        "documentation must be deferred out of the initial response, got: {}",
        always
    );
    assert!(
        !always["data"].is_null(),
        "a deferred item must carry `data` for resolve to key off, got: {}",
        always
    );

    // `completionItem/resolve`'s params ARE the completion item itself (no
    // wrapper), per the LSP spec.
    let resolved = server.call("completionItem/resolve", always.clone());
    server.stop();
    let doc = &resolved["result"]["documentation"];
    let doc_text = doc.as_str().or_else(|| doc["value"].as_str());
    assert_eq!(
        doc_text,
        Some("Always evaluates to true."),
        "resolve should repopulate the alias's ### doc, got: {}",
        resolved
    );
}

#[test]
fn test_completion_on_blank_line_after_field() {
    // Completing on a fresh line after `cost = 5` must offer the block's other
    // fields (the parser's leaf range absorbs the trailing blank line, which used
    // to resolve to the cost value and return nothing) (cwtools-vscode#20).
    let text = "my_decision = {\n    cost = 5\n    \n}\n";
    // Cursor on the blank line after `cost = 5` (line 2, col 4).
    let labels = completion_labels("common/decisions/test.txt", text, 2, 4);
    assert!(
        labels.iter().any(|l| l == "allowed"),
        "blank line after a field should offer sibling fields, got: {:?}",
        labels
    );
}

#[test]
fn test_small_context_completion_is_complete() {
    // Strategy A (perf/completion-responsiveness): a resolved-context list at
    // or under CONTEXT_COMPLETE_THRESHOLD is returned unfiltered and marked
    // `is_incomplete: false` — small enough that VS Code filters and
    // re-filters it client-side for free as the user keeps typing, with zero
    // further requests until a word boundary or trigger char forces a
    // re-query. (Large/filtered/fallback lists still must stay
    // `is_incomplete: true` — see test_completion_in_half_typed_state.)
    let resp = completion_response(
        "common/decisions/test.txt",
        "my_decision = {\n    cost = 5\n}\n",
        1,
        4,
    );
    let is_incomplete = resp["result"]["isIncomplete"]
        .as_bool()
        .or_else(|| resp["result"]["is_incomplete"].as_bool());
    assert_eq!(
        is_incomplete,
        Some(false),
        "a small resolved-context list must be marked complete, got: {}",
        resp
    );
}

#[test]
fn test_completion_after_change_with_stale_ast_stays_incomplete() {
    let rel_path = "common/decisions/test.txt";
    let initial_text = "my_decision = {\n    cost = 5\n    \n}\n";
    let changed_text = "my_decision = {\n    allowed = {\n        \n";
    let mut server = completion_server(COMPLETION_RULES, rel_path, initial_text, &[]);

    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": server.uri(rel_path), "version": 2 },
            "contentChanges": [{ "text": changed_text }]
        }),
    );

    let resp = request_completion(&mut server, rel_path, 2, 8);
    server.stop();
    let is_incomplete = resp["result"]["isIncomplete"]
        .as_bool()
        .or_else(|| resp["result"]["is_incomplete"].as_bool());
    assert_eq!(
        is_incomplete,
        Some(true),
        "completion resolved from a stale or dirty AST must stay incomplete, got: {}",
        resp
    );
}

#[test]
fn test_completion_in_half_typed_state() {
    // User scenario: type a partial block, walk away, come back, start
    // typing more. The parser fails on the partial text, so the last good
    // AST is None and `rules_at_pos` has nothing to walk. Completion must
    // still return SOMETHING (the cached fallback, the loc list, or a
    // re-parsed AST) and flag `is_incomplete` so the popup re-engages on
    // the next keystroke. Regression for the "super unresponsive when you
    // come back to a half-typed file" complaint.
    let text =
        "my_decision = {\n    allowed = {\n        has_completed_focus = \n    }\n    cost = 5\n";
    let resp = completion_response("common/decisions/test.txt", text, 3, 32);
    // Either a context-aware list (from the re-parsed AST) or the fallback
    // list (if even the re-parse failed) — both are valid, but the
    // response must not be empty and must be marked incomplete.
    let items = completion_items(&resp);
    assert!(
        !items.is_empty(),
        "half-typed state must return some completions, got: {}",
        resp
    );
    let is_incomplete = resp["result"]["isIncomplete"]
        .as_bool()
        .or_else(|| resp["result"]["is_incomplete"].as_bool());
    assert_eq!(
        is_incomplete,
        Some(true),
        "half-typed completion must be marked is_incomplete, got: {}",
        resp
    );
}

#[test]
fn test_completion_items_carry_text_edit_anchor() {
    // Every item must carry an explicit `textEdit` so the client filters and
    // inserts against the cursor range instead of guessing a word boundary (the
    // guess breaks right after a backspace across `=` / `<` / `>`). Blank-line
    // key position inside the decision block (line 2, col 4): the range anchors
    // at the cursor and `filterText` is pinned to the label. (Non-empty token
    // ranges are covered by paths::test_current_token_range.)
    let text = "my_decision = {\n    cost = 5\n    \n}\n";
    let resp = completion_response("common/decisions/test.txt", text, 2, 4);
    let items = completion_items(&resp);
    assert!(!items.is_empty(), "expected items, got: {}", resp);
    let allowed = items
        .iter()
        .find(|i| i["label"] == "allowed")
        .unwrap_or_else(|| panic!("`allowed` not offered, got: {}", resp));
    let range = &allowed["textEdit"]["range"];
    assert_eq!(range["start"]["line"], 2, "got: {}", allowed);
    assert_eq!(
        range["start"]["character"], 4,
        "replace range must anchor at the cursor token, got: {}",
        allowed
    );
    assert_eq!(range["end"]["character"], 4, "got: {}", allowed);
    // filterText is pinned to the label so the client never filters a snippet.
    assert_eq!(allowed["filterText"], "allowed", "got: {}", allowed);
}

#[test]
fn test_completion_offers_mathexpr_operators_in_math_block() {
    // Inside a `math_expr` block (`set_math = { x = { | } }`), completion must
    // offer `value` and the registered mathexpr operators (add/subtract/…),
    // resolved by the position descent into the synthesized math-clause rules —
    // not the flat fallback.
    let text = "d = {\n    set_math = {\n        x = {\n            \n        }\n    }\n}\n";
    // Blank line inside the innermost math block `x = { ... }` (line 3, col 12).
    let labels = completion_labels("common/decisions/test.txt", text, 3, 12);
    assert!(
        labels.iter().any(|l| l == "add") && labels.iter().any(|l| l == "subtract"),
        "math operators should be offered inside a math block, got: {:?}",
        labels
    );
    assert!(
        labels.iter().any(|l| l == "value"),
        "`value` base should be offered inside a math block, got: {:?}",
        labels
    );
}

#[test]
fn test_completion_math_block_value_excludes_effects() {
    // Value position after `add = ` inside a math block (line 3, col 18).
    let text = "d = {\n    set_math = {\n        x = {\n            add = \n        }\n    }\n}\n";
    let labels = completion_labels("common/decisions/test.txt", text, 3, 18);
    assert!(
        !labels.iter().any(|l| l == "add_political_power"),
        "effects must not appear at math value position, got: {:?}",
        labels
    );
}

#[test]
fn test_completion_math_leaf_value_excludes_effects() {
    // Value position after `x = ` at the set_variable level (line 2, col 12).
    let text = "d = {\n    set_math = {\n        x = \n    }\n}\n";
    let labels = completion_labels("common/decisions/test.txt", text, 2, 12);
    assert!(
        !labels.iter().any(|l| l == "add_political_power"),
        "effects must not appear at math leaf value position, got: {:?}",
        labels
    );
}

#[test]
fn test_cwt_completion_replaces_partial_constructs() {
    for (text, line, character, label, start) in [
        ("alias[", 0, 6, "alias", 0),
        ("types = {\n  type[", 1, 7, "type", 2),
        ("enums = {\n  enum[", 1, 7, "enum", 2),
        ("rule = filepath[", 0, 16, "filepath[folder,extension]", 7),
    ] {
        let response = completion_response("config/test.cwt", text, line, character);
        assert_eq!(response["result"]["isIncomplete"], true, "{response}");
        let item = response["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["label"] == label)
            .unwrap_or_else(|| panic!("missing {label}: {response}"));
        assert_eq!(item["textEdit"]["range"]["start"]["character"], start);
        assert_eq!(item["textEdit"]["range"]["end"]["character"], character);
    }
}

#[test]
fn test_localisation_completion_range_uses_utf16_columns() {
    let text = "l_english:\n KNOWN_KEY:0 \"known\"\n TEST:0 \"😀 $key\"";
    let response = completion_response("localisation/test_l_english.yml", text, 2, 16);
    let items = response["result"]
        .as_array()
        .or_else(|| response["result"]["items"].as_array())
        .unwrap_or_else(|| panic!("missing completion items: {response}"));
    let item = items
        .iter()
        .find(|item| item["label"] == "known_key")
        .unwrap_or_else(|| panic!("missing loc key: {response}"));
    assert_eq!(item["textEdit"]["range"]["start"]["character"], 13);
    assert_eq!(item["textEdit"]["range"]["end"]["character"], 16);
}

#[test]
fn test_localisation_completion_range_uses_utf32_columns() {
    let text = "l_english:\n KNOWN_KEY:0 \"known\"\n TEST:0 \"😀 $key\"";
    let files = &[("localisation/test_l_english.yml", text)];
    let caps = serde_json::json!({
        "general": { "positionEncodings": ["utf-32"] }
    });
    let result = feature_request(
        COMPLETION_RULES,
        files,
        &["localisation/test_l_english.yml"],
        caps,
        "localisation/test_l_english.yml",
        "textDocument/completion",
        serde_json::json!({ "position": { "line": 2, "character": 15 } }),
    );
    let items = result
        .as_array()
        .or_else(|| result["items"].as_array())
        .unwrap_or_else(|| panic!("missing completion items: {result}"));
    let item = items
        .iter()
        .find(|item| item["label"] == "known_key")
        .unwrap_or_else(|| panic!("missing loc key: {result}"));
    assert_eq!(item["textEdit"]["range"]["start"]["character"], 12);
    assert_eq!(item["textEdit"]["range"]["end"]["character"], 15);
}

#[test]
fn test_completion_skips_resource_files() {
    let response = completion_response("gfx/interface/test.dds", "anything", 0, 8);
    assert!(response["result"].is_null(), "{response}");
}

#[test]
fn test_completion_modifiers_in_mio_equipment_bonus() {
    let text = "my_org = {\n    name = org\n    equipment_bonus = {\n        some_equipment = {\n            \n        }\n    }\n}\n";
    // Cursor on the blank line inside the equipment block (line 4, col 12).
    let labels = completion_labels(
        "common/military_industrial_organization/organizations/test.txt",
        text,
        4,
        12,
    );
    assert!(
        labels.iter().any(|l| l == "build_cost_ic"),
        "modifier names should be offered inside an equipment_bonus entry, got: {:?}",
        labels
    );
    // `name` is a top-level mio field, not a modifier; it must not leak into the
    // equipment entry's modifier completions.
    assert!(
        !labels.iter().any(|l| l == "name"),
        "out-of-context field `name` should not appear inside an equipment entry, got: {:?}",
        labels
    );
}

const SCRIPT_ENUMS: (&str, &str) = (
    "common/script_enums.txt",
    "script_enum_equipment_stat = {\n\tbuild_cost_ic\n\treliability\n\tsoft_attack\n}\n",
);

#[test]
fn test_completion_equipment_stats_in_mio_trait_bonus() {
    // The real MIO shape: equipment_bonus keyed by the equipment_stat complex
    // enum, collected from common/script_enums.txt.
    let text = "my_org = {\n    name = org\n    trait = {\n        token = t1\n        equipment_bonus = {\n            \n        }\n    }\n}\n";
    // Cursor on the blank line inside equipment_bonus (line 5, col 12).
    let labels = completion_labels_with_files(
        "common/military_industrial_organization/organizations/test.txt",
        text,
        &[SCRIPT_ENUMS],
        5,
        12,
    );
    assert!(
        labels.iter().any(|l| l == "build_cost_ic"),
        "equipment stats should be offered, got: {:?}",
        labels
    );
    assert!(
        labels.iter().any(|l| l == "soft_attack"),
        "got: {:?}",
        labels
    );
    assert!(labels.iter().any(|l| l == "instant"), "got: {:?}", labels);
}

#[test]
fn test_completion_focus_keys_after_clause_subblock() {
    // Cursor on the blank line AFTER `completion_reward = { ... }` must offer
    // focus-level keys (id, x, y, cost, …), not the effects inside the sub-block.
    // Regression: parser extends completion_reward's range past `}`, causing
    // descend() to enter the sub-block and return effect aliases.
    let text = "my_focus = {\n    completion_reward = {\n        add_political_power = 5\n    }\n    \n}\n";
    // Line 4, col 4: blank line after `}` of completion_reward, still inside focus.
    let labels = completion_labels("common/national_focus/test.txt", text, 4, 4);
    assert!(
        labels.iter().any(|l| l == "id"),
        "focus keys should be offered after a clause sub-block, got: {:?}",
        labels
    );
    assert!(
        labels.iter().any(|l| l == "cost"),
        "focus keys should be offered after a clause sub-block, got: {:?}",
        labels
    );
    assert!(
        !labels.iter().any(|l| l == "add_political_power"),
        "effect from sub-block must not leak into focus context, got: {:?}",
        labels
    );
}

#[test]
fn test_completion_focus_effects_inside_clause_subblock() {
    // Cursor on the blank line INSIDE `completion_reward = { }` must offer
    // effects, not focus-level keys.
    let text = "my_focus = {\n    completion_reward = {\n        \n    }\n}\n";
    // Line 2, col 8: blank line inside completion_reward.
    let labels = completion_labels("common/national_focus/test.txt", text, 2, 8);
    assert!(
        labels.iter().any(|l| l == "add_political_power"),
        "effects should be offered inside completion_reward, got: {:?}",
        labels
    );
    assert!(
        !labels.iter().any(|l| l == "id"),
        "focus key `id` must not appear inside completion_reward, got: {:?}",
        labels
    );
}

#[test]
fn test_completion_event_keys_after_clause_subblock() {
    // Cursor after `trigger = { ... }` inside an event block must offer event-level
    // keys (id, title, immediate, option, …), not trigger aliases.
    let text = "my_event = {\n    trigger = {\n        always = yes\n    }\n    \n}\n";
    // Line 4, col 4: blank line after `}` of trigger, still inside event.
    let labels = completion_labels("events/test.txt", text, 4, 4);
    assert!(
        labels.iter().any(|l| l == "title"),
        "event keys should be offered after a clause sub-block, got: {:?}",
        labels
    );
    assert!(
        labels.iter().any(|l| l == "immediate"),
        "event keys should be offered after a clause sub-block, got: {:?}",
        labels
    );
    assert!(
        !labels.iter().any(|l| l == "always"),
        "trigger alias must not leak into event context, got: {:?}",
        labels
    );
}

#[test]
fn test_completion_idea_names_for_has_idea() {
    // has_idea = | offers idea names collected via the idea_name complex enum.
    let ideas = (
        "common/ideas/test_ideas.txt",
        "ideas = {\n\tcountry = {\n\t\tmy_test_idea = {\n\t\t\tcost = 1\n\t\t}\n\t}\n}\n",
    );
    let text = "my_decision = {\n    allowed = {\n        has_idea = \n    }\n    cost = 5\n}\n";
    // Cursor right after `has_idea = ` (line 2, col 19).
    let labels = completion_labels_with_files("common/decisions/test.txt", text, &[ideas], 2, 19);
    assert!(
        labels.iter().any(|l| l == "my_test_idea"),
        "idea names should be offered for has_idea, got: {:?}",
        labels
    );
}

#[test]
fn test_completion_country_flags_for_has_country_flag() {
    // Flags written by set_country_flag anywhere in the workspace are offered
    // for has_country_flag = |.
    let setter = (
        "common/decisions/setter.txt",
        "other_decision = {\n    complete_effect = {\n        set_country_flag = my_war_flag\n    }\n    cost = 1\n}\n",
    );
    let text =
        "my_decision = {\n    allowed = {\n        has_country_flag = \n    }\n    cost = 5\n}\n";
    // Cursor right after `has_country_flag = ` (line 2, col 27).
    let labels = completion_labels_with_files("common/decisions/test.txt", text, &[setter], 2, 27);
    assert!(
        labels.iter().any(|l| l == "my_war_flag"),
        "collected country flags should be offered, got: {:?}",
        labels
    );
}

#[test]
fn test_completion_country_flags_merged_from_two_other_files() {
    // #237: the value set merges contributions from every file in the
    // workspace, not just the first one found. Two other files each set a
    // different flag; both must be offered together.
    let setter_a = (
        "common/decisions/setter_a.txt",
        "decision_a = {\n    complete_effect = {\n        set_country_flag = flag_from_a\n    }\n    cost = 1\n}\n",
    );
    let setter_b = (
        "common/decisions/setter_b.txt",
        "decision_b = {\n    complete_effect = {\n        set_country_flag = flag_from_b\n    }\n    cost = 1\n}\n",
    );
    let text =
        "my_decision = {\n    allowed = {\n        has_country_flag = \n    }\n    cost = 5\n}\n";
    // Cursor right after `has_country_flag = ` (line 2, col 27).
    let labels = completion_labels_with_files(
        "common/decisions/test.txt",
        text,
        &[setter_a, setter_b],
        2,
        27,
    );
    assert!(
        labels.iter().any(|l| l == "flag_from_a"),
        "flag set in one other file should be offered, got: {:?}",
        labels
    );
    assert!(
        labels.iter().any(|l| l == "flag_from_b"),
        "flag set in a second other file should be offered, got: {:?}",
        labels
    );
}

#[test]
fn test_completion_focus_int_value_no_variable_dump() {
    // #75: at a focus `x = ` (typed `int`), completion must offer NOTHING rather
    // than dumping every saved variable. A variable is seeded in another file so
    // the old fallback would have surfaced it.
    let vars = (
        "common/decisions/vars.txt",
        "seed_dec = {\n    set_math = {\n        my_saved_var = 5\n    }\n}\n",
    );
    // Control: the seeded variable IS offered at a math value position, proving it
    // is indexed — so the absence below is the guard working, not an empty index.
    let control = completion_labels_custom_rules(
        COMPLETION_RULES,
        "common/decisions/test.txt",
        "d = {\n    set_math = {\n        foo = \n    }\n}\n",
        std::slice::from_ref(&vars),
        2,
        14,
    );
    assert!(
        control.iter().any(|l| l == "my_saved_var"),
        "control: seeded variable must be indexed and offered at a math value position, got: {:?}",
        control
    );
    // The fix: at focus `x = ` (int), no variable dump.
    let labels = completion_labels_custom_rules(
        COMPLETION_RULES,
        "common/national_focus/test.txt",
        "my_focus = {\n    x = \n}\n",
        &[vars],
        1,
        8,
    );
    assert!(
        !labels.iter().any(|l| l == "my_saved_var"),
        "focus int value must not dump saved variables, got: {:?}",
        labels
    );
}

const LOCALISATION_RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
    type[focus] = { path = "game/common/national_focus" }
}
decision = {
    set_math = {
        value_set[variable] = math_expr
        value_set[variable] = scalar
    }
    loc_name = localisation
}
focus = {
    id = scalar
}
"#;

#[test]
fn test_completion_scripted_loc_offers_outer_fields_without_variable_dump() {
    let vars = (
        "common/decisions/vars.txt",
        "seed_dec = {\n    set_math = {\n        my_saved_var = 5\n    }\n}\n",
    );

    let name_labels = completion_labels_custom_rules(
        SCRIPTED_LOC_COMPLETION_RULES,
        "common/scripted_loc/test.txt",
        "defined_text = {\n    n\n}\n",
        &[vars],
        1,
        5,
    );
    assert!(
        name_labels.iter().any(|label| label == "name"),
        "scripted loc outer fields should offer name, got: {:?}",
        name_labels
    );
    assert!(
        !name_labels.iter().any(|label| label == "my_saved_var"),
        "scripted loc outer fields must not dump variables, got: {:?}",
        name_labels
    );

    let text_labels = completion_labels_custom_rules(
        SCRIPTED_LOC_COMPLETION_RULES,
        "common/scripted_loc/test.txt",
        "defined_text = {\n    t\n}\n",
        &[vars],
        1,
        5,
    );
    assert!(
        text_labels.iter().any(|label| label == "text"),
        "scripted loc outer fields should offer text, got: {:?}",
        text_labels
    );
    assert!(
        !text_labels.iter().any(|label| label == "my_saved_var"),
        "scripted loc outer fields must not dump variables, got: {:?}",
        text_labels
    );

    let nested_labels = completion_labels_custom_rules(
        SCRIPTED_LOC_COMPLETION_RULES,
        "common/scripted_loc/test.txt",
        "defined_text = {\n    text = {\n        \n    }\n}\n",
        &[vars],
        2,
        8,
    );
    assert!(
        nested_labels
            .iter()
            .any(|label| label == "localization_key"),
        "scripted loc text block should keep its own fields, got: {:?}",
        nested_labels
    );
}

#[test]
fn test_completion_localisation_value_offers_keys_not_variable_dump() {
    // #74: a `localisation`-typed value position must offer indexed loc keys,
    // not the flat variable dump.
    let vars = (
        "common/decisions/vars.txt",
        "seed_dec = {\n    set_math = {\n        my_saved_var = 5\n    }\n}\n",
    );
    let loc = (
        "localisation/test_l_english.yml",
        "l_english:\n MY_FOCUS:0 \"A focus\"\n",
    );
    let labels = completion_labels_custom_rules(
        LOCALISATION_RULES,
        "common/decisions/test.txt",
        "my_dec = {\n    loc_name = \n}\n",
        &[vars, loc],
        1,
        15,
    );
    assert!(
        labels.iter().any(|l| l == "my_focus"),
        "localisation value should offer indexed loc keys, got: {:?}",
        labels
    );
    assert!(
        !labels.iter().any(|l| l == "my_saved_var"),
        "localisation value must not dump saved variables, got: {:?}",
        labels
    );
}

const TWO_OVERLOAD_FLAG_RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
decision = {
    allowed = {
        alias_name[trigger] = alias_match_left[trigger]
    }
    complete_effect = {
        alias_name[effect] = alias_match_left[effect]
    }
    set_math = {
        value_set[variable] = math_expr
        value_set[variable] = scalar
    }
    cost = int
}
alias[trigger:has_country_flag] = value[country_flag]
alias[trigger:has_country_flag] = {
    flag = value[country_flag]
    ## cardinality = 0..1
    days = int
}
alias[effect:set_country_flag] = value_set[country_flag]
alias[effect:set_country_flag] = {
    flag = value_set[country_flag]
    ## cardinality = 0..1
    days = int
}
"#;

#[test]
fn test_completion_has_country_flag_two_overloads_no_dump() {
    // #79: with both a value and a block overload of has_country_flag /
    // set_country_flag declared, the value-form flag set must still resolve, and
    // an empty flag set must NOT fall back to the generic variable dump.
    let setter = (
        "common/decisions/setter.txt",
        "other = {\n    complete_effect = {\n        set_country_flag = my_war_flag\n    }\n    cost = 1\n}\n",
    );
    let flag_read =
        "my_decision = {\n    allowed = {\n        has_country_flag = \n    }\n    cost = 5\n}\n";

    // Flags still resolve past the two-overload interaction.
    let with_flag = completion_labels_custom_rules(
        TWO_OVERLOAD_FLAG_RULES,
        "common/decisions/test.txt",
        flag_read,
        std::slice::from_ref(&setter),
        2,
        27,
    );
    assert!(
        with_flag.iter().any(|l| l == "my_war_flag"),
        "two overloads: collected country flags must still resolve, got: {:?}",
        with_flag
    );

    // Empty flag set (no setter) + a seeded variable: the reader must offer
    // neither the (absent) flag nor the variable dump.
    let vars = (
        "common/decisions/vars.txt",
        "seed_dec = {\n    set_math = {\n        my_saved_var = 5\n    }\n}\n",
    );
    let empty_set = completion_labels_custom_rules(
        TWO_OVERLOAD_FLAG_RULES,
        "common/decisions/test.txt",
        flag_read,
        &[vars],
        2,
        27,
    );
    assert!(
        !empty_set.iter().any(|l| l == "my_saved_var"),
        "empty flag set must not dump saved variables, got: {:?}",
        empty_set
    );
}

/// Rules mirroring the real HOI4 MIO shapes for the #76/#78 tests: a `mio:` scope
/// link (links.cwt), the MIO scope (scopes.cwt), a MIO-category and a country-only
/// modifier (modifiers.cwt + modifier_categories.cwt), and effect aliases split by
/// `## scope`. The `equipment_bonus` block `## push_scope`s into the MIO scope so
/// its modifier completions resolve against `military_industrial_organization`.
const MIO_SCOPE_RULES: &str = r#"
types = {
    type[military_industrial_organization] = {
        path = "game/common/military_industrial_organization/organizations"
    }
    type[scripted_effect] = {
        path = "game/common/scripted_effects"
    }
    type[decision] = {
        path = "game/common/decisions"
    }
    type[country_tag] = {
        path = "game/common/country_tags"
    }
}
links = {
    mio = {
        prefix = mio:
        output_scope = military_industrial_organization
        input_scopes = country
        from_data = yes
        data_source = <military_industrial_organization>
    }
    country_ref = {
        output_scope = country
        input_scopes = country
        from_data = yes
        data_source = <country_tag>
    }
}
scopes = {
    Country = {
        aliases = { country }
    }
    "Military Industrial Organizations" = {
        aliases = { military_industrial_organization }
    }
}
modifiers = {
    military_industrial_organization_funds_gain = military_industrial_organization
    war_support_factor = country
}
modifier_categories = {
    military_industrial_organization = {
        supported_scopes = { military_industrial_organization }
    }
    country = {
        supported_scopes = { country }
    }
}
decision = {
    complete_effect = {
        alias_name[effect] = alias_match_left[effect]
    }
}
military_industrial_organization = {
    name = scalar
    ## push_scope = military_industrial_organization
    equipment_bonus = {
        alias_name[modifier] = alias_match_left[modifier]
    }
}
scripted_effect = {
    alias_name[effect] = alias_match_left[effect]
}
alias[effect:scope_field] = { alias_name[effect] = alias_match_left[effect] }
### MIO-scope effect
## scope = military_industrial_organization
alias[effect:add_mio_funds] = int
### Country-only effect
## scope = country
alias[effect:add_political_power] = int
"#;

const MIO_INSTANCE: (&str, &str) = (
    "common/military_industrial_organization/organizations/orgs.txt",
    "MY_ORG = {\n    name = org\n}\n",
);

#[test]
fn test_completion_scope_link_keys_in_effect_block() {
    // #76: at a key position inside an effect block, the `mio:` scope-switch key
    // is offered per MIO instance (`mio:MY_ORG = { … }`). The scope_field alias
    // makes the block accept a scope switch; the key was never suggested before.
    let text = "my_dec = {\n    complete_effect = {\n        \n    }\n}\n";
    let country_tags = ("common/country_tags/tags.txt", "GER = {\n}\n");
    let labels = completion_labels_custom_rules(
        MIO_SCOPE_RULES,
        "common/decisions/d.txt",
        text,
        &[MIO_INSTANCE, country_tags],
        2,
        8,
    );
    assert!(
        labels.iter().any(|l| l == "mio:MY_ORG"),
        "scope-link key mio:MY_ORG must be offered in an effect block, got: {:?}",
        labels
    );
    // The prefix-less `<country_tag>` from-data link must NOT be flooded in as a
    // bare scope-switch key: a raw country tag is high-cardinality and rarely the
    // way a scope switch is completed (#76 wanted only the prefixed keys).
    assert!(
        !labels.iter().any(|l| l == "GER"),
        "bare country-tag scope-link key must not flood the list, got: {:?}",
        labels
    );
}

#[test]
fn test_completion_effects_scope_filtered_in_mio_block() {
    // #78 layer 1: inside `mio:MY_ORG = { … }` the current scope is
    // military_industrial_organization. A MIO-scope effect must appear and rank in
    // the top bucket; a country-only effect must NOT be dropped (scope tracking is
    // imperfect) but de-ranked into the bottom bucket, behind the matching one.
    let text = "my_dec = {\n    complete_effect = {\n        mio:MY_ORG = {\n            \n        }\n    }\n}\n";
    let items = completion_items_custom_rules(
        MIO_SCOPE_RULES,
        "common/decisions/d.txt",
        text,
        &[MIO_INSTANCE],
        3,
        12,
    );
    let labels: Vec<&str> = items.iter().map(|(l, _)| l.as_str()).collect();
    let mio_effect = items.iter().find(|(l, _)| l == "add_mio_funds");
    let country_effect = items.iter().find(|(l, _)| l == "add_political_power");
    assert!(
        mio_effect.is_some(),
        "MIO-scope effect must be offered inside a MIO block, got: {:?}",
        labels
    );
    let mio_sort = mio_effect.and_then(|(_, s)| s.clone());
    assert!(
        mio_sort.as_deref().is_some_and(|s| s.starts_with("0_")),
        "MIO-scope effect must rank in the top bucket, got sortText: {:?}",
        mio_sort
    );
    assert!(
        country_effect.is_some(),
        "country-only effect must NOT be dropped for scope mismatch, got: {:?}",
        labels
    );
    let country_sort = country_effect.and_then(|(_, s)| s.clone());
    assert!(
        country_sort.as_deref().is_some_and(|s| s.starts_with("z_")),
        "scope-mismatched effect must sink to the bottom bucket, got sortText: {:?}",
        country_sort
    );
    assert!(
        mio_sort < country_sort,
        "MIO-scope effect must sort ahead of the mismatched one, got {:?} vs {:?}",
        mio_sort,
        country_sort
    );
}

#[test]
fn test_completion_modifiers_scope_filtered_in_mio_block() {
    // #78 layer 2: inside a `## push_scope`d MIO `equipment_bonus` block, the
    // MIO-category modifier ranks top and the country-category one is de-ranked to
    // the bottom bucket (not dropped) — the modifier→category→supported_scopes
    // plumbing at work.
    let text = "my_org = {\n    name = org\n    equipment_bonus = {\n        \n    }\n}\n";
    let items = completion_items_custom_rules(
        MIO_SCOPE_RULES,
        "common/military_industrial_organization/organizations/test.txt",
        text,
        &[],
        3,
        8,
    );
    let labels: Vec<&str> = items.iter().map(|(l, _)| l.as_str()).collect();
    let mio_mod = items
        .iter()
        .find(|(l, _)| l == "military_industrial_organization_funds_gain");
    let country_mod = items.iter().find(|(l, _)| l == "war_support_factor");
    assert!(
        mio_mod.is_some(),
        "MIO-category modifier must be offered in a MIO-scope modifier block, got: {:?}",
        labels
    );
    let mio_sort = mio_mod.and_then(|(_, s)| s.clone());
    assert!(
        mio_sort.as_deref().is_some_and(|s| s.starts_with("0_")),
        "MIO-category modifier must rank in the top bucket, got sortText: {:?}",
        mio_sort
    );
    assert!(
        country_mod.is_some(),
        "country-category modifier must NOT be dropped for scope mismatch, got: {:?}",
        labels
    );
    let country_sort = country_mod.and_then(|(_, s)| s.clone());
    assert!(
        country_sort.as_deref().is_some_and(|s| s.starts_with("z_")),
        "scope-mismatched modifier must sink to the bottom bucket, got sortText: {:?}",
        country_sort
    );
    assert!(
        mio_sort < country_sort,
        "MIO-category modifier must sort ahead of the mismatched one, got {:?} vs {:?}",
        mio_sort,
        country_sort
    );
}

#[test]
fn test_completion_effects_unfiltered_when_scope_unknown() {
    // #78 regression guard: a scripted_effects file is scope-agnostic (SCOPE_ANY),
    // so no scope filtering applies — both the MIO-scope and country-only effects
    // must still be listed.
    let text = "my_se = {\n    \n}\n";
    let labels = completion_labels_custom_rules(
        MIO_SCOPE_RULES,
        "common/scripted_effects/se.txt",
        text,
        &[],
        1,
        4,
    );
    assert!(
        labels.iter().any(|l| l == "add_mio_funds"),
        "MIO-scope effect must still be listed when scope is unknown, got: {:?}",
        labels
    );
    assert!(
        labels.iter().any(|l| l == "add_political_power"),
        "country-only effect must still be listed when scope is unknown, got: {:?}",
        labels
    );
}

/// Rules for the #64 and #66 integration tests.
const SCRIPTED_EFFECT_RULES: &str = r#"
types = {
    type[scripted_effect] = {
        path = "game/common/scripted_effects"
    }
    type[decision] = {
        path = "game/common/decisions"
    }
}
decision = {
    complete_effect = {
        alias_name[effect] = alias_match_left[effect]
    }
}
alias[effect:<scripted_effect>] = yes
scripted_effect = {
    alias_name[effect] = alias_match_left[effect]
}
"#;

#[test]
fn test_completion_scripted_effects_in_effect_block() {
    // #64: `alias[effect:<scripted_effect>] = yes` means every scripted_effect
    // instance must appear as a KEYWORD completion inside effect blocks. The bug
    // was that the raw placeholder `<scripted_effect>` appeared instead of actual
    // instance names.
    let se_file = (
        "common/scripted_effects/my_effects.txt",
        "my_special_effect = {\n}\n",
    );
    // Blank line inside `complete_effect = { }` of the decision.
    let text = "my_dec = {\n    complete_effect = {\n        \n    }\n}\n";
    let labels = completion_labels_custom_rules(
        SCRIPTED_EFFECT_RULES,
        "common/decisions/d.txt",
        text,
        &[se_file],
        2,
        8,
    );
    assert!(
        labels.iter().any(|l| l == "my_special_effect"),
        "scripted_effect instances must be offered in effect blocks, got: {:?}",
        labels
    );
    assert!(
        !labels.iter().any(|l| l == "<scripted_effect>"),
        "raw placeholder must not appear in labels, got: {:?}",
        labels
    );
}

/// Rules for the #65 integration test: a `dynamic_modifier` type whose body uses
/// `alias_keys_field[modifier]` as the key pattern.
const DYNAMIC_MODIFIER_RULES: &str = r#"
types = {
    type[dynamic_modifier] = {
        path = "game/common/dynamic_modifiers"
    }
}
modifiers = {
    build_cost_ic = economy
    production_speed_factor = economy
}
dynamic_modifier = {
    ## cardinality = 0..inf
    alias_keys_field[modifier] = float
}
"#;

#[test]
fn test_completion_alias_keys_field_in_dynamic_modifier() {
    // #65: a block whose children use `alias_keys_field[modifier]` as their key
    // (common/dynamic_modifiers/*.txt in HOI4) must offer modifier names.
    let text = "my_dmod = {\n    \n}\n";
    let labels = completion_labels_custom_rules(
        DYNAMIC_MODIFIER_RULES,
        "common/dynamic_modifiers/test.txt",
        text,
        &[],
        1,
        4,
    );
    assert!(
        labels.iter().any(|l| l == "build_cost_ic"),
        "modifier names must be offered inside a dynamic_modifier block, got: {:?}",
        labels
    );
    assert!(
        labels.iter().any(|l| l == "production_speed_factor"),
        "all modifier keys must be offered, got: {:?}",
        labels
    );
}

#[test]
fn test_completion_value_deleted_then_reoffered_keeps_context() {
    // User scenario: a decision's `allowed` block has `has_country_flag = my_war_flag`
    // with a working completion. They backspace the value, leaving
    // `has_country_flag = ` (or shorter). The same flag set must still be
    // offered — the block context (`allowed = { ... }`) hasn't changed, only
    // the value text has. The flat variable dump must NOT be substituted.
    let setter = (
        "common/decisions/setter.txt",
        "other_decision = {\n    complete_effect = {\n        set_country_flag = my_war_flag\n    }\n    cost = 1\n}\n",
    );
    let open_text = "my_decision = {\n    allowed = {\n        has_country_flag = my_war_flag\n    }\n    cost = 5\n}\n";
    // After backspacing the value, the cursor is right after `= ` on line 2.
    let new_text =
        "my_decision = {\n    allowed = {\n        has_country_flag = \n    }\n    cost = 5\n}\n";

    // Post-debounce (the AST has caught up to the new text): same flag set.
    let labels_post = completion_labels_after_change(
        "common/decisions/test.txt",
        open_text,
        std::slice::from_ref(&setter),
        new_text,
        2,
        27,
        true,
    );
    assert!(
        labels_post.iter().any(|l| l == "my_war_flag"),
        "post-debounce: backspaced value must still offer my_war_flag, got: {:?}",
        labels_post
    );

    // Mid-debounce (the AST is still the open_text one — the user is typing
    // fast and the next completion request arrives before the 250ms validate
    // has fired). Same expected behavior: the context-aware list, not the
    // generic variable dump.
    let labels_mid = completion_labels_after_change(
        "common/decisions/test.txt",
        open_text,
        &[setter],
        new_text,
        2,
        27,
        false,
    );
    assert!(
        labels_mid.iter().any(|l| l == "my_war_flag"),
        "mid-debounce: backspaced value must still offer my_war_flag, got: {:?}",
        labels_mid
    );
}

#[test]
fn test_completion_label_details_carry_type_origin() {
    // A client advertising labelDetailsSupport sees the type origin next to
    // instance labels at build time (not deferred to resolve).
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        ("common/decisions/d.txt", "adec = {\n    has_focus = \n}\n"),
    ];
    let caps = serde_json::json!({
        "textDocument": { "completion": { "completionItem": { "labelDetailsSupport": true } } }
    });
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/d.txt"],
        caps,
        "common/decisions/d.txt",
        "textDocument/completion",
        serde_json::json!({ "position": { "line": 1, "character": 16 } }),
    );
    let items = result["items"].as_array().expect("completion items");
    let focus_item = items
        .iter()
        .find(|i| i["label"] == "MY_FOCUS")
        .unwrap_or_else(|| panic!("MY_FOCUS must be offered, got: {}", result));
    assert_eq!(
        focus_item["labelDetails"]["description"], "focus",
        "type origin as labelDetails, got: {}",
        result
    );
}

#[test]
fn test_completion_label_details_absent_without_support() {
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        ("common/decisions/d.txt", "adec = {\n    has_focus = \n}\n"),
    ];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/d.txt"],
        serde_json::json!({}),
        "common/decisions/d.txt",
        "textDocument/completion",
        serde_json::json!({ "position": { "line": 1, "character": 16 } }),
    );
    let items = result["items"].as_array().expect("completion items");
    let focus_item = items
        .iter()
        .find(|i| i["label"] == "MY_FOCUS")
        .unwrap_or_else(|| panic!("MY_FOCUS must be offered, got: {}", result));
    assert!(
        focus_item["labelDetails"].is_null(),
        "no labelDetails without client support, got: {}",
        result
    );
}
