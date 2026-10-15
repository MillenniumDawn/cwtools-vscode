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
