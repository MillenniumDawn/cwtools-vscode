use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::common::{
    GOTO_RULES, TestServer, editor_server, editor_server_with_rules, feature_request, path_uri,
    poll_until, storm_builder, storm_start,
};

#[test]
fn test_document_symbols_nested() {
    // A focus tree with two focuses: the outline is a nested tree, each block
    // named by its `id` child so repeated `focus` keys stay distinct.
    let doc = "focus_tree = {\n    id = my_tree\n    focus = {\n        id = focus_a\n        x = 1\n    }\n    focus = {\n        id = focus_b\n    }\n}\n";
    let files = &[("common/national_focus/f.txt", doc)];
    let caps = serde_json::json!({
        "textDocument": { "documentSymbol": { "hierarchicalDocumentSymbolSupport": true } }
    });
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/national_focus/f.txt"],
        caps,
        "common/national_focus/f.txt",
        "textDocument/documentSymbol",
        serde_json::json!({}),
    );
    let syms = result.as_array().expect("nested symbols array");
    let tree = &syms[0];
    assert_eq!(
        tree["name"], "my_tree",
        "top symbol named by id, got: {}",
        result
    );
    // selection_range ⊆ range: they share a start, selection ends within range.
    assert_eq!(tree["selectionRange"]["start"], tree["range"]["start"]);
    let children = tree["children"].as_array().expect("nested children");
    assert!(
        children.iter().any(|c| c["name"] == "focus_a"),
        "expected nested focus_a, got: {}",
        result
    );
    assert!(
        children.iter().any(|c| c["name"] == "focus_b"),
        "expected nested focus_b, got: {}",
        result
    );
}

#[test]
fn test_folding_ranges_nested_blocks() {
    let doc = "outer = {\n    inner = {\n        x = 1\n    }\n}\n";
    let files = &[("common/national_focus/f.txt", doc)];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/national_focus/f.txt"],
        serde_json::json!({}),
        "common/national_focus/f.txt",
        "textDocument/foldingRange",
        serde_json::json!({}),
    );
    let ranges = result.as_array().expect("folding ranges");
    let has = |s: u64, e: u64| {
        ranges
            .iter()
            .any(|r| r["startLine"] == s && r["endLine"] == e)
    };
    assert!(has(0, 4), "expected outer fold 0..4, got: {}", result);
    assert!(has(1, 3), "expected inner fold 1..3, got: {}", result);
}

#[test]
fn test_folding_ranges_comments_and_regions() {
    // A comment block folds as kind "comment"; #region/#endregion pairs fold
    // as kind "region"; brace folds are unchanged.
    let doc = "# header one\n# header two\n# header three\n#region Alpha\na = {\n    x = 1\n}\n#endregion\n";
    let files = &[("common/national_focus/f.txt", doc)];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/national_focus/f.txt"],
        serde_json::json!({}),
        "common/national_focus/f.txt",
        "textDocument/foldingRange",
        serde_json::json!({}),
    );
    let ranges = result.as_array().expect("folding ranges");
    let has = |s: u64, e: u64, k: &str| {
        ranges
            .iter()
            .any(|r| r["startLine"] == s && r["endLine"] == e && r["kind"] == k)
    };
    assert!(has(0, 2, "comment"), "comment block fold, got: {}", result);
    assert!(has(3, 7, "region"), "#region marker fold, got: {}", result);
    assert!(has(4, 6, "region"), "brace fold, got: {}", result);
}

#[test]
fn test_selection_range_expands_token_to_blocks() {
    // Expanding from `bar`: token, then block content, then the full block.
    let doc = "a = {\n    foo = bar\n}\n";
    let files = &[("common/national_focus/f.txt", doc)];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/national_focus/f.txt"],
        serde_json::json!({}),
        "common/national_focus/f.txt",
        "textDocument/selectionRange",
        serde_json::json!({ "positions": [{ "line": 1, "character": 10 }] }),
    );
    let chain = &result.as_array().expect("one entry per position")[0];
    assert_eq!(
        chain["range"]["start"]["character"], 10,
        "token start: {}",
        result
    );
    assert_eq!(
        chain["range"]["end"]["character"], 13,
        "token end: {}",
        result
    );
    let parent = &chain["parent"];
    assert_eq!(
        parent["range"]["start"]["line"], 0,
        "content parent: {}",
        result
    );
    let grandparent = &parent["parent"];
    assert_eq!(
        grandparent["range"]["start"]["character"], 4,
        "full-pair grandparent starts at the open brace: {}",
        result
    );
}

#[test]
fn test_document_highlight_occurrences() {
    // `MY_FOCUS` appears three times; highlighting one returns all three.
    let doc = "a = {\n    has_focus = MY_FOCUS\n}\nb = {\n    has_focus = MY_FOCUS\n}\nc = {\n    load_oob = MY_FOCUS\n}\n";
    let files = &[("common/decisions/d.txt", doc)];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/d.txt"],
        serde_json::json!({}),
        "common/decisions/d.txt",
        "textDocument/documentHighlight",
        serde_json::json!({ "position": { "line": 1, "character": 16 } }),
    );
    let hl = result.as_array().expect("highlights array");
    assert_eq!(
        hl.len(),
        3,
        "expected 3 occurrences of MY_FOCUS, got: {}",
        result
    );
}

