use serde_json::{Value, json};

use crate::common::{DECISION_RULES, ServerBuilder, TestServer, watched_created};

/// A `thing` type with two `## required` name-derived loc keys, `name` and
/// `desc` (`$_desc`).
const THING_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        localisation = {
            ## required
            name = "$"
            ## required
            desc = "$_desc"
        }
    }
}
thing = { x = scalar }
"#;

const THING_REL: &str = "common/things/test.txt";
const THING_TEXT: &str = "my_thing = { x = yes }\n";

/// A builder over `THING_RULES` and `my_thing` on disk, with an empty base game.
fn thing_builder() -> ServerBuilder {
    TestServer::builder()
        .rules_file("r.cwt", THING_RULES)
        .vanilla()
        .file(THING_REL, THING_TEXT)
}

/// Apply `edits` (line-descending, so earlier columns stay valid) to `text` and
/// return the result.
fn apply_line_edits(text: &str, edits: &[Value]) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut resolved: Vec<(usize, usize, usize, String)> = edits
        .iter()
        .map(|e| {
            (
                e["range"]["start"]["line"].as_u64().unwrap() as usize,
                e["range"]["start"]["character"].as_u64().unwrap() as usize,
                e["range"]["end"]["character"].as_u64().unwrap() as usize,
                e["newText"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    resolved.sort_by(|a, b| b.cmp(a));
    for (line, sc, ec, new_text) in resolved {
        let chars: Vec<char> = lines[line].chars().collect();
        let mut out: String = chars[..sc].iter().collect();
        out.push_str(&new_text);
        out.extend(chars[ec..].iter());
        lines[line] = out;
    }
    format!("{}\n", lines.join("\n"))
}

/// Run `fixAllWorkspace`, answering the `workspace/applyEdit` it sends. Returns
/// the response and the edit it applied, if any.
fn fix_all_workspace(server: &mut TestServer) -> (Value, Option<Value>) {
    server.send_request(
        2,
        "workspace/executeCommand",
        json!({ "command": "fixAllWorkspace", "arguments": [] }),
    );
    server.read_response_answering_apply_edit(2)
}

#[test]
fn test_source_fix_all_applies_every_fixable_diagnostic_at_once() {
    // Two empty `limit = { }` blocks -> two CW281 diagnostics, each carrying a
    // fix. `source.fixAll` must return ONE action holding both edits, and
    // applying them must give the same file the CLI `fix --apply` would.
    let rel = "common/decisions/test.txt";
    let text = "a = { limit = { } }\nb = { limit = { } }\n";
    let mut server = TestServer::builder()
        .rules_file("r.cwt", DECISION_RULES)
        .vanilla()
        .file(rel, text)
        .spawn_scanned();
    server.open(rel, text);

    // Capture the published diagnostics: the client round-trips them back on the
    // codeAction request, and the fix payloads ride along in `data`.
    let diagnostics = server.wait_for_diags(rel);
    let fixable: Vec<&Value> = diagnostics
        .iter()
        .filter(|d| d["data"].is_object())
        .collect();
    println!("fixable diagnostics = {}", fixable.len());
    assert_eq!(
        fixable.len(),
        2,
        "two empty limits -> two fixable diagnostics: {diagnostics:#?}"
    );

    let resp = server.call(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": server.uri(rel) },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 2, "character": 0 },
            },
            "context": { "diagnostics": diagnostics, "only": ["source.fixAll"] },
        }),
    );
    server.stop();
    println!("source.fixAll action = {}", resp["result"]);
    let actions = resp["result"].as_array().expect("actions");
    assert_eq!(actions.len(), 1, "only source.fixAll was requested: {resp}");
    assert_eq!(actions[0]["kind"], "source.fixAll");
    assert_eq!(actions[0]["title"], "Fix all (2 auto-fixable)");
    assert_eq!(
        actions[0]["diagnostics"].as_array().map(Vec::len),
        Some(2),
        "the action claims both diagnostics"
    );
    let edits = actions[0]["edit"]["changes"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(edits.len(), 2, "one edit per fixable diagnostic");

    // Apply the edits and check the result: both empty limits removed.
    let fixed = apply_line_edits(text, edits);
    println!("fixed source = {fixed:?}");
    assert_eq!(fixed, "a = { }\nb = { }\n");
}

#[test]
fn test_create_loc_key_code_action_inserts_after_sibling_key() {
    // A type declaring two `## required` name-derived loc keys; the mod's own
    // loc file already has one of them (`my_thing`). CW100 fires for the
    // other (`my_thing_desc`), and its code action must insert the missing
    // stub right after the sibling's definition — the cross-file
    // `WorkspaceEdit` this feature exists for.
    let loc_text = "l_english:\n my_thing:0 \"My Thing\"\n";
    let mut server = thing_builder()
        .file("localisation/things_l_english.yml", loc_text)
        .spawn_scanned();

    server.open(THING_REL, THING_TEXT);
    let diag = server.wait_for_diag_object(THING_REL, "CW100");
    assert!(
        diag["message"]
            .as_str()
            .is_some_and(|m| m.contains("my_thing_desc")),
        "got: {diag}"
    );

    let resp = server.code_actions(THING_REL, &diag, true);
    server.stop();

    let actions = resp["result"].as_array().expect("array result");
    assert_eq!(
        actions.len(),
        2,
        "the create-loc-key action plus the ignore action: {resp}"
    );
    let action = &actions[0];
    assert_eq!(action["title"], "Create localisation key my_thing_desc");
    assert_eq!(action["kind"], "quickfix");

    let ops = action["edit"]["documentChanges"]
        .as_array()
        .expect("documentChanges carries the cross-file edit");
    assert_eq!(ops.len(), 1, "no sibling file needed creating: {resp}");
    let op = &ops[0];
    assert!(
        op["textDocument"]["uri"]
            .as_str()
            .is_some_and(|u| u.ends_with("things_l_english.yml")),
        "got: {op}"
    );
    let edits = op["edits"].as_array().expect("edits array");
    assert_eq!(edits.len(), 1);
    let e = &edits[0];
    // Inserted right after the sibling's line (`l_english:` is line 0,
    // ` my_thing:0 "My Thing"` is line 1), at the start of the next line.
    assert_eq!(e["range"]["start"]["line"], 2);
    assert_eq!(e["range"]["start"]["character"], 0);
    assert_eq!(
        e["range"]["start"], e["range"]["end"],
        "an empty (insertion) range"
    );
    assert_eq!(e["newText"], " my_thing_desc:0 \"TODO\"\n");

    // Applying it reproduces the expected file.
    let mut lines: Vec<&str> = loc_text.lines().collect();
    lines.insert(2, " my_thing_desc:0 \"TODO\"");
    let fixed = format!("{}\n", lines.join("\n"));
    assert_eq!(
        fixed,
        "l_english:\n my_thing:0 \"My Thing\"\n my_thing_desc:0 \"TODO\"\n"
    );
}

#[test]
fn test_create_loc_key_code_action_creates_a_new_loc_file_when_the_workspace_has_none() {
    // The third resolution tier (`LocInsertTarget::NewFile`): the workspace
    // has no loc file at all, so the create-loc-key action must add a
    // `Create` resource op alongside the insertion edit, and the inserted
    // text must open with the BOM the game requires for a loc file to load —
    // nothing else in the suite pins that byte.
    //
    // CW100 is gated on the merged loc index being non-empty
    // (`append_missing_loc_errors`), so an empty workspace with no loc
    // anywhere would suppress it entirely. The base-game (vanilla) dir is
    // given a loc file instead: it merges into the union (index non-empty,
    // CW100 fires) but sits outside `editable_roots`, so the edit boundary
    // refuses it for the sibling-site and existing-file tiers alike and
    // resolution falls through to NewFile.
    //
    // Only vanilla defines `my_thing`'s name loc — the workspace has no
    // `localisation/` directory at all.
    let mut server = thing_builder()
        .vanilla_file(
            "localisation/things_l_english.yml",
            "l_english:\n my_thing:0 \"My Thing\"\n",
        )
        .spawn_scanned();

    server.open(THING_REL, THING_TEXT);
    let diag = server.wait_for_diag_object(THING_REL, "CW100");
    assert!(
        diag["message"]
            .as_str()
            .is_some_and(|m| m.contains("my_thing_desc")),
        "got: {diag}"
    );

    let resp = server.code_actions(THING_REL, &diag, true);
    server.stop();

    let actions = resp["result"].as_array().expect("array result");
    assert_eq!(
        actions.len(),
        2,
        "the create-loc-key action plus the ignore action: {resp}"
    );
    let action = &actions[0];
    assert_eq!(action["title"], "Create localisation key my_thing_desc");
    assert_eq!(action["kind"], "quickfix");

    let ops = action["edit"]["documentChanges"]
        .as_array()
        .expect("documentChanges carries the cross-file edit");
    assert_eq!(ops.len(), 2, "a Create op plus the insertion edit: {resp}");

    let create_op = &ops[0];
    assert_eq!(create_op["kind"], "create");
    assert!(
        create_op["uri"]
            .as_str()
            .is_some_and(|u| u.ends_with("localisation/cwtools_generated_l_english.yml")),
        "got: {create_op}"
    );
    assert_eq!(create_op["options"]["overwrite"], false);
    assert_eq!(create_op["options"]["ignoreIfExists"], true);

    let edit_op = &ops[1];
    assert_eq!(edit_op["textDocument"]["uri"], create_op["uri"]);
    let edits = edit_op["edits"].as_array().expect("edits array");
    assert_eq!(edits.len(), 1);
    let e = &edits[0];
    assert_eq!(e["range"]["start"]["line"], 0);
    assert_eq!(e["range"]["start"]["character"], 0);
    assert_eq!(
        e["range"]["start"], e["range"]["end"],
        "an empty (insertion) range"
    );
    assert_eq!(
        e["newText"], "\u{FEFF}l_english:\n my_thing_desc:0 \"TODO\"\n",
        "the new file's content must open with the BOM the game requires"
    );
}

#[test]
fn test_create_loc_key_code_action_batch_dedupes_a_shared_new_file() {
    // #142: `name` and `desc` are BOTH missing (unlike the two tests above,
    // where one sibling already has a definition), and no loc file for the
    // language exists anywhere the edit boundary would accept — so both
    // diagnostics resolve to the identical generated `NewFile` target. One
    // codeAction request returning both diagnostics at once (an "instance
    // missing both title and desc" batch, no diagnostics round trip between
    // them) must not offer two independent actions that would each insert
    // their own BOM + header: applying the response must produce the header
    // exactly once, with both stubs.
    //
    // An unrelated vanilla key only: the merged loc index is non-empty (CW100's
    // gate) but neither `my_thing` nor `my_thing_desc` resolves anywhere.
    let cache_dir = tempfile::tempdir().unwrap();
    let mut server = thing_builder()
        .vanilla_file(
            "localisation/other_l_english.yml",
            "l_english:\n unrelated_key:0 \"Unrelated\"\n",
        )
        .init_opts(json!({ "cacheDir": cache_dir.path().to_string_lossy() }))
        .spawn_scanned();

    server.open(THING_REL, THING_TEXT);
    let diagnostics = server.wait_for_diags(THING_REL);
    let cw100: Vec<Value> = diagnostics
        .into_iter()
        .filter(|d| d["code"] == "CW100")
        .collect();
    assert_eq!(
        cw100.len(),
        2,
        "both name and desc must be missing: {cw100:#?}"
    );

    let resp = server.call(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": server.uri(THING_REL) },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 1, "character": 0 },
            },
            "context": { "diagnostics": cw100, "only": ["quickfix"] },
        }),
    );
    server.stop();

    let actions = resp["result"].as_array().expect("array result");
    assert_eq!(
        actions.len(),
        2,
        "the second key must be absorbed into the first's action, not offered \
         as a second independent NewFile action — only the ignore action joins: {resp}"
    );

    let ops = actions[0]["edit"]["documentChanges"]
        .as_array()
        .expect("documentChanges carries the cross-file edit");
    assert_eq!(
        ops.len(),
        2,
        "one Create plus a SINGLE TextDocumentEdit for the file, not one per key: {resp}"
    );
    assert_eq!(ops[0]["kind"], "create");

    // Both inserts share the (0,0) start position within the one
    // TextDocumentEdit: per the LSP spec, array order (not sequential
    // mutation) decides the order they appear in the resulting text, so
    // concatenating `newText` in array order reproduces what a client
    // applying this edit would see.
    let edits = ops[1]["edits"].as_array().expect("edits array");
    assert_eq!(
        edits.len(),
        2,
        "the header edit and the folded stub, as two edits in one TextDocumentEdit: {resp}"
    );
    let applied: String = edits
        .iter()
        .map(|e| e["newText"].as_str().unwrap())
        .collect();
    assert_eq!(
        applied.matches("l_english:").count(),
        1,
        "the header must appear exactly once: {applied:?}"
    );
    assert!(applied.contains(" my_thing:0 "), "got: {applied:?}");
    assert!(applied.contains(" my_thing_desc:0 "), "got: {applied:?}");
}

