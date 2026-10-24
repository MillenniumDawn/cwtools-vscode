use std::time::Duration;

use serde_json::{Value, json};

use crate::common::{
    GOTO_RULES, TestServer, feature_builder, feature_request, feature_request_once, feature_start,
    loc, locations, poll_until_attempts, watched_created,
};

#[test]
fn test_loc_outline_shows_keys_in_yml() {
    let yml = "l_english:\n my_key:0 \"Hello\"\n my_key_desc:0 \"Desc\"\n my_other:0 \"World\"\n";
    let files = &[("localisation/test_l_english.yml", yml)];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["localisation/test_l_english.yml"],
        serde_json::json!({"textDocument": {"documentSymbol": {"hierarchicalDocumentSymbolSupport": false}}}),
        "localisation/test_l_english.yml",
        "textDocument/documentSymbol",
        serde_json::json!({}),
    );
    let syms = result.as_array().expect("loc outline array");
    let names: Vec<String> = syms
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        names.contains(&"my_key".to_string()),
        "my_key in {:?}",
        names
    );
    assert!(
        names.contains(&"my_key_desc".to_string()),
        "my_key_desc in {:?}",
        names
    );
    assert!(
        names.contains(&"my_other".to_string()),
        "my_other in {:?}",
        names
    );
}

#[test]
fn test_loc_references_find_script_usages() {
    let yml = "l_english:\n my_loc_key:0 \"Hello\"\n";
    let script = "generated = {\n    title = my_loc_key\n    desc = my_loc_key\n}\n";
    let files = &[
        ("localisation/test_l_english.yml", yml),
        ("common/test/event.txt", script),
    ];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["localisation/test_l_english.yml", "common/test/event.txt"],
        serde_json::json!({}),
        "localisation/test_l_english.yml",
        "textDocument/references",
        serde_json::json!({
            "position": {"line": 1, "character": 2},
            "context": {"includeDeclaration": false}
        }),
    );
    let locs = result.as_array().expect("references array");
    assert!(!locs.is_empty(), "expected script usages, got null/empty");
    let has_script = locs
        .iter()
        .any(|l| l["uri"].as_str().unwrap().contains("event.txt"));
    assert!(has_script, "script usage not found in {:?}", result);
}

#[test]
fn test_loc_rename_with_desc_sibling() {
    let yml =
        "l_english:\n my_item:0 \"Item\"\n my_item_desc:0 \"Desc\"\n my_item_tooltip:0 \"Tip\"\n";
    let script = "thing = {\n    name = my_item\n    tooltip = my_item_tooltip\n}\n";
    let files = &[
        ("localisation/test_l_english.yml", yml),
        ("common/test/thing.txt", script),
    ];
    // Prepare rename range
    let prep = feature_request(
        GOTO_RULES,
        files,
        &["localisation/test_l_english.yml"],
        serde_json::json!({}),
        "localisation/test_l_english.yml",
        "textDocument/prepareRename",
        serde_json::json!({"position": {"line": 1, "character": 2}}),
    );
    assert!(
        !prep.is_null(),
        "prepareRename for loc key must not be null, got {:?}",
        prep
    );
    // Rename
    let resp = rename_loc_key(files, "localisation/test_l_english.yml", "new_item");
    let edit = &resp["result"];
    assert!(
        !edit.is_null(),
        "rename result must not be null, got {}",
        resp
    );
    let (_, new_texts) = rename_edits(edit);
    assert!(
        new_texts.contains(&"new_item".to_string()),
        "new_item in edit, got {:?}",
        new_texts
    );
    assert!(
        new_texts.contains(&"new_item_desc".to_string()),
        "sibling _desc renamed, got {:?}",
        new_texts
    );
    assert!(
        new_texts.contains(&"new_item_tooltip".to_string()),
        "sibling _tooltip renamed, got {:?}",
        new_texts
    );
    assert!(
        !new_texts.iter().any(|s| s.contains("extra")),
        "unexpected extra sibling, got {:?}",
        new_texts
    );
}

#[test]
fn test_loc_rename_preserves_unrelated_same_spelling_tokens() {
    let rules = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
    type[scripted_effect] = { path = "game/common/scripted_effects" }
}
decision = {
    name = localisation
    literal = scalar
    effect = <scripted_effect>
    set_country_flag = scalar
    set_variable = { var = scalar value = float }
}
scripted_effect = { add_political_power = float }
"#;
    let files = &[
        (
            "localisation/test_l_english.yml",
            "l_english:\n shared_key:0 \"Display\"\n other:0 \"$shared_key$\"\n",
        ),
        (
            "localisation/test_l_french.yml",
            "l_french:\n shared_key:0 \"Affichage\"\n",
        ),
        (
            "common/decisions/test.txt",
            "choice = {\n    name = shared_key\n    literal = \"shared_key\"\n    effect = shared_key\n    set_country_flag = shared_key\n    set_variable = { var = shared_key value = 1 }\n}\n",
        ),
        (
            "common/scripted_effects/test.txt",
            "shared_key = { add_political_power = 1 }\n",
        ),
    ];
    let result = feature_request_once(
        rules,
        files,
        &[files[0].0],
        json!({}),
        files[0].0,
        "textDocument/rename",
        json!({ "position": { "line": 1, "character": 2 }, "newName": "display_key" }),
    );
    let changes = result["changes"].as_object().expect("rename changes");
    let edits_for = |rel: &str| {
        changes
            .iter()
            .find(|(uri, _)| uri.ends_with(rel))
            .map(|(_, edits)| edits.as_array().expect("text edits"))
    };
    assert_eq!(edits_for(files[0].0).unwrap().len(), 2, "{result}");
    assert_eq!(edits_for(files[1].0).unwrap().len(), 1, "{result}");
    let script_edits = edits_for(files[2].0).expect("localisation reference edit");
    assert!(
        script_edits.iter().any(|edit| {
            edit["range"]["start"] == json!({ "line": 1, "character": 11 })
                && edit["range"]["end"] == json!({ "line": 1, "character": 21 })
                && edit["newText"] == "display_key"
        }),
        "genuine localisation reference must be renamed: {result}"
    );
    assert_eq!(
        script_edits.len(),
        1,
        "rename must preserve unrelated strings, effects, flags and variables: {result}"
    );
    assert!(
        edits_for(files[3].0).is_none(),
        "rename must preserve the unrelated scripted effect definition: {result}"
    );
}