#[test]
fn test_document_highlight_skips_comments_and_reports_kinds() {
    // MY_FOCUS appears twice in code and once in a comment: the comment
    // occurrence must not be highlighted, and value occurrences report
    // kind Read (2) instead of Text (1).
    let doc = "a = {\n    has_focus = MY_FOCUS\n}\n# note MY_FOCUS legacy\nb = {\n    has_focus = MY_FOCUS\n}\n";
    let files = &[("common/decisions/d.txt", doc)];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/d.txt"],
        serde_json::json!({}),
        "common/decisions/d.txt",
        "textDocument/documentHighlight",
        serde_json::json!({ "position": { "line": 1, "character": 16 } }),
    );
    let hl = result.as_array().expect("highlights array");
    assert_eq!(
        hl.len(),
        2,
        "comment occurrence must be skipped, got: {}",
        result
    );
    assert!(
        hl.iter().all(|h| h["kind"] == 2),
        "value occurrences must report Read (2), got: {}",
        result
    );
}

const LINK_RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
decision = {
    ## cardinality = 0..1
    picture = filepath[gfx/,.dds]
    ## cardinality = 0..1
    icon = icon[gfx/interface]
}
"#;

#[test]
fn test_document_links_resolve_filepath_and_icon_leaves() {
    // `picture` is filepath[gfx/,.dds] and `icon` is icon[gfx/interface]:
    // both leaves become links to the files they reference. A value whose
    // target doesn't exist gets no link.
    let files = &[
        ("gfx/pic.dds", "dds"),
        ("gfx/interface/myicon.dds", "dds"),
        (
            "common/decisions/d.txt",
            "adec = {\n    picture = pic\n    icon = myicon\n}\n",
        ),
    ];
    let result = feature_request(
        LINK_RULES,
        files,
        &["common/decisions/d.txt"],
        serde_json::json!({}),
        "common/decisions/d.txt",
        "textDocument/documentLink",
        serde_json::json!({}),
    );
    let links = result.as_array().expect("document links");
    let link_at = |line: u64, ch: u64| {
        links
            .iter()
            .find(|l| l["range"]["start"]["line"] == line && l["range"]["start"]["character"] == ch)
            .unwrap_or_else(|| panic!("no link at {}:{}, got: {}", line, ch, result))
    };
    assert!(
        link_at(1, 14)["target"]
            .as_str()
            .unwrap_or("")
            .ends_with("gfx/pic.dds"),
        "picture link target, got: {}",
        result
    );
    assert!(
        link_at(2, 11)["target"]
            .as_str()
            .unwrap_or("")
            .ends_with("gfx/interface/myicon.dds"),
        "icon link target, got: {}",
        result
    );
}

#[test]
fn test_workspace_symbols_rank_and_cover_all_sources() {
    // Three symbol sources match "my": a focus instance (exact-prefix rank on
    // "my_focus"), a loc key (prefix), and an @-constant (substring). The
    // result must cover all three with distinct kinds and be ordered
    // rank-then-name, not hash order.
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        (
            "common/decisions/d.txt",
            "@my_const = 5\nadec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
        (
            "localisation/english/x_l_english.yml",
            "l_english:\n my_focus_tooltip:0 \"Text\"\n",
        ),
    ];
    let result = feature_request(
        GOTO_RULES,
        files,
        &[],
        serde_json::json!({}),
        "common/national_focus/f.txt",
        "workspace/symbol",
        serde_json::json!({ "query": "my" }),
    );
    let syms = result.as_array().expect("symbols array");
    let names: Vec<&str> = syms.iter().filter_map(|s| s["name"].as_str()).collect();
    assert_eq!(
        names,
        vec!["MY_FOCUS", "my_focus_tooltip", "@my_const"],
        "expected rank-then-name order, got: {}",
        result
    );
    let kind_of = |n: &str| {
        syms.iter()
            .find(|s| s["name"] == n)
            .map(|s| s["kind"].as_u64().unwrap())
    };
    assert_eq!(kind_of("MY_FOCUS"), Some(23), "focus instance is STRUCT");
    assert_eq!(kind_of("@my_const"), Some(14), "@-constant is CONSTANT");
    assert_eq!(kind_of("my_focus_tooltip"), Some(20), "loc key is KEY");
}

#[test]
fn test_workspace_symbols_empty_query_caps_at_limit_best_by_name() {
    // The picker's initial request carries an empty query, which admits every
    // symbol at substring rank. With 511 symbols in the workspace the answer
    // must be exactly the 500 lexicographically-first names, not an arbitrary
    // subset and not the whole universe.
    let mut loc = String::from("l_english:\n");
    for i in 0..510 {
        loc.push_str(&format!(" key_{i:03}:0 \"T\"\n"));
    }
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        ("localisation/english/big_l_english.yml", loc.as_str()),
    ];
    let result = feature_request(
        GOTO_RULES,
        files,
        &[],
        serde_json::json!({}),
        "common/national_focus/f.txt",
        "workspace/symbol",
        serde_json::json!({ "query": "" }),
    );
    let syms = result.as_array().expect("symbols array");
    let names: Vec<&str> = syms.iter().filter_map(|s| s["name"].as_str()).collect();
    assert_eq!(names.len(), 500, "capped at 500, got {}", names.len());
    // All ranks tie for the empty query, so order is by name: "MY_FOCUS"
    // (uppercase sorts before lowercase) then key_000..key_498; the last
    // eleven keys fall past the cap.
    assert_eq!(names[0], "MY_FOCUS");
    assert_eq!(names[1], "key_000");
    assert_eq!(names[499], "key_498");
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "response is name-ordered");
}