#[test]
fn test_create_loc_key_code_action_reresolves_once_the_new_file_exists() {
    // Step 4 of #142: the SAFE one-at-a-time flow. Applying the first action
    // creates the generated file; a FRESH codeAction request afterward (a
    // real diagnostics round trip, unlike the in-batch case above) must
    // resolve via `ExistingFileAppend`, not `NewFile` again — no second
    // header, even for the exact same diagnostic re-requested.
    let mut server = thing_builder()
        .vanilla_file(
            "localisation/things_l_english.yml",
            "l_english:\n my_thing:0 \"My Thing\"\n",
        )
        .spawn_scanned();

    server.open(THING_REL, THING_TEXT);
    let diag = server.wait_for_diag_object(THING_REL, "CW100");

    let first_resp = server.code_actions(THING_REL, &diag, true);
    let first_actions = first_resp["result"].as_array().expect("array result");
    assert_eq!(first_actions.len(), 2);
    let first_ops = first_actions[0]["edit"]["documentChanges"]
        .as_array()
        .expect("documentChanges");
    assert_eq!(first_ops.len(), 2, "Create + header insert: {first_resp}");
    let generated_uri = first_ops[0]["uri"]
        .as_str()
        .expect("create uri")
        .to_string();
    let generated_new_text = first_ops[1]["edits"][0]["newText"]
        .as_str()
        .expect("insert text")
        .to_string();

    // Simulate the client applying that first action: write the file exactly
    // as the edit describes.
    let generated_path = tower_lsp::lsp_types::Url::parse(&generated_uri)
        .unwrap()
        .to_file_path()
        .unwrap();
    std::fs::create_dir_all(generated_path.parent().unwrap()).unwrap();
    std::fs::write(&generated_path, &generated_new_text).unwrap();

    // A real client's file watcher (which covers loc `*.yml`) fires a CREATED
    // event when the action's file lands on disk; that invalidates the
    // loc-discovery cache so the next request re-walks and sees the file.
    server.send(&watched_created(std::slice::from_ref(&generated_uri)));

    let second_resp = server.code_actions(THING_REL, &diag, true);
    server.stop();

    let second_actions = second_resp["result"].as_array().expect("array result");
    assert_eq!(second_actions.len(), 2);
    let second_ops = second_actions[0]["edit"]["documentChanges"]
        .as_array()
        .expect("documentChanges");
    assert_eq!(
        second_ops.len(),
        1,
        "the file exists now, so no Create op and no second header: {second_resp}"
    );
    let second_edit = &second_ops[0]["edits"][0];
    assert!(
        !second_edit["newText"]
            .as_str()
            .unwrap()
            .contains("l_english:"),
        "re-resolving after the file exists must not re-emit the header: {second_resp}"
    );
}