#[test]
fn test_loc_rename_updates_coupled_entity_ids_and_typed_references() {
    let rules = r#"
types = {
    type[focus] = { path = "game/common/national_focus" name_field = id }
    type[idea] = {
        path = "game/common/ideas"
        localisation = {
            name = "idea_$"
            desc = "idea_$_desc"
        }
    }
    type[decision] = { path = "game/common/decisions" }
    type[scripted_effect] = { path = "game/common/scripted_effects" }
}
focus = { id = localisation }
idea = { enabled = bool }
decision = { has_focus = <focus> has_idea = <idea> effect = <scripted_effect> literal = scalar }
scripted_effect = { enabled = bool }
"#;
    for (key, new_key, definition, reference, expected_definition, expected_reference) in [
        (
            "shared_key",
            "display_key",
            "common/national_focus/test.txt",
            "has_focus",
            "focus = { id = display_key }\n",
            "display_key",
        ),
        (
            "idea_shared_key_desc",
            "idea_display_key_desc",
            "common/ideas/test.txt",
            "has_idea",
            "display_key = { enabled = yes }\n",
            "display_key",
        ),
    ] {
        let en = format!("l_english:\n {key}:0 \"Display\"\n other:0 \"${key}$\"\n");
        let fr = format!("l_french:\n {key}:0 \"Affichage\"\n");
        let script = format!(
            "choice = {{ {reference} = shared_key effect = shared_key literal = \"shared_key\" }}\n"
        );
        let files = [
            ("localisation/test_l_english.yml", en.as_str()),
            ("localisation/test_l_french.yml", fr.as_str()),
            (
                "common/national_focus/test.txt",
                "focus = { id = shared_key }\n",
            ),
            ("common/ideas/test.txt", "shared_key = { enabled = yes }\n"),
            ("common/decisions/test.txt", script.as_str()),
            (
                "common/scripted_effects/test.txt",
                "shared_key = { enabled = yes }\n",
            ),
        ];
        let result = feature_request_once(
            rules,
            &files,
            &[files[0].0],
            json!({}),
            files[0].0,
            "textDocument/rename",
            json!({ "position": { "line": 1, "character": 2 }, "newName": new_key }),
        );
        let rewritten = crate::navigation::apply_rename_changes(&result, &files);
        assert_eq!(rewritten[definition], expected_definition, "{result}");
        assert_eq!(
            rewritten[files[4].0],
            format!(
                "choice = {{ {reference} = {expected_reference} effect = shared_key literal = \"shared_key\" }}\n"
            ),
            "{result}"
        );
        assert!(!rewritten.contains_key(files[5].0), "{result}");
        assert_eq!(
            rewritten[files[0].0],
            format!("l_english:\n {new_key}:0 \"Display\"\n other:0 \"${new_key}$\"\n"),
            "{result}"
        );
        assert_eq!(
            rewritten[files[1].0],
            format!("l_french:\n {new_key}:0 \"Affichage\"\n"),
            "{result}"
        );
    }
}

#[test]
fn test_loc_rename_classifies_quoted_list_and_explicit_binding_references() {
    let rules = r#"
types = {
    type[decision] = {
        path = "game/common/decisions"
        localisation = { label = title }
    }
}
decision = {
    title = scalar
    quoted = localisation
    names = { localisation }
    literal = scalar
}
"#;
    let files = [
        (
            "localisation/test_l_english.yml",
            "l_english:\n shared_key:0 \"Display\"\n",
        ),
        (
            "common/decisions/test.txt",
            "choice = { title = shared_key quoted = \"SHARED_KEY\" names = { shared_key } literal = \"shared_key\" }\n",
        ),
    ];
    let result = feature_request_once(
        rules,
        &files,
        &[files[0].0],
        json!({}),
        files[0].0,
        "textDocument/rename",
        json!({ "position": { "line": 1, "character": 2 }, "newName": "display_key" }),
    );
    let rewritten = crate::navigation::apply_rename_changes(&result, &files);
    assert_eq!(
        rewritten[files[1].0],
        "choice = { title = display_key quoted = \"display_key\" names = { display_key } literal = \"shared_key\" }\n",
        "{result}"
    );
}