#[test]
fn test_references_finds_closed_file() {
    // A (open) and B (never opened) both reference focus MY_FOCUS. Find-refs from
    // A must reach B via the workspace reverse index.
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        (
            "common/decisions/a.txt",
            "adec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
        (
            "common/decisions/b.txt",
            "bdec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
    ];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/a.txt"],
        serde_json::json!({}),
        "common/decisions/a.txt",
        "textDocument/references",
        serde_json::json!({
            "position": { "line": 1, "character": 16 },
            "context": { "includeDeclaration": true }
        }),
    );
    let locs = result.as_array().expect("references array");
    assert!(
        locs.iter()
            .any(|l| l["uri"].as_str().unwrap_or("").ends_with("decisions/b.txt")),
        "references must include the closed file b.txt, got: {}",
        result
    );
}

#[test]
fn test_references_exclude_declaration_omits_definition() {
    // Same layout, but the client asks for includeDeclaration = false: the
    // definition site in national_focus/f.txt must be omitted while the use
    // sites in the decision files are still returned.
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        (
            "common/decisions/a.txt",
            "adec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
        (
            "common/decisions/b.txt",
            "bdec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
    ];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/a.txt"],
        serde_json::json!({}),
        "common/decisions/a.txt",
        "textDocument/references",
        serde_json::json!({
            "position": { "line": 1, "character": 16 },
            "context": { "includeDeclaration": false }
        }),
    );
    let locs = result.as_array().expect("references array");
    assert!(
        locs.iter()
            .any(|l| l["uri"].as_str().unwrap_or("").ends_with("decisions/b.txt")),
        "use sites must still be returned, got: {}",
        result
    );
    assert!(
        !locs.iter().any(|l| l["uri"]
            .as_str()
            .unwrap_or("")
            .ends_with("national_focus/f.txt")),
        "definition must be omitted when includeDeclaration is false, got: {}",
        result
    );
}

#[test]
fn test_rename_edits_closed_file() {
    // Renaming MY_FOCUS from the open file A must also edit the closed file B, at
    // the value column (16), not the key.
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        (
            "common/decisions/a.txt",
            "adec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
        (
            "common/decisions/b.txt",
            "bdec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
    ];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/a.txt"],
        serde_json::json!({}),
        "common/decisions/a.txt",
        "textDocument/rename",
        serde_json::json!({ "position": { "line": 1, "character": 16 }, "newName": "NEW_FOCUS" }),
    );
    let changes = result["changes"]
        .as_object()
        .expect("WorkspaceEdit changes");
    let b_key = changes
        .keys()
        .find(|u| u.ends_with("decisions/b.txt"))
        .unwrap_or_else(|| panic!("rename must edit closed file b.txt, got: {}", result));
    let edits = changes[b_key].as_array().expect("edits for b.txt");
    assert_eq!(
        edits[0]["range"]["start"]["character"], 16,
        "edit must target the value column, got: {}",
        result
    );
    assert_eq!(edits[0]["newText"], "NEW_FOCUS");
}

#[test]
fn test_rename_emits_versioned_document_changes_when_supported() {
    // A client that advertises workspace.workspaceEdit.documentChanges gets
    // TextDocumentEdits with versions: the open doc carries its version, the
    // closed file null.
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        (
            "common/decisions/a.txt",
            "adec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
        (
            "common/decisions/b.txt",
            "bdec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
    ];
    let caps = serde_json::json!({
        "workspace": { "workspaceEdit": { "documentChanges": true } }
    });
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/a.txt"],
        caps,
        "common/decisions/a.txt",
        "textDocument/rename",
        serde_json::json!({ "position": { "line": 1, "character": 16 }, "newName": "NEW_FOCUS" }),
    );
    assert!(
        result["changes"].is_null(),
        "legacy changes must be absent, got: {}",
        result
    );
    let doc_changes = result["documentChanges"]
        .as_array()
        .unwrap_or_else(|| panic!("documentChanges must be present, got: {}", result));
    let entry_for = |suffix: &str| {
        doc_changes
            .iter()
            .find(|e| {
                e["textDocument"]["uri"]
                    .as_str()
                    .unwrap_or("")
                    .ends_with(suffix)
            })
            .unwrap_or_else(|| panic!("no documentChanges entry for {}, got: {}", suffix, result))
    };
    assert_eq!(
        entry_for("decisions/a.txt")["textDocument"]["version"],
        1,
        "open doc carries its version, got: {}",
        result
    );
    assert!(
        entry_for("decisions/b.txt")["textDocument"]["version"].is_null(),
        "closed file version is null, got: {}",
        result
    );
}

#[derive(Clone, Copy, Debug)]
enum RenameInterleave {
    Change,
    Close,
    Open,
    CloseReopen,
}

#[test]
fn test_rename_refuses_changes_after_snapshot_capture() {
    for localisation in [false, true] {
        for document_changes in [true, false] {
            let response = rename_during_document_transition(
                localisation,
                document_changes,
                RenameInterleave::Change,
            );
            assert_eq!(
                response["error"]["code"], -32801,
                "localisation={localisation}, documentChanges={document_changes}: {response}"
            );
            assert!(response["result"].is_null(), "got: {response}");
        }
    }
}