#[test]
fn test_fix_all_workspace_applies_every_fixable_diagnostic() {
    // Two empty `limit = { }` blocks -> two CW281 diagnostics, each carrying a
    // fix. `fixAllWorkspace` must send one `workspace/applyEdit` covering both,
    // then report how many fixes/files it applied — the workspace-wide
    // counterpart of `source.fixAll` (and of `cwtools fix --apply`).
    let rel = "common/decisions/test.txt";
    let text = "a = { limit = { } }\nb = { limit = { } }\n";
    let mut server = TestServer::builder()
        .rules_file("r.cwt", DECISION_RULES)
        .vanilla()
        .file(rel, text)
        .spawn_scanned();
    server.open(rel, text);
    server.wait_for_diags(rel);

    let (resp, applied_edit) = fix_all_workspace(&mut server);
    server.stop();

    assert_eq!(
        resp["result"].as_str(),
        Some("Applied 2 fix(es) across 1 file(s)"),
        "got: {resp}"
    );

    let edit = applied_edit.expect("the server must call workspace/applyEdit");
    let changes = edit["changes"].as_object().expect("changes map");
    assert_eq!(changes.len(), 1, "one file touched: {edit}");
    let edits = changes.values().next().unwrap().as_array().unwrap();
    assert_eq!(edits.len(), 2, "one edit per fixable diagnostic");

    let fixed = apply_line_edits(text, edits);
    assert_eq!(fixed, "a = { }\nb = { }\n");
}