#[test]
fn test_loc_rename_refuses_conflicting_keys_entities_and_bindings() {
    let rules = r#"
types = {
    type[focus] = { path = "game/common/national_focus" name_field = id }
    type[idea] = {
        path = "game/common/ideas"
        localisation = { first = "$_x" second = "x_$" }
    }
    type[per_file] = {
        path = "game/common/per_file"
        type_per_file = yes
        localisation = { name = "$" }
    }
}
focus = { id = localisation }
idea = { enabled = bool }
per_file = { enabled = bool }
"#;
    for (loc_text, script_path, script, replacement, expected_reason) in [
        (
            "l_english:\n shared_key:0 \"Display\"\n display_key:0 \"Existing\"\n",
            "common/national_focus/test.txt",
            "focus = { id = shared_key }\n",
            "display_key",
            "localisation key 'display_key' already exists",
        ),
        (
            "l_english:\n shared_key:0 \"Display\"\n",
            "common/national_focus/test.txt",
            "focus = { id = shared_key }\nfocus = { id = display_key }\n",
            "display_key",
            "focus 'display_key' already exists",
        ),
        (
            "l_english:\n x_x:0 \"Display\"\n",
            "common/ideas/test.txt",
            "x = { enabled = yes }\n",
            "new_x",
            "rule-defined prefix and suffix",
        ),
        (
            "l_english:\n shared_key:0 \"Display\"\n",
            "localisation/hidden_l_french.yml",
            "l_french:\n display_key:0 \"Existing\"\n",
            "display_key",
            "localisation key 'display_key' already exists",
        ),
        (
            "l_english:\n shared_key:0 \"Display\"\n",
            "common/per_file/shared_key.txt",
            "enabled = yes\n",
            "display_key",
            "filename-derived entities require a file rename",
        ),
    ] {
        let files = [
            ("localisation/test_l_english.yml", loc_text),
            (script_path, script),
        ];
        let mut server = feature_start(
            feature_builder(rules, &files, json!({})),
            &files,
            &[files[0].0],
        );
        let uri = server.uri(files[0].0);
        let response = server.call(
            "textDocument/rename",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": 1, "character": 2 },
                "newName": replacement,
            }),
        );
        server.stop();
        assert_eq!(response["error"]["code"], -32803, "{response}");
        assert!(
            response["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(expected_reason)),
            "{response}"
        );
        assert!(response.get("result").is_none(), "{response}");
    }
}

#[test]
fn test_loc_rename_uses_unsaved_coupled_documents_and_negotiated_positions() {
    let rules = r#"
types = {
    type[focus] = { path = "game/common/national_focus" name_field = id }
    type[decision] = { path = "game/common/decisions" }
}
focus = { id = localisation }
decision = { has_focus = <focus> literal = scalar }
"#;
    let files = [
        (
            "localisation/test_l_english.yml",
            "l_english:\n shared_key:0 \"Display\"\n",
        ),
        (
            "common/national_focus/test.txt",
            "focus = { id = shared_key }\n",
        ),
        (
            "common/decisions/test.txt",
            "choice = { has_focus = shared_key }\n",
        ),
    ];
    let prefix = "choice = { literal = \"😀\" has_focus = ";
    for (encoding, column) in [
        ("utf-16", prefix.encode_utf16().count()),
        ("utf-32", prefix.chars().count()),
    ] {
        let caps = json!({
            "general": { "positionEncodings": [encoding] },
            "workspace": { "workspaceEdit": { "documentChanges": true } },
        });
        let mut server = feature_start(
            feature_builder(rules, &files, caps),
            &files,
            &[files[0].0, files[1].0, files[2].0],
        );
        server.change(
            files[1].0,
            2,
            "# unsaved\nfocus = { id = \"shared_key\" }\n",
        );
        server.change(
            files[2].0,
            2,
            &format!("# unsaved\n{prefix}shared_key }}\n"),
        );
        let uri = server.uri(files[0].0);
        let response = server.call(
            "textDocument/rename",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": 1, "character": 2 },
                "newName": "display_key",
            }),
        );
        server.stop();
        let docs = response["result"]["documentChanges"]
            .as_array()
            .expect("versioned rename edits");
        assert_eq!(docs.len(), 3, "{response}");
        for (rel, start, end) in [
            (files[1].0, 15, 27),
            (files[2].0, column, column + "shared_key".len()),
        ] {
            let doc = docs
                .iter()
                .find(|doc| doc["textDocument"]["uri"].as_str().unwrap().ends_with(rel))
                .unwrap();
            assert_eq!(doc["textDocument"]["version"], 2, "{response}");
            assert_eq!(doc["edits"].as_array().unwrap().len(), 1, "{response}");
            assert_eq!(
                doc["edits"][0]["range"],
                json!({
                    "start": { "line": 1, "character": start },
                    "end": { "line": 1, "character": end },
                }),
                "{response}"
            );
            assert_eq!(doc["edits"][0]["newText"], "display_key", "{response}");
        }
    }
}