#[test]
fn test_rename_refuses_open_close_transitions_after_snapshot_capture() {
    for localisation in [false, true] {
        for transition in [
            RenameInterleave::Close,
            RenameInterleave::Open,
            RenameInterleave::CloseReopen,
        ] {
            let response = rename_during_document_transition(localisation, true, transition);
            assert_eq!(
                response["error"]["code"], -32801,
                "localisation={localisation}, {transition:?}: {response}"
            );
            assert!(response["result"].is_null(), "got: {response}");
        }
    }
}

fn rename_during_document_transition(
    localisation: bool,
    document_changes: bool,
    transition: RenameInterleave,
) -> Value {
    let markers = tempfile::tempdir().unwrap();
    let gate = markers.path().join("snapshot-hold");
    let ready = markers.path().join("snapshot-ready");
    let files = [
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        (
            "common/decisions/a.txt",
            "adec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
        (
            "common/decisions/b.txt",
            "bdec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
        (
            "localisation/test_l_english.yml",
            "l_english:\n my_key:0 \"Hello\"\n",
        ),
        (
            "localisation/test_l_french.yml",
            "l_french:\n my_key:0 \"Bonjour\"\n",
        ),
    ];
    let mut builder = TestServer::builder()
        .rules_file("r.cwt", GOTO_RULES)
        .vanilla()
        .capabilities(
            json!({ "workspace": { "workspaceEdit": { "documentChanges": document_changes } } }),
        )
        .env("CWTOOLS_NAV_SNAPSHOT_HOLD_FILE", &gate)
        .env("CWTOOLS_NAV_SNAPSHOT_HOLD_READY_FILE", &ready);
    for (rel, text) in files {
        builder = builder.file(rel, text);
    }
    let source = if localisation { files[3] } else { files[1] };
    let closed = if localisation { files[4] } else { files[2] };
    let mut server = builder.spawn();
    server.initialized();
    server.wait_for_scan_done();
    server.open(source.0, source.1);
    server.wait_for_diagnostics(source.0);

    std::fs::write(&gate, b"hold").unwrap();
    server.send_request(
        700,
        "textDocument/rename",
        json!({
            "textDocument": { "uri": server.uri(source.0) },
            "position": { "line": 1, "character": if localisation { 2 } else { 18 } },
            "newName": if localisation { "renamed_key" } else { "RENAMED_FOCUS" }
        }),
    );
    assert!(
        poll_until(
            Instant::now() + Duration::from_secs(20),
            Duration::from_millis(10),
            || ready.exists().then_some(())
        )
        .is_some(),
        "rename did not capture its open-buffer snapshot"
    );
    let shifted_source = if localisation {
        "l_english:\n\n my_key:0 \"Hello\"\n"
    } else {
        "# inserted line\nadec = {\n    has_focus = MY_FOCUS\n}\n"
    };
    match transition {
        RenameInterleave::Change => server.change(source.0, 2, shifted_source),
        RenameInterleave::Close | RenameInterleave::CloseReopen => {
            server.close(source.0);
            if matches!(transition, RenameInterleave::CloseReopen) {
                rename_notification_barrier(&mut server);
                server.open(source.0, shifted_source);
            }
        }
        RenameInterleave::Open => {
            let text = if localisation {
                "l_french:\n\n my_key:0 \"Bonjour\"\n"
            } else {
                "# inserted line\nbdec = {\n    has_focus = MY_FOCUS\n}\n"
            };
            server.open(closed.0, text);
        }
    }
    // This command waits for the notification handlers, so the transition
    // is observed while the snapshot gate is still held.
    rename_notification_barrier(&mut server);
    assert!(
        gate.exists(),
        "snapshot hold released before the transition"
    );
    std::fs::remove_file(&gate).unwrap();
    let response = server.read_response_for_id(700);
    server.stop();
    response
}

fn rename_notification_barrier(server: &mut TestServer) {
    let response = server.call(
        "workspace/executeCommand",
        json!({ "command": "getFileTypes", "arguments": [] }),
    );
    assert!(
        response["error"].is_null(),
        "notification barrier failed: {response}"
    );
}

/// One `@const` rename in a session with NO workspace folder, driven straight
/// through the wire so a refusal (`error`) is visible instead of being
/// flattened into a null `result` the way [`feature_request`] does. Nothing is
/// scanned and no folder is reported, so `editable_roots` is empty and the edit
/// boundary would refuse every path — the own-document exemption is the only
/// reason these renames still work.
fn at_const_rename_without_workspace(doc_uri: &str, text: &str) -> Value {
    let mut server = TestServer::builder()
        .rules_file("r.cwt", GOTO_RULES)
        .no_root()
        .spawn();
    server.initialized();
    server.open_uri(doc_uri, text);
    let resp = server.call(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": doc_uri },
            "position": { "line": 0, "character": 2 },
            "newName": "@renamed",
        }),
    );
    server.stop();
    resp
}

#[test]
fn test_rename_at_constant_refuses_an_untitled_document() {
    let doc = "@my_const = 5\nadec = {\n    x = @my_const\n}\n";
    let resp = at_const_rename_without_workspace("untitled:Untitled-1", doc);
    assert!(
        resp["error"].is_null(),
        "rename must answer cleanly: {resp}"
    );
    assert!(
        resp["result"].is_null(),
        "an untitled buffer must not enter open-document state: {resp}"
    );
}