#[test]
fn test_fix_all_workspace_skips_stale_closed_file_edits() {
    let rel = "common/decisions/test.txt";
    let mut server = TestServer::builder()
        .rules_file("r.cwt", DECISION_RULES)
        .vanilla()
        .file(rel, "a = { limit = { } }\n")
        .spawn_scanned();

    // Change a closed file after its diagnostic was published. The old fix range
    // must not be applied to the new contents.
    server.write_file(rel, "a = { limit = { x = 1 } }\n");
    let (resp, applied_edit) = fix_all_workspace(&mut server);
    server.stop();

    assert!(applied_edit.is_none(), "stale fixes must not be applied");
    assert_eq!(
        resp["result"].as_str(),
        Some("Applied 0 fix(es) across 0 file(s); 1 skipped (stale)"),
        "got: {resp}"
    );
}

/// A workspace whose one decision file carries a fixable CW281, initialized and
/// scanned. The scan's diagnostics for the file are asserted on the way past:
/// they are what puts it in the `fixAllWorkspace` store, so a test that never
/// saw them would pass on an empty store for the wrong reason. They are read
/// before `wait_for_scan_done` because the scan publishes them before closing
/// its loading bar, and that drain would swallow them.
fn fixable_server() -> TestServer {
    let mut server = TestServer::builder()
        .rules_file("r.cwt", DECISION_RULES)
        .vanilla()
        .file("common/decisions/test.txt", "a = { limit = { } }\n")
        .spawn();
    server.initialized();
    let scanned = server.wait_for_diags("test.txt");
    assert!(
        scanned.iter().any(|d| d["code"] == "CW281"),
        "the fixture must publish a fixable CW281, got: {scanned:?}"
    );
    server.wait_for_scan_done();
    server
}