#[test]
fn test_loc_rename_coupled_references_respect_nested_rule_context() {
    let rules = r#"
types = {
    type[focus] = { path = "game/common/national_focus" name_field = id }
    type[decision] = { path = "game/common/decisions" }
}
focus = { id = localisation }
decision = { has_focus = <focus> nested = { has_focus = scalar } }
"#;
    let files = [
        (
            "localisation/test_l_english.yml",
            "l_english:\n shared_key:0 \"Display\"\n",
        ),
        (
            "common/national_focus/test.txt",
            "focus = { id = shared_key }\n",
        ),
        (
            "common/decisions/test.txt",
            "choice = { has_focus = shared_key nested = { has_focus = shared_key } }\n",
        ),
    ];
    let result = feature_request_once(
        rules,
        &files,
        &[files[0].0],
        json!({}),
        files[0].0,
        "textDocument/rename",
        json!({ "position": { "line": 1, "character": 2 }, "newName": "display_key" }),
    );
    let rewritten = crate::navigation::apply_rename_changes(&result, &files);
    assert_eq!(
        rewritten[files[2].0],
        "choice = { has_focus = display_key nested = { has_focus = shared_key } }\n",
        "{result}"
    );
}

#[test]
fn test_loc_rename_follows_active_subtype_bindings_and_alias_key_references() {
    let rules = r#"
types = {
    type[scripted_effect] = {
        path = "game/common/scripted_effects"
        subtype[named] = { enabled = yes }
        localisation = {
            subtype[named] = { name = "$" }
        }
    }
    type[decision] = { path = "game/common/decisions" }
}
scripted_effect = { enabled = bool }
decision = {
    effect = <scripted_effect.named>
    complete_effect = { alias_name[effect] = alias_match_left[effect] }
    ordinary = scalar
}
alias[effect:<scripted_effect.named>] = yes
"#;
    let files = [
        (
            "localisation/test_l_english.yml",
            "l_english:\n shared_key:0 \"Display\"\n",
        ),
        (
            "common/scripted_effects/test.txt",
            "shared_key = { enabled = yes }\n",
        ),
        (
            "common/decisions/test.txt",
            "choice = { effect = shared_key complete_effect = { shared_key = yes } ordinary = shared_key }\n",
        ),
    ];
    let result = feature_request_once(
        rules,
        &files,
        &[files[0].0],
        json!({}),
        files[0].0,
        "textDocument/rename",
        json!({ "position": { "line": 1, "character": 2 }, "newName": "display_key" }),
    );
    let rewritten = crate::navigation::apply_rename_changes(&result, &files);
    assert_eq!(
        rewritten[files[1].0], "display_key = { enabled = yes }\n",
        "{result}"
    );
    assert_eq!(
        rewritten[files[2].0],
        "choice = { effect = display_key complete_effect = { display_key = yes } ordinary = shared_key }\n",
        "{result}"
    );
}

#[test]
fn test_loc_rename_refuses_a_read_only_coupled_entity() {
    let rules = r#"
types = {
    type[idea] = { path = "game/common/ideas" localisation = { name = "$" } }
    type[decision] = { path = "game/common/decisions" }
}
idea = { enabled = bool }
decision = { idea = <idea> }
"#;
    let files = [
        (
            "localisation/test_l_english.yml",
            "l_english:\n shared_key:0 \"Display\"\n",
        ),
        (
            "common/decisions/test.txt",
            "choice = { idea = shared_key }\n",
        ),
    ];
    let builder = feature_builder(rules, &files, json!({}))
        .vanilla_file("common/ideas/test.txt", "shared_key = { enabled = yes }\n");
    let mut server = feature_start(builder, &files, &[files[0].0, files[1].0]);
    let definition_path = std::fs::canonicalize(server.vanilla_dir().join("common/ideas/test.txt"))
        .expect("the base-game definition exists");
    let reference_uri = server.uri(files[1].0);
    assert!(
        poll_until_attempts(40, Duration::from_millis(25), || {
            let result = server.request(
                "textDocument/definition",
                json!({
                    "textDocument": { "uri": reference_uri },
                    "position": { "line": 0, "character": 20 },
                }),
            );
            locations(&result)
                .iter()
                .any(|(uri, _)| {
                    tower_lsp::lsp_types::Url::parse(uri)
                        .ok()
                        .and_then(|url| url.to_file_path().ok())
                        .and_then(|path| std::fs::canonicalize(path).ok())
                        .as_ref()
                        == Some(&definition_path)
                })
                .then_some(())
        })
        .is_some(),
        "the base-game entity must be indexed before rename"
    );
    let uri = server.uri(files[0].0);
    let response = server.call(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 2 },
            "newName": "display_key",
        }),
    );
    server.stop();
    assert_eq!(response["error"]["code"], -32803, "{response}");
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("only edits files in the workspace folders")),
        "{response}"
    );
    assert!(response.get("result").is_none(), "{response}");
}