#[test]
fn test_rename_at_constant_refuses_a_file_without_a_workspace_folder() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("loose.txt");
    let doc = "@my_const = 5\nadec = {\n    x = @my_const\n}\n";
    std::fs::write(&path, doc).unwrap();
    let doc_uri = path_uri(&path);
    let resp = at_const_rename_without_workspace(&doc_uri, doc);
    assert!(
        resp["error"].is_null(),
        "rename must answer cleanly: {resp}"
    );
    assert!(
        resp["result"].is_null(),
        "a buffer without a workspace root must not enter open-document state: {resp}"
    );
}

/// A rename whose edit set reaches a second document the edit boundary refuses
/// is cancelled outright. Here the definition lives in the base-game install
/// and the workspace only links to it, so the scan indexes it (the link is a
/// workspace path) but writing it would write through the link into the
/// install. Applying only the in-workspace half would leave the reference
/// pointing at a name that no longer matches its definition, so it is the whole
/// edit set or nothing (#160).
///
/// The in-workspace counterpart (a cross-file rename where every target is
/// inside the workspace, which now runs through the same choke point) is
/// `test_rename_edits_closed_file`.
#[cfg(unix)]
#[test]
fn test_rename_does_not_edit_through_a_symlink() {
    let rel = "common/decisions/a.txt";
    let text = "adec = {\n    has_focus = MY_FOCUS\n}\n";
    // The focus is defined in the base-game install; the workspace reaches it
    // only through a link. The scan rejects the symlink (#161), so the
    // definition is never indexed and rename only edits the in-workspace
    // reference, never the base-game file.
    let mut server = TestServer::builder()
        .rules_file("r.cwt", GOTO_RULES)
        .vanilla_file("f_source.txt", "MY_FOCUS = { x = yes }\n")
        .file(rel, text)
        .spawn();
    let source = server.vanilla_dir().join("f_source.txt");
    let linked_def = server.path("common/national_focus/f.txt");
    std::fs::create_dir_all(linked_def.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&source, &linked_def).unwrap();
    server.initialized();
    server.wait_for_scan_done();
    server.open_and_wait(rel, text);

    let resp = server.call(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": server.uri(rel) },
            "position": { "line": 1, "character": 16 },
            "newName": "NEW_FOCUS",
        }),
    );
    server.stop();

    // The symlinked definition is rejected by the scan, so rename only edits
    // the in-workspace reference and never touches the base-game file.
    let changes = resp["result"]["changes"]
        .as_object()
        .unwrap_or_else(|| panic!("rename must carry edits, got: {resp}"));
    assert!(
        changes
            .keys()
            .all(|u| u.ends_with("common/decisions/a.txt")),
        "rename must only edit the in-workspace file, got: {resp}"
    );
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "MY_FOCUS = { x = yes }\n",
        "the base-game file is untouched"
    );
}

#[test]
fn test_rename_at_constant_renames_file_locally() {
    // `@` script constants are file-local: renaming from the definition
    // rewrites the definition and the read, but never the occurrence in a
    // trailing comment.
    let doc = "@my_const = 5\nadec = {\n    x = @my_const # @my_const stays\n}\n";
    let files = &[("common/decisions/d.txt", doc)];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/d.txt"],
        serde_json::json!({}),
        "common/decisions/d.txt",
        "textDocument/rename",
        serde_json::json!({ "position": { "line": 0, "character": 2 }, "newName": "@renamed" }),
    );
    let changes = result["changes"]
        .as_object()
        .expect("WorkspaceEdit changes");
    let key = changes
        .keys()
        .find(|u| u.ends_with("decisions/d.txt"))
        .unwrap_or_else(|| panic!("rename must edit d.txt, got: {}", result));
    let edits = changes[key].as_array().expect("edits for d.txt");
    let mut spans: Vec<(u64, u64)> = edits
        .iter()
        .map(|e| {
            (
                e["range"]["start"]["line"].as_u64().unwrap(),
                e["range"]["start"]["character"].as_u64().unwrap(),
            )
        })
        .collect();
    spans.sort_unstable();
    assert_eq!(
        spans,
        vec![(0, 0), (2, 8)],
        "definition and read, never the comment, got: {}",
        result
    );
    assert!(edits.iter().all(|e| e["newText"] == "@renamed"));
}