/// The `fixAllWorkspace` result for a server whose only fixable file is gone
/// from the Problems panel: the store must be empty, not merely stale.
const NOTHING_FIXABLE: &str = "No auto-fixable problems in the workspace.";

#[test]
fn test_deleting_a_watched_file_drops_its_fixable_edits() {
    // The DELETE batch publishes empty diagnostics for the file. That publish
    // owns the `fixAllWorkspace` store too, or the deleted file's fixes outlive
    // the diagnostics they came from (#133).
    let mut server = fixable_server();
    let rel = "common/decisions/test.txt";
    let uri = server.uri(rel);

    std::fs::remove_file(server.path(rel)).unwrap();
    server.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": uri, "type": 3 }] }),
    );
    let cleared = server.wait_for_diags("test.txt");
    assert!(cleared.is_empty(), "the delete must clear the panel");

    let (resp, applied_edit) = fix_all_workspace(&mut server);
    server.stop();

    assert!(applied_edit.is_none(), "a deleted file has nothing to fix");
    assert_eq!(
        resp["result"].as_str(),
        Some(NOTHING_FIXABLE),
        "got: {resp}"
    );
}

#[test]
fn test_closing_a_document_drops_its_fixable_edits() {
    // did_close's empty publish must take the store entry with it. The entry is
    // keyed by the open buffer's version, so leaving it behind survives until
    // some later reopen matches that version against different content (#133).
    let mut server = fixable_server();
    let rel = "common/decisions/test.txt";

    server.notify(
        "textDocument/didOpen",
        json!({"textDocument":{"uri":server.uri(rel),"languageId":"hoi4","version":7,
            "text": "a = { limit = { } }\n"}}),
    );
    let open_diags = server.wait_for_diags("test.txt");
    assert!(
        open_diags.iter().any(|d| d["code"] == "CW281"),
        "expected a fixable CW281 while open, got: {open_diags:?}"
    );

    server.close(rel);
    let cleared = server.wait_for_diags("test.txt");
    assert!(cleared.is_empty(), "the close must clear the panel");

    let (resp, applied_edit) = fix_all_workspace(&mut server);
    server.stop();

    assert!(
        applied_edit.is_none(),
        "a closed file's diagnostics were cleared, so nothing may be applied"
    );
    assert_eq!(
        resp["result"].as_str(),
        Some(NOTHING_FIXABLE),
        "got: {resp}"
    );
}