#[test]
fn test_loc_rename_expands_rule_defined_localisation_siblings() {
    let rules = r#"
types = {
    type[idea] = {
        path = "game/common/ideas"
        localisation = { name = "$" flavour = "prefix_$_flavour" }
    }
    type[decision] = { path = "game/common/decisions" }
}
idea = { enabled = bool }
decision = { name = localisation }
"#;
    let files = [
        (
            "localisation/test_l_english.yml",
            "l_english:\n shared_key:0 \"Display\"\n prefix_shared_key_flavour:0 \"$shared_key$\"\n",
        ),
        ("common/ideas/test.txt", "shared_key = { enabled = yes }\n"),
        (
            "common/decisions/test.txt",
            "choice = { name = prefix_shared_key_flavour }\n",
        ),
    ];
    let result = feature_request_once(
        rules,
        &files,
        &[files[0].0],
        json!({}),
        files[0].0,
        "textDocument/rename",
        json!({ "position": { "line": 1, "character": 2 }, "newName": "display_key" }),
    );
    let rewritten = crate::navigation::apply_rename_changes(&result, &files);
    assert_eq!(
        rewritten[files[0].0],
        "l_english:\n display_key:0 \"Display\"\n prefix_display_key_flavour:0 \"$display_key$\"\n",
        "{result}"
    );
    assert_eq!(
        rewritten[files[2].0], "choice = { name = prefix_display_key_flavour }\n",
        "{result}"
    );
}

#[test]
fn test_loc_rename_does_not_start_from_an_unrelated_literal() {
    let files = [
        (
            "localisation/test_l_english.yml",
            "l_english:\n shared_key:0 \"Display\"\n",
        ),
        (
            "common/characters/test.txt",
            "character = { name = \"shared_key\" }\n",
        ),
    ];
    for method in ["textDocument/prepareRename", "textDocument/rename"] {
        let result = feature_request_once(
            GOTO_RULES,
            &files,
            &[files[1].0],
            json!({}),
            files[1].0,
            method,
            json!({ "position": { "line": 0, "character": 24 }, "newName": "display_key" }),
        );
        assert!(result.is_null(), "{method}: {result}");
    }
}

#[test]
fn test_loc_outline_hierarchical_vs_flat() {
    let yml = "l_english:\n aaa:0 \"A\"\n bbb:0 \"B\"\n";
    let files = &[("localisation/test_l_english.yml", yml)];
    // Flat
    let flat = feature_request(
        GOTO_RULES,
        files,
        &["localisation/test_l_english.yml"],
        serde_json::json!({"textDocument": {"documentSymbol": {"hierarchicalDocumentSymbolSupport": false}}}),
        "localisation/test_l_english.yml",
        "textDocument/documentSymbol",
        serde_json::json!({}),
    );
    let flat_arr = flat.as_array().expect("flat array");
    assert_eq!(
        flat_arr.len(),
        2,
        "flat outline should have 2 symbols, got {:?}",
        flat
    );
    assert!(
        flat_arr[0]["kind"].is_number(),
        "SymbolInformation must have kind"
    );
    // Hierarchical
    let hierarchical = feature_request(
        GOTO_RULES,
        files,
        &["localisation/test_l_english.yml"],
        serde_json::json!({"textDocument": {"documentSymbol": {"hierarchicalDocumentSymbolSupport": true}}}),
        "localisation/test_l_english.yml",
        "textDocument/documentSymbol",
        serde_json::json!({}),
    );
    let hier_arr = hierarchical.as_array().expect("hierarchical array");
    assert_eq!(
        hier_arr.len(),
        2,
        "hierarchical outline should have 2 symbols, got {:?}",
        hierarchical
    );
    assert!(
        hier_arr[0]["selectionRange"].is_object(),
        "DocumentSymbol must have selectionRange"
    );
}

#[test]
fn test_loc_outline_outside_localisation_is_empty() {
    let yml = "l_english:\n my_key:0 \"Hello\"\n";
    // File under .github is has_loc_ext but not is_loc_file, so outline should be empty (no game AST)
    let files = &[(".github/workflows/ci.yml", yml)];
    let result = feature_request_once(
        GOTO_RULES,
        files,
        &[".github/workflows/ci.yml"],
        serde_json::json!({"textDocument": {"documentSymbol": {"hierarchicalDocumentSymbolSupport": false}}}),
        ".github/workflows/ci.yml",
        "textDocument/documentSymbol",
        serde_json::json!({}),
    );
    assert!(
        result.is_null() || result.as_array().map(|a| a.is_empty()).unwrap_or(false),
        "outside localisation dir should have no outline, got {:?}",
        result
    );
}

#[test]
fn test_loc_references_include_declaration_flag() {
    let yml = "l_english:\n my_key:0 \"Hello\"\n";
    let script = "x = {\n    title = my_key\n}\n";
    let files = &[
        ("localisation/test_l_english.yml", yml),
        ("common/test/event.txt", script),
    ];
    let with_decl = feature_request(
        GOTO_RULES,
        files,
        &["localisation/test_l_english.yml", "common/test/event.txt"],
        serde_json::json!({}),
        "localisation/test_l_english.yml",
        "textDocument/references",
        serde_json::json!({"position": {"line": 1, "character": 2}, "context": {"includeDeclaration": true}}),
    );
    let locs_true = with_decl.as_array().expect("refs with decl");
    assert!(
        locs_true
            .iter()
            .any(|l| l["uri"].as_str().unwrap().contains("test_l_english.yml")),
        "includeDeclaration true must include yml definition, got {:?}",
        with_decl
    );
    let without_decl = feature_request(
        GOTO_RULES,
        files,
        &["localisation/test_l_english.yml", "common/test/event.txt"],
        serde_json::json!({}),
        "localisation/test_l_english.yml",
        "textDocument/references",
        serde_json::json!({"position": {"line": 1, "character": 2}, "context": {"includeDeclaration": false}}),
    );
    let locs_false = without_decl.as_array().expect("refs without decl");
    assert!(
        !locs_false
            .iter()
            .any(|l| l["uri"].as_str().unwrap().contains("test_l_english.yml")),
        "includeDeclaration false must not include yml definition, got {:?}",
        without_decl
    );
    assert!(
        locs_false
            .iter()
            .any(|l| l["uri"].as_str().unwrap().contains("event.txt")),
        "script usage must still be present, got {:?}",
        without_decl
    );
}