#[test]
fn test_rename_rejects_invalid_replacement_names_over_the_wire() {
    let files = [
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        (
            "common/decisions/a.txt",
            "@cost = 1\nadec = {\n    has_focus = MY_FOCUS\n    x = @cost\n}\n",
        ),
        (
            "localisation/test_l_english.yml",
            "l_english:\n my_key:0 \"Hello\"\n",
        ),
    ];
    let mut builder = storm_builder();
    for (rel, text) in files {
        builder = builder.file(rel, text);
    }
    let mut server = storm_start(builder);
    for (rel, text) in &files[1..] {
        server.open(rel, text);
        server.wait_for_diagnostics(rel);
    }
    let cases = [
        (files[1].0, 0, 2, "@new name"),
        (files[1].0, 0, 2, "@new\n}"),
        (files[1].0, 0, 2, "plain_name"),
        (files[1].0, 0, 2, "@"),
        (files[1].0, 0, 2, "@@new"),
        (files[1].0, 2, 18, "@foo"),
        (files[1].0, 2, 18, "$p$"),
        (files[1].0, 2, 18, "foo$x$"),
        (files[1].0, 2, 18, "[foo]"),
        (files[1].0, 2, 18, "\"@foo\""),
        (files[1].0, 2, 18, "\"[foo]\""),
        (files[1].0, 2, 18, "new}name"),
        (files[1].0, 2, 18, "new#comment"),
        (files[1].0, 2, 18, "new name"),
        (files[1].0, 2, 18, "\"unclosed"),
        (files[1].0, 2, 18, ""),
        (files[2].0, 1, 2, "new:key"),
        (files[2].0, 1, 2, "new\nkey"),
        (files[2].0, 1, 2, "new$ref"),
        (files[2].0, 1, 2, "\"new\""),
        (files[2].0, 1, 2, ""),
    ];
    let mut responses = Vec::new();
    for (rel, line, character, new_name) in &cases {
        let uri = server.uri(rel);
        responses.push(server.call(
            "textDocument/rename",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character }, "newName": new_name
            }),
        ));
    }
    server.stop();
    for ((_, _, _, new_name), response) in cases.iter().zip(responses) {
        assert_eq!(
            response["error"]["code"], -32803,
            "{new_name:?}: {response}"
        );
        assert!(response["result"].is_null(), "{new_name:?}: {response}");
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("replacement name"),
            "{response}"
        );
    }
}

#[test]
fn test_rename_valid_constant_name_preserves_parseability() {
    let files = [("common/decisions/a.txt", "@cost = 1\nx = @cost\n")];
    let result = feature_request(
        GOTO_RULES,
        &files,
        &[files[0].0],
        serde_json::json!({}),
        files[0].0,
        "textDocument/rename",
        serde_json::json!({ "position": { "line": 0, "character": 2 }, "newName": "@new.cost_1" }),
    );
    let rewritten = apply_rename_changes(&result, &files);
    assert_eq!(rewritten[files[0].0], "@new.cost_1 = 1\nx = @new.cost_1\n");
}

#[test]
fn test_rename_valid_type_names_preserve_parseability() {
    for new_name in ["NEW_FOCUS", "NEW.focus_1", "\"New focus_1\""] {
        let files = [
            ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
            (
                "common/decisions/a.txt",
                "adec = {\n    has_focus = MY_FOCUS\n}\n",
            ),
        ];
        let result = feature_request(
            GOTO_RULES,
            &files,
            &[files[1].0],
            serde_json::json!({}),
            files[1].0,
            "textDocument/rename",
            serde_json::json!({ "position": { "line": 1, "character": 18 }, "newName": new_name }),
        );
        let rewritten = apply_rename_changes(&result, &files);
        assert_eq!(
            rewritten[files[0].0],
            format!("{new_name} = {{ x = yes }}\n")
        );
        assert_eq!(
            rewritten[files[1].0],
            format!("adec = {{\n    has_focus = {new_name}\n}}\n")
        );
    }
}

#[test]
fn test_rename_quoted_type_targets_preserve_parseability() {
    let files = [
        (
            "common/national_focus/f.txt",
            "\"MY_FOCUS\" = { x = yes }\n",
        ),
        (
            "common/decisions/a.txt",
            "adec = {\n    has_focus = \"MY_FOCUS\"\n}\n",
        ),
    ];
    let result = feature_request(
        GOTO_RULES,
        &files,
        &[files[1].0],
        serde_json::json!({}),
        files[1].0,
        "textDocument/rename",
        serde_json::json!({ "position": { "line": 1, "character": 18 }, "newName": "\"New focus_1\"" }),
    );
    let rewritten = apply_rename_changes(&result, &files);
    assert_eq!(rewritten[files[0].0], "\"New focus_1\" = { x = yes }\n");
    assert_eq!(
        rewritten[files[1].0],
        "adec = {\n    has_focus = \"New focus_1\"\n}\n"
    );
}

#[test]
fn test_rename_valid_localisation_name_preserves_parseability() {
    let files = [
        (
            "localisation/test_l_english.yml",
            "l_english:\n my_key:0 \"Hello\"\n my_key_desc:0 \"$my_key$\"\n",
        ),
        ("common/test/a.txt", "x = my_key\n"),
    ];
    let result = feature_request(
        GOTO_RULES,
        &files,
        &[files[0].0],
        serde_json::json!({}),
        files[0].0,
        "textDocument/rename",
        serde_json::json!({ "position": { "line": 1, "character": 2 }, "newName": "new.key_1" }),
    );
    let rewritten = apply_rename_changes(&result, &files);
    assert_eq!(
        rewritten[files[0].0],
        "l_english:\n new.key_1:0 \"Hello\"\n new.key_1_desc:0 \"$new.key_1$\"\n"
    );
    assert_eq!(rewritten[files[1].0], "x = new.key_1\n");
}