#[test]
fn test_fix_all_workspace_reports_nothing_to_fix() {
    // No diagnostics ever published -> the store is empty -> the command must
    // say so without sending a `workspace/applyEdit` at all.
    let mut server = TestServer::builder().spawn_scanned();

    let (resp, applied_edit) = fix_all_workspace(&mut server);
    server.stop();
    assert!(
        applied_edit.is_none(),
        "nothing to fix -> no applyEdit call"
    );
    assert_eq!(
        resp["result"].as_str(),
        Some("No auto-fixable problems in the workspace."),
        "got: {resp}"
    );
}

#[test]
fn test_fix_all_workspace_finds_nothing_once_the_applied_fix_is_reopened_clean() {
    // Full lifecycle: fixAllWorkspace applies a fix, the client applies it (a
    // didChange with the fixed text on the still-open document), the server
    // republishes without the diagnostic — which also drops the entry from
    // the `fixable_edits` store, since `publish_filtered` updates the store
    // before it publishes (validate.rs) — and a second fixAllWorkspace call
    // must then report nothing left to fix rather than resending the
    // already-applied edit.
    let rel = "common/decisions/test.txt";
    let text = "a = { limit = { } }\n";
    let mut server = TestServer::builder()
        .rules_file("r.cwt", DECISION_RULES)
        .vanilla()
        .file(rel, text)
        .spawn_scanned();
    server.open(rel, text);
    let before = server.wait_for_diags(rel);
    assert!(
        before.iter().any(|d| d["code"] == "CW281"),
        "expected CW281 before the fix, got: {before:?}"
    );

    let (resp, applied_edit) = fix_all_workspace(&mut server);
    assert_eq!(
        resp["result"].as_str(),
        Some("Applied 1 fix(es) across 1 file(s)"),
        "got: {resp}"
    );
    assert!(
        applied_edit.is_some(),
        "the server must call workspace/applyEdit"
    );

    // Simulate the client having applied that edit itself.
    server.change(rel, 2, "a = { }\n");
    let after = server.diag_codes_for(rel, 1);
    assert!(
        !after.contains(&"CW281".to_string()),
        "the fixed text must not still report CW281, got: {:?}",
        after
    );

    let (resp2, applied_edit2) = fix_all_workspace(&mut server);
    server.stop();
    assert!(
        applied_edit2.is_none(),
        "nothing left to fix -> no second applyEdit call"
    );
    assert_eq!(
        resp2["result"].as_str(),
        Some("No auto-fixable problems in the workspace."),
        "got: {resp2}"
    );
}