#[test]
fn test_loc_references_case_insensitive_and_whole_token() {
    let yml = "l_english:\n my_key:0 \"Hello\"\n";
    // MY_KEY different case should match, my_key_extra should NOT, comment should NOT
    let script = "x = {\n    title = MY_KEY\n    other = my_key_extra\n    # my_key comment\n    quoted = \"my_key\"\n}\n";
    let files = &[
        ("localisation/test_l_english.yml", yml),
        ("common/test/event.txt", script),
    ];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["localisation/test_l_english.yml", "common/test/event.txt"],
        serde_json::json!({}),
        "localisation/test_l_english.yml",
        "textDocument/references",
        serde_json::json!({"position": {"line": 1, "character": 2}, "context": {"includeDeclaration": false}}),
    );
    let locs = result.as_array().expect("refs");
    // Filter to event.txt locations
    let script_locs: Vec<_> = locs
        .iter()
        .filter(|l| l["uri"].as_str().unwrap().contains("event.txt"))
        .collect();
    // Should find MY_KEY (line 1) and quoted "my_key" (line 4), but not my_key_extra nor comment
    assert_eq!(
        script_locs.len(),
        2,
        "expected 2 script usages (case-insensitive, whole-token, quoted), got {:?}",
        result
    );
    let lines: Vec<u64> = script_locs
        .iter()
        .map(|l| l["range"]["start"]["line"].as_u64().unwrap())
        .collect();
    assert!(
        lines.contains(&1),
        "MY_KEY on line 1 not found, got {:?}",
        lines
    );
    assert!(
        lines.contains(&4),
        "quoted my_key on line 4 not found, got {:?}",
        lines
    );
}

#[test]
fn test_loc_references_from_script_side() {
    let yml = "l_english:\n my_key:0 \"Hello\"\n";
    let script = "x = {\n    title = my_key\n}\n";
    let files = &[
        ("localisation/test_l_english.yml", yml),
        ("common/test/event.txt", script),
    ];
    // Cursor on script usage (line 1, inside my_key)
    let result = feature_request(
        GOTO_RULES,
        files,
        &["localisation/test_l_english.yml", "common/test/event.txt"],
        serde_json::json!({}),
        "common/test/event.txt",
        "textDocument/references",
        serde_json::json!({"position": {"line": 1, "character": 13}, "context": {"includeDeclaration": true}}),
    );
    let locs = result.as_array().expect("refs from script");
    assert!(
        locs.iter()
            .any(|l| l["uri"].as_str().unwrap().contains("test_l_english.yml")),
        "script-side references must include yml definition, got {:?}",
        result
    );
}

#[test]
fn test_loc_rename_only_existing_siblings() {
    let yml = "l_english:\n my_item:0 \"Item\"\n my_item_desc:0 \"Desc\"\n";
    // No my_item_tooltip exists, so rename should NOT produce it
    let files = &[("localisation/test_l_english.yml", yml)];
    let resp = rename_loc_key(files, "localisation/test_l_english.yml", "new_item");
    let (_, new_texts) = rename_edits(&resp["result"]);
    assert!(
        new_texts.contains(&"new_item".to_string()),
        "new_item, got {:?}",
        new_texts
    );
    assert!(
        new_texts.contains(&"new_item_desc".to_string()),
        "sibling _desc must be renamed, got {:?}",
        new_texts
    );
    assert!(
        !new_texts.contains(&"new_item_tooltip".to_string()),
        "non-existent _tooltip must NOT be in edit, got {:?}",
        new_texts
    );
}

#[test]
fn test_loc_rename_across_languages() {
    let yml_en = "l_english:\n my_key:0 \"Hello\"\n";
    let yml_fr = "l_french:\n my_key:0 \"Bonjour\"\n";
    let files = &[
        ("localisation/test_l_english.yml", yml_en),
        ("localisation/test_l_french.yml", yml_fr),
    ];
    let resp = rename_loc_key(files, "localisation/test_l_english.yml", "new_key");
    let edit = &resp["result"];
    assert!(!edit.is_null(), "rename must succeed, got {}", resp);
    let (uris, new_texts) = rename_edits(edit);
    assert!(
        uris.iter().any(|u| u.contains("test_l_english.yml")),
        "english file must be edited, got {:?}",
        uris
    );
    assert!(
        uris.iter().any(|u| u.contains("test_l_french.yml")),
        "french file must be edited, got {:?}",
        uris
    );
    assert!(
        new_texts.iter().filter(|t| *t == "new_key").count() >= 2,
        "expected at least 2 new_key edits, got {:?} in {:?}",
        new_texts,
        uris
    );
}