/// Apply the wire edits to ASCII fixtures, then parse every changed document.
fn apply_rename_changes(
    result: &serde_json::Value,
    files: &[(&str, &str)],
) -> HashMap<String, String> {
    let changes = result["changes"].as_object().expect("rename changes");
    let mut rewritten = HashMap::new();
    for (uri, edits) in changes {
        let (rel, original) = files.iter().find(|(rel, _)| uri.ends_with(rel)).unwrap();
        assert!(original.is_ascii());
        let offset = |position: &serde_json::Value| {
            original
                .split_inclusive('\n')
                .take(position["line"].as_u64().unwrap() as usize)
                .map(str::len)
                .sum::<usize>()
                + position["character"].as_u64().unwrap() as usize
        };
        let mut edits: Vec<_> = edits.as_array().unwrap().iter().collect();
        edits.sort_by_key(|edit| std::cmp::Reverse(offset(&edit["range"]["start"])));
        let mut text = original.to_string();
        for edit in edits {
            text.replace_range(
                offset(&edit["range"]["start"])..offset(&edit["range"]["end"]),
                edit["newText"].as_str().unwrap(),
            );
        }
        if rel.ends_with(".yml") {
            let parsed = cwtools_localization::parse_loc_files(rel, &text, None).unwrap();
            assert!(!parsed.is_empty());
            assert!(
                parsed.iter().all(|file| file.parse_errors.is_empty()),
                "{text}"
            );
        } else {
            let table = cwtools_string_table::string_table::StringTable::new();
            let parsed = cwtools_parser::parser::parse_string(&text, &table);
            assert!(parsed.errors.is_empty(), "{text}: {:?}", parsed.errors);
        }
        rewritten.insert(rel.to_string(), text);
    }
    rewritten
}

#[test]
fn test_prepare_rename_covers_at_constant_token() {
    let doc = "@my_const = 5\nadec = {\n    x = @my_const\n}\n";
    let files = &[("common/decisions/d.txt", doc)];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/d.txt"],
        serde_json::json!({}),
        "common/decisions/d.txt",
        "textDocument/prepareRename",
        serde_json::json!({ "position": { "line": 2, "character": 10 } }),
    );
    assert_eq!(result["start"]["character"], 8, "token start: {}", result);
    assert_eq!(result["end"]["character"], 17, "token end: {}", result);
}

#[test]
fn test_rename_targets_value_not_trailing_comment() {
    // Regression: a use site whose line repeats the instance name in a trailing
    // comment must rename the VALUE (col 16), never the comment occurrence. The
    // old value-column scan took the LAST match on the raw line, so it wrote the
    // new text into the comment and left the real value dangling (silent
    // corruption). The value is resolved as the first token after the `=`.
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        (
            "common/decisions/a.txt",
            "adec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
        (
            "common/decisions/b.txt",
            "bdec = {\n    has_focus = MY_FOCUS   # keep MY_FOCUS until 1939\n}\n",
        ),
    ];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/a.txt"],
        serde_json::json!({}),
        "common/decisions/a.txt",
        "textDocument/rename",
        serde_json::json!({ "position": { "line": 1, "character": 16 }, "newName": "NEW_FOCUS" }),
    );
    let changes = result["changes"]
        .as_object()
        .expect("WorkspaceEdit changes");
    let b_key = changes
        .keys()
        .find(|u| u.ends_with("decisions/b.txt"))
        .unwrap_or_else(|| panic!("rename must edit closed file b.txt, got: {}", result));
    let edits = changes[b_key].as_array().expect("edits for b.txt");
    assert_eq!(
        edits[0]["range"]["start"]["character"], 16,
        "edit must target the value column, not the trailing comment, got: {}",
        result
    );
    assert_eq!(edits[0]["newText"], "NEW_FOCUS");
}