/// A hostile workspace ships `localisation/things_l_english.yml` as a symlink to
/// a file outside it. The loc walk rejects the symlink (#161), so the sibling
/// key it defines is never indexed and CW100 fires for both `name` and `desc`
/// — and the create-key action must not offer to write through the link
/// (#160). It falls through to the new-file tier instead, which lands inside
/// the workspace.
#[cfg(unix)]
#[test]
fn test_create_loc_key_action_refuses_a_symlinked_loc_file() {
    let outside = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();

    // The real loc file lives outside the workspace; only a link to it is in.
    let target = outside.path().join("things_l_english.yml");
    std::fs::write(&target, "l_english:\n my_thing:0 \"My Thing\"\n").unwrap();

    // A real vanilla loc file keeps the loc index non-empty so CW100's gate
    // fires; the symlinked file is rejected and contributes nothing.
    let mut server = thing_builder()
        .vanilla_file(
            "localisation/other_l_english.yml",
            "l_english:\n unrelated_key:0 \"Unrelated\"\n",
        )
        .init_opts(json!({ "cacheDir": cache_dir.path().to_string_lossy() }))
        .spawn();
    let link = server.path("localisation/things_l_english.yml");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    server.initialized();
    server.wait_for_scan_done();
    server.open(THING_REL, THING_TEXT);

    // The symlinked loc file is rejected by the scan (#161), so neither
    // `my_thing` nor `my_thing_desc` resolves — both are missing. Without that
    // this test would pass vacuously on a workspace where nothing was indexed.
    let diags = server.wait_for_diags(THING_REL);
    let cw100: Vec<Value> = diags.into_iter().filter(|d| d["code"] == "CW100").collect();
    assert_eq!(
        cw100.len(),
        2,
        "both name and desc must be missing (the symlinked loc file is not indexed): {cw100:#?}"
    );
    let diag = cw100
        .iter()
        .find(|d| {
            d["message"]
                .as_str()
                .is_some_and(|m| m.contains("my_thing_desc"))
        })
        .cloned()
        .expect("CW100 for my_thing_desc");

    let resp = server.code_actions(THING_REL, &diag, true);
    server.stop();

    let actions = resp["result"].as_array().expect("array result");
    assert_eq!(
        actions.len(),
        2,
        "the create-loc-key action plus the ignore action: {resp}"
    );
    let ops = actions[0]["edit"]["documentChanges"]
        .as_array()
        .expect("documentChanges carries the cross-file edit");
    let targets: Vec<&str> = ops
        .iter()
        .filter_map(|op| {
            op["uri"]
                .as_str()
                .or_else(|| op["textDocument"]["uri"].as_str())
        })
        .collect();
    assert!(
        !targets.iter().any(|u| u.ends_with("things_l_english.yml")),
        "must not edit through the symlink, got: {targets:?}"
    );
    let outside_uri = crate::common::path_uri(outside.path());
    assert!(
        !targets.iter().any(|u| u.starts_with(&outside_uri)),
        "must not edit outside the workspace, got: {targets:?}"
    );
    assert!(
        targets
            .iter()
            .all(|u| u.ends_with("localisation/cwtools_generated_l_english.yml")),
        "falls through to a new in-workspace file, got: {targets:?}"
    );
}