#[test]
fn test_loc_rename_updates_dollar_refs_in_yml() {
    let yml_main = "l_english:\n my_key:0 \"Hello\"\n";
    let yml_ref = "l_english:\n other:0 \"See $my_key$ and $my_key|Y$ end\"\n";
    let files = &[
        ("localisation/main_l_english.yml", yml_main),
        ("localisation/ref_l_english.yml", yml_ref),
    ];
    let resp = rename_loc_key(files, "localisation/main_l_english.yml", "new_key");
    let edit = &resp["result"];
    assert!(!edit.is_null(), "rename must succeed, got {}", resp);
    let (uris, new_texts) = rename_edits(edit);
    assert!(
        uris.iter().any(|u| u.contains("main_l_english.yml")),
        "main file must be edited, got {:?}",
        uris
    );
    assert!(
        uris.iter().any(|u| u.contains("ref_l_english.yml")),
        "ref file must be edited, got {:?}",
        uris
    );
    assert!(
        new_texts.iter().filter(|t| *t == "new_key").count() >= 3,
        "expected at least 3 new_key edits (def + 2 refs), got {:?} in {:?}",
        new_texts,
        uris
    );
}

/// Definitions come from the loc index, one per configured language, and an
/// open buffer is read as the editor has it: after a line is inserted above
/// the English entry, its definition moves with it while the closed French
/// file keeps the indexed line (#474).
#[test]
fn test_loc_references_definitions_per_language_from_index() {
    let yml_en = "l_english:\n my_key:0 \"Hello\"\n";
    let yml_fr = "l_french:\n my_key:0 \"Bonjour\"\n";
    let script = "x = {\n    title = my_key\n}\n";
    let en_rel = "localisation/test_l_english.yml";
    let files = &[
        (en_rel, yml_en),
        ("localisation/test_l_french.yml", yml_fr),
        ("common/test/event.txt", script),
    ];
    let mut server = loc_references_server(files, en_rel);

    let definitions = |server: &mut TestServer, line: u32| {
        let locs = loc_references(server, en_rel, line);
        assert!(
            locs.iter()
                .any(|l| l["uri"].as_str().unwrap().contains("event.txt")),
            "script usage missing from {locs:?}"
        );
        loc_definitions(&locs)
    };

    assert_eq!(
        definitions(&mut server, 1),
        vec![
            ("test_l_english.yml".to_string(), 1, 1),
            ("test_l_french.yml".to_string(), 1, 1),
        ]
    );

    // A comment line above the entry. The edit's own validation does not
    // re-index an open buffer; the request has to read the buffer itself.
    server.change(en_rel, 2, "l_english:\n # moved\n my_key:0 \"Hello\"\n");
    server.wait_for_diagnostics(en_rel);
    let after = definitions(&mut server, 2);
    server.stop();
    assert_eq!(
        after,
        vec![
            ("test_l_english.yml".to_string(), 2, 1),
            ("test_l_french.yml".to_string(), 1, 1),
        ]
    );
}

/// The per-file index update between scans keeps to the scan's rules: a
/// watched file in a language that is not configured is not a definition,
/// the way the scan would not have parsed it. And a closed file edited
/// without a watcher event still lists its definition, found where the entry
/// is now rather than dropped because the indexed line moved (#474).
#[test]
fn test_loc_references_watched_file_keeps_to_configured_languages_and_finds_a_moved_line() {
    let yml_en = "l_english:\n my_key:0 \"Hello\"\n";
    let yml_fr = "l_french:\n my_key:0 \"Bonjour\"\n";
    let script = "x = {\n    title = my_key\n}\n";
    let en_rel = "localisation/test_l_english.yml";
    let files = &[
        (en_rel, yml_en),
        ("localisation/test_l_french.yml", yml_fr),
        ("common/test/event.txt", script),
    ];
    let mut server = loc_references_server(files, en_rel);

    // A German file appears on disk: the watcher reports it, the server
    // validates it, but German is not a configured language.
    let de_rel = "localisation/test_l_german.yml";
    let de_uri = server.write_file_uri(de_rel, "l_german:\n my_key:0 \"Hallo\"\n");
    server.send(&watched_created(std::slice::from_ref(&de_uri)));
    server.wait_for_diagnostics(de_rel);
    assert_eq!(
        loc_definitions(&loc_references(&mut server, en_rel, 1)),
        vec![
            ("test_l_english.yml".to_string(), 1, 1),
            ("test_l_french.yml".to_string(), 1, 1),
        ],
        "a language the scan does not parse must not become a definition"
    );

    // The closed French file gains a line above its entry with no watcher
    // event: the index still says line 1, the file says line 2.
    server.write_file(
        "localisation/test_l_french.yml",
        "l_french:\n # moved\n my_key:0 \"Bonjour\"\n",
    );
    let moved = loc_definitions(&loc_references(&mut server, en_rel, 1));
    server.stop();
    assert_eq!(
        moved,
        vec![
            ("test_l_english.yml".to_string(), 1, 1),
            ("test_l_french.yml".to_string(), 2, 1),
        ],
        "a definition whose indexed line moved must be found where it is now"
    );
}