#[test]
fn test_code_lens_resolves_reference_locations() {
    let definition = "FOCUS_A = { cost = 1 }\n";
    let use_site = "FOCUS_B = { prerequisite = FOCUS_A }\n";
    let definition_rel = "game/common/national_focus/definition.txt";
    let mut server = editor_server(&[
        (definition_rel, definition),
        ("game/common/national_focus/use.txt", use_site),
    ]);
    let uri = server.uri(definition_rel);

    let response = server.call(
        "textDocument/codeLens",
        json!({ "textDocument": { "uri": uri } }),
    );
    let lens = response["result"]
        .as_array()
        .and_then(|lenses| {
            lenses
                .iter()
                .find(|lens| lens["data"]["instanceName"] == "FOCUS_A")
        })
        .expect("FOCUS_A code lens")
        .clone();

    let response = server.call("codeLens/resolve", lens);
    server.stop();
    assert_eq!(response["result"]["command"]["title"], "1 reference");
    assert_eq!(
        response["result"]["command"]["command"],
        "cwtools.showReferences"
    );
    assert_eq!(response["result"]["command"]["arguments"][0], uri);
    assert_eq!(
        response["result"]["command"]["arguments"][2]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
}

#[test]
fn test_code_lens_counts_scripted_effect_calls() {
    let definition = "my_se = { log = \"hi\" }\n";
    let use_site = "my_dec = {\n    complete_effect = {\n        my_se = yes\n    }\n}\n";
    let definition_rel = "game/common/scripted_effects/e.txt";
    let mut server = editor_server_with_rules(
        GOTO_RULES,
        &[
            (definition_rel, definition),
            ("game/common/decisions/d.txt", use_site),
        ],
    );
    let uri = server.uri(definition_rel);

    let response = server.call(
        "textDocument/codeLens",
        json!({ "textDocument": { "uri": uri } }),
    );
    let lens = response["result"]
        .as_array()
        .and_then(|lenses| {
            lenses
                .iter()
                .find(|lens| lens["data"]["instanceName"] == "my_se")
        })
        .expect("my_se code lens")
        .clone();

    let response = server.call("codeLens/resolve", lens);
    server.stop();
    assert_eq!(
        response["result"]["command"]["title"], "1 reference",
        "scripted effect calls are keys, not values: {response}"
    );
    assert_eq!(
        response["result"]["command"]["arguments"][2]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
}

#[test]
fn call_hierarchy_indexes_scripted_effect_edges_and_recursive_calls() {
    let first = "first_effect = {\n    second_effect = yes\n    third_effect = yes\n}\n";
    let second = "second_effect = {\n    first_effect = yes\n}\n";
    let third = "third_effect = {\n    log = \"done\"\n}\n";
    let caller_a = "caller_a = {\n    first_effect = yes\n}\n";
    let caller_b = "caller_b = {\n    first_effect = yes\n}\n";
    let decision =
        "my_decision = {\n    complete_effect = {\n        first_effect = yes\n    }\n}\n";
    let first_rel = "game/common/scripted_effects/first.txt";
    let mut server = editor_server_with_rules(
        GOTO_RULES,
        &[
            (first_rel, first),
            ("game/common/scripted_effects/second.txt", second),
            ("game/common/scripted_effects/third.txt", third),
            ("game/common/scripted_effects/callers_a.txt", caller_a),
            ("game/common/scripted_effects/callers_b.txt", caller_b),
            ("game/common/decisions/d.txt", decision),
        ],
    );
    let uri = server.uri(first_rel);
    let prepared = server.call(
        "textDocument/prepareCallHierarchy",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 2 }
        }),
    );
    let item = prepared["result"][0].clone();
    assert_eq!(
        item["name"], "first_effect",
        "prepared callable: {prepared}"
    );
    assert_eq!(item["range"]["start"]["line"], 0);
    assert_eq!(item["range"]["end"]["line"], 4);
    assert_eq!(item["selectionRange"]["start"]["line"], 0);
    assert_eq!(item["selectionRange"]["start"]["character"], 0);
    assert_eq!(item["selectionRange"]["end"]["character"], 12);

    let incoming = server.call("callHierarchy/incomingCalls", json!({ "item": item }));
    let incoming = incoming["result"].as_array().expect("incoming call array");
    let callers = incoming
        .iter()
        .map(|call| call["from"]["name"].as_str().unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        callers,
        ["second_effect", "caller_a", "caller_b", "my_decision"].into()
    );
    let decision_call = incoming
        .iter()
        .find(|call| call["from"]["name"] == "my_decision")
        .unwrap();
    assert_eq!(decision_call["fromRanges"][0]["start"]["line"], 2);
    assert_eq!(decision_call["fromRanges"][0]["start"]["character"], 8);
    assert_eq!(decision_call["fromRanges"][0]["end"]["character"], 20);

    let first_item = prepared["result"][0].clone();
    let outgoing = server.call("callHierarchy/outgoingCalls", json!({ "item": first_item }));
    let outgoing = outgoing["result"].as_array().expect("outgoing call array");
    let targets = outgoing
        .iter()
        .map(|call| call["to"]["name"].as_str().unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(targets, ["second_effect", "third_effect"].into());
    assert!(
        outgoing
            .iter()
            .all(|call| call["fromRanges"].as_array().unwrap().len() == 1)
    );
    assert_eq!(outgoing[0]["fromRanges"][0]["start"]["line"], 1);
    assert_eq!(outgoing[0]["fromRanges"][0]["start"]["character"], 4);
    assert_eq!(outgoing[0]["fromRanges"][0]["end"]["character"], 17);
    server.stop();
}

#[test]
fn call_hierarchy_prepare_returns_none_for_non_callable_entity() {
    let doc = "my_focus = {\n    id = my_focus\n}\n";
    let rel = "game/common/national_focus/f.txt";
    let mut server = editor_server_with_rules(GOTO_RULES, &[(rel, doc)]);
    let uri = server.uri(rel);
    let response = server.call(
        "textDocument/prepareCallHierarchy",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 2 }
        }),
    );
    server.stop();
    assert!(
        response["result"].is_null(),
        "non-callable result: {response}"
    );
}

#[test]
fn call_hierarchy_supports_scripted_triggers() {
    const RULES: &str = r#"
types = { type[scripted_trigger] = { path = "game/common/scripted_triggers" } }
scripted_trigger = { alias_name[trigger] = alias_match_left[trigger] }
alias[trigger:<scripted_trigger>] = yes
"#;
    let outer = "outer_trigger = {\n    inner_trigger = yes\n}\n";
    let inner = "inner_trigger = {\n    always = yes\n}\n";
    let outer_rel = "game/common/scripted_triggers/outer.txt";
    let mut server = editor_server_with_rules(
        RULES,
        &[
            (outer_rel, outer),
            ("game/common/scripted_triggers/inner.txt", inner),
        ],
    );
    let uri = server.uri(outer_rel);
    let prepared = server.call(
        "textDocument/prepareCallHierarchy",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 2 }
        }),
    );
    let item = prepared["result"][0].clone();
    assert_eq!(
        item["name"], "outer_trigger",
        "prepared trigger: {prepared}"
    );
    let outgoing = server.call("callHierarchy/outgoingCalls", json!({ "item": item }));
    let outgoing = outgoing["result"]
        .as_array()
        .expect("outgoing trigger calls");
    assert_eq!(outgoing.len(), 1, "trigger call edge: {outgoing:?}");
    assert_eq!(outgoing[0]["to"]["name"], "inner_trigger");
    server.stop();
}