/// Which of `rel_paths` published a diagnostic with `code` during the initial
/// scan, in the order seen. The scan publishes before it clears the loading
/// bar, so this replaces `wait_for_scan_done` rather than following it —
/// which also means a file the scan never validated shows up as a missing
/// entry when the bar goes off, instead of blocking on a frame that never
/// arrives.
#[cfg(unix)]
fn scan_diag_paths(server: &mut TestServer, rel_paths: &[&str], code: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    server.read_until("the scan to close its loading bar", |v| {
        if v["method"] == "textDocument/publishDiagnostics" {
            let uri = v["params"]["uri"].as_str().unwrap_or("");
            let carries_code = v["params"]["diagnostics"]
                .as_array()
                .is_some_and(|ds| ds.iter().any(|d| d["code"] == code));
            if carries_code
                && let Some(rel) = rel_paths.iter().find(|rel| uri.ends_with(**rel))
                && !seen.iter().any(|s| s == rel)
            {
                seen.push((*rel).to_string());
            }
        }
        v["method"] == "loadingBar" && v["params"]["enable"] == Value::Bool(false)
    });
    seen
}

/// `fixAllWorkspace` collects every URI that published a fixable diagnostic.
/// The workspace scan rejects symlinks (#161), so a linked file is never
/// validated and never enters the fixAllWorkspace store; the generated
/// `applyEdit` must only touch the real in-workspace file.
///
/// The link points into the base-game install on purpose. That is the case the
/// read boundary (#163) cannot catch: the install is a legitimate place to read
/// from, so the file's text resolves and the edit would be planned. Only a
/// separate, workspace-only *edit* boundary refuses it.
#[cfg(unix)]
#[test]
fn test_fix_all_workspace_skips_a_symlinked_file() {
    let real_rel = "common/decisions/real.txt";
    // A file in the base-game install, reachable only through a link inside the
    // workspace — readable, and so plannable, but never writable.
    let linked_rel = "common/decisions/linked.txt";
    let mut server = TestServer::builder()
        .rules_file("r.cwt", DECISION_RULES)
        .vanilla_file("linked.txt", "b = { limit = { } }\n")
        .file(real_rel, "a = { limit = { } }\n")
        .spawn();
    let target = server.vanilla_dir().join("linked.txt");
    std::os::unix::fs::symlink(&target, server.path(linked_rel)).unwrap();
    server.initialized();

    // The scan rejects the symlink (#161), so the linked file is never
    // validated and never enters the fixAllWorkspace store. The real file is
    // still fixable.
    let validated = scan_diag_paths(&mut server, &[real_rel, linked_rel], "CW281");
    assert!(
        validated.iter().any(|p| p == real_rel),
        "the real file must be fixable too, saw: {validated:?}"
    );
    assert!(
        !validated.iter().any(|p| p == linked_rel),
        "the scan must reject the symlink and not publish a diagnostic for it, saw: {validated:?}"
    );

    let (resp, applied_edit) = fix_all_workspace(&mut server);
    server.stop();

    let edit = applied_edit.expect("the server must call workspace/applyEdit");
    let changes = edit["changes"].as_object().expect("changes map");
    let touched: Vec<&String> = changes.keys().collect();
    assert!(
        touched.iter().all(|u| u.ends_with("real.txt")),
        "only the real in-workspace file may be edited, got: {touched:?}"
    );
    assert_eq!(
        resp["result"].as_str(),
        Some("Applied 1 fix(es) across 1 file(s)"),
        "got: {resp}"
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "b = { limit = { } }\n",
        "the base-game file is untouched"
    );
}