/// A key the mod and the base game both define goes to the mod's file; once
/// the mod's file is deleted under the watcher, goto falls back to the base
/// game's definition without waiting for a rescan (#474).
#[test]
fn test_loc_goto_falls_back_to_the_base_game_after_a_watched_delete() {
    let def_rel = "localisation/english/def_l_english.yml";
    let use_rel = "localisation/english/use_l_english.yml";
    let use_text = loc(" A:0 \"$SHARED_KEY$\"\n");
    let mut server = TestServer::builder()
        .rules(GOTO_RULES)
        .vanilla_file(
            "localisation/english/base_l_english.yml",
            loc(" SHARED_KEY:0 \"Base text\"\n"),
        )
        .file(def_rel, loc(" SHARED_KEY:0 \"Mod text\"\n"))
        .file(use_rel, use_text.as_str())
        .spawn();
    server.initialized();
    server.wait_for_scan_done();
    let def_uri = server.uri(def_rel);
    server.open_and_wait(use_rel, &use_text);

    let before = goto_lands_in(&mut server, use_rel, "def_l_english.yml");
    assert_eq!(
        before.len(),
        1,
        "the mod's definition alone should answer, got: {before:?}"
    );

    std::fs::remove_file(server.path(def_rel)).unwrap();
    server.notify(
        "workspace/didChangeWatchedFiles",
        serde_json::json!({ "changes": [{ "uri": def_uri, "type": 3 }] }),
    );
    let after = goto_lands_in(&mut server, use_rel, "base_l_english.yml");
    server.stop();
    assert!(
        after.iter().any(|u| u.ends_with("base_l_english.yml")),
        "with the mod's definition deleted, goto should fall back to the base game, got: {after:?}"
    );
    assert!(
        !after.iter().any(|u| u.ends_with("def_l_english.yml")),
        "a deleted file must not stay a goto target, got: {after:?}"
    );
}

/// Rename the loc key at (1, 2) of `open_rel` to `new_name` on a server over
/// `files`, past its scan, with `open_rel` open and a client that takes
/// `documentChanges`. Returns the whole response.
fn rename_loc_key(files: &[(&str, &str)], open_rel: &str, new_name: &str) -> Value {
    let caps = json!({ "workspace": { "workspaceEdit": { "documentChanges": true } } });
    let mut server = feature_start(feature_builder(GOTO_RULES, files, caps), files, &[open_rel]);
    let uri = server.uri(open_rel);
    let resp = server.call(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 2 },
            "newName": new_name,
        }),
    );
    server.stop();
    resp
}

/// The edited URIs and the new texts of a rename result, whether it comes as
/// `documentChanges` or as `changes`.
fn rename_edits(edit: &Value) -> (Vec<String>, Vec<String>) {
    let mut uris = Vec::new();
    let mut new_texts = Vec::new();
    let mut collect = |edits: &Value| {
        for e in edits.as_array().into_iter().flatten() {
            if let Some(text) = e["newText"].as_str() {
                new_texts.push(text.to_string());
            }
        }
    };
    if let Some(docs) = edit["documentChanges"].as_array() {
        for doc in docs {
            if let Some(uri) = doc["textDocument"]["uri"].as_str() {
                uris.push(uri.to_string());
            }
            collect(&doc["edits"]);
        }
    } else if let Some(map) = edit["changes"].as_object() {
        for (uri, edits) in map {
            uris.push(uri.clone());
            collect(edits);
        }
    }
    (uris, new_texts)
}

/// A server over `files`, configured for English and French, past its scan with
/// `en_rel` open.
fn loc_references_server(files: &[(&str, &str)], en_rel: &str) -> TestServer {
    let builder = feature_builder(GOTO_RULES, files, json!({}))
        .init_opts(json!({ "localisationLanguages": ["English", "French"] }));
    feature_start(builder, files, &[en_rel])
}

/// Every location `textDocument/references` returns for the key at (line, 2) of
/// `en_rel`, declarations included.
fn loc_references(server: &mut TestServer, en_rel: &str, line: u32) -> Vec<Value> {
    let uri = server.uri(en_rel);
    let result = server.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": 2 },
            "context": { "includeDeclaration": true },
        }),
    );
    result
        .as_array()
        .unwrap_or_else(|| panic!("references array, got {result}"))
        .clone()
}

/// The `(file name, line, character)` of each loc definition among `locs`.
fn loc_definitions(locs: &[Value]) -> Vec<(String, u64, u64)> {
    let mut defs: Vec<(String, u64, u64)> = locs
        .iter()
        .filter(|l| l["uri"].as_str().unwrap().contains("_l_"))
        .map(|l| {
            (
                l["uri"]
                    .as_str()
                    .unwrap()
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .to_string(),
                l["range"]["start"]["line"].as_u64().unwrap(),
                l["range"]["start"]["character"].as_u64().unwrap(),
            )
        })
        .collect();
    defs.sort();
    defs
}

/// `$SHARED_KEY$` on line 1 col 10 of `rel`. Polls until the answer ends in
/// `want`, since both the scan and the watched batch land asynchronously, and
/// returns the last answer.
fn goto_lands_in(server: &mut TestServer, rel: &str, want: &str) -> Vec<String> {
    let uri = server.uri(rel);
    let mut last = Vec::new();
    let _ = poll_until_attempts(50, Duration::from_millis(300), || {
        let result = server.request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": 1, "character": 10 },
            }),
        );
        last = locations(&result).into_iter().map(|(uri, _)| uri).collect();
        last.iter().any(|u| u.ends_with(want)).then_some(())
    });
    last
}
