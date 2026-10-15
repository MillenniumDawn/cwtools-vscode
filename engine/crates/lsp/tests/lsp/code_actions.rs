use std::time::Duration;

use serde_json::Value;

use crate::common::{DECISION_RULES, TestServer, publish_has_code};

/// A server over `DECISION_RULES` and an empty base game (so the index counts as
/// complete) with `files` on disk, past its startup scan.
fn decision_server(files: &[(&str, &str)]) -> TestServer {
    let mut builder = TestServer::builder()
        .rules_file("r.cwt", DECISION_RULES)
        .vanilla();
    for (rel, text) in files {
        builder = builder.file(rel, *text);
    }
    builder.spawn_scanned()
}

#[test]
fn test_code_action_quickfix_from_diagnostic() {
    // End-to-end: an empty `limit = { }` produces a CW281 diagnostic carrying a
    // fix payload in `data`. Round-tripping that diagnostic back through
    // textDocument/codeAction must yield one QUICKFIX whose edit, applied to the
    // source, deletes the empty limit — the same result as the CLI `fix`.
    let rel_path = "common/decisions/test.txt";
    let text = "x = { limit = { } }\n";
    let mut server = decision_server(&[(rel_path, text)]);
    // Capability advertised: quickfix code actions, no resolve step.
    let ca = &server.init_response()["result"]["capabilities"]["codeActionProvider"];
    assert_eq!(ca["codeActionKinds"][0], "quickfix", "got: {ca}");
    assert_eq!(ca["resolveProvider"], false, "got: {ca}");

    server.open(rel_path, text);
    let diag = server.wait_for_diag_object(rel_path, "CW281");
    assert!(
        diag.get("data").is_some(),
        "the diagnostic must carry a fix payload in `data`: {diag}"
    );

    let resp = server.code_actions(rel_path, &diag, false);
    server.stop();

    assert_eq!(resp["id"], 2, "got: {resp}");
    let all = resp["result"].as_array().expect("array result");
    // An unfiltered request also carries the `source.fixAll` and
    // ignore-in-workspace actions; this test is about the per-diagnostic
    // quickfix.
    let actions: Vec<&Value> = all.iter().filter(|a| a["kind"] == "quickfix").collect();
    assert_eq!(actions.len(), 2, "the fix plus the ignore action: {resp}");
    let action = actions[0];
    assert_eq!(action["kind"], "quickfix");
    assert_eq!(action["title"], "Remove empty limit");

    // The edit must reproduce the CLI `fix` output when applied.
    let changes = action["edit"]["changes"]
        .as_object()
        .expect("edit carries a changes map");
    let edits = changes
        .values()
        .next()
        .and_then(|v| v.as_array())
        .expect("edits for the document");
    assert_eq!(edits.len(), 1, "one text edit: {resp}");
    let e = &edits[0];
    let sc = e["range"]["start"]["character"].as_u64().unwrap() as usize;
    let ec = e["range"]["end"]["character"].as_u64().unwrap() as usize;
    let new_text = e["newText"].as_str().unwrap();
    // The fix is confined to line 0; splice the char range on that line.
    let nl = text.find('\n').unwrap();
    let chars: Vec<char> = text[..nl].chars().collect();
    let mut fixed: String = chars[..sc].iter().collect();
    fixed.push_str(new_text);
    fixed.extend(chars[ec..].iter());
    fixed.push_str(&text[nl..]);
    assert_eq!(
        fixed, "x = { }\n",
        "applied edit must delete the empty limit"
    );
}

#[test]
fn test_inline_ignore_directive_suppresses_a_keystroke_diagnostic() {
    // The same file opened twice, once with `# cwtools-ignore CW281` trailing
    // the offending line. The plain copy proves the setup fires CW281; the
    // directive'd copy must publish it never.
    let clean_rel = "common/decisions/clean.txt";
    let clean_text = "x = { limit = { } }\n";
    let ignored_rel = "common/decisions/ignored.txt";
    let ignored_text = "x = { limit = { } } # cwtools-ignore CW281\n";
    let mut server = decision_server(&[(clean_rel, clean_text), (ignored_rel, ignored_text)]);
    assert!(server.init_response()["result"]["capabilities"]["codeActionProvider"].is_object());

    // The plain copy proves the setup fires CW281.
    server.open(clean_rel, clean_text);
    let diag = server.wait_for_diag_object(clean_rel, "CW281");
    assert!(diag.get("data").is_some());

    // The directive'd copy must not publish it. The first publish for the
    // file after did_open is the keystroke validation result; a later one
    // (e.g. from a dependent sweep) would be filtered the same way, but the
    // first is enough to prove the directive is honored.
    server.open(ignored_rel, ignored_text);
    let publish = server.publish_for(ignored_rel, 1);
    assert!(
        !publish_has_code(&publish, "CW281"),
        "directive must suppress CW281: {publish}"
    );
    server.stop();
}

#[test]
fn test_inline_ignore_directive_suppresses_a_scan_diagnostic() {
    // Closed files: the workspace scan validates them from disk, and the
    // directive must filter its publish the same way the keystroke path does.
    // The plain copy proves the scan publishes CW281 at all.
    let clean_rel = "common/decisions/clean.txt";
    let ignored_rel = "common/decisions/ignored.txt";
    let mut server = TestServer::builder()
        .rules_file("r.cwt", DECISION_RULES)
        .vanilla()
        .file(clean_rel, "x = { limit = { } }\n")
        .file(ignored_rel, "x = { limit = { } } # cwtools-ignore CW281\n")
        .spawn();
    server.initialized();

    // Capture every publish the startup scan sends, up to its bar-off.
    let mut clean_published = false;
    let mut ignored_bad: Vec<Value> = Vec::new();
    server.read_until_within(Duration::from_secs(60), "the startup scan to finish", |v| {
        if v["method"] == "textDocument/publishDiagnostics" {
            if let Some(uri) = v["params"]["uri"].as_str() {
                if uri.ends_with(clean_rel) && publish_has_code(v, "CW281") {
                    clean_published = true;
                }
                if uri.ends_with(ignored_rel) && publish_has_code(v, "CW281") {
                    ignored_bad.push(v.clone());
                }
            }
            return false;
        }
        v["method"] == "loadingBar" && v["params"]["enable"] == Value::Bool(false)
    });
    server.stop();

    assert!(
        clean_published,
        "the scan must publish CW281 for the plain file, or the test proves nothing"
    );
    assert!(
        ignored_bad.is_empty(),
        "directive must suppress CW281 in the scan publish: {ignored_bad:?}"
    );
}

#[test]
fn test_ignore_code_action_edits_the_workspace_settings() {
    // Round-trip: a CW281 diagnostic's codeAction response carries an
    // "Ignore CW281 in this workspace" action whose edit adds the code to
    // `cwtools.errors.ignore` in the workspace's .vscode/settings.json.
    let rel_path = "common/decisions/test.txt";
    let text = "x = { limit = { } }\n";
    let mut server = decision_server(&[(rel_path, text)]);

    server.open(rel_path, text);
    let diag = server.wait_for_diag_object(rel_path, "CW281");

    let resp = server.code_actions(rel_path, &diag, false);
    server.stop();

    assert_eq!(resp["id"], 2, "got: {resp}");
    let all = resp["result"].as_array().expect("array result");
    let action = all
        .iter()
        .find(|a| a["title"] == "Ignore CW281 in this workspace")
        .unwrap_or_else(|| panic!("no ignore action in the response: {resp}"));
    assert_eq!(action["kind"], "quickfix");
    assert_eq!(action["diagnostics"].as_array().unwrap().len(), 1);

    let changes = action["edit"]["changes"]
        .as_object()
        .expect("edit carries a changes map");
    assert_eq!(changes.len(), 1, "only the settings file is edited");
    let (uri, edits) = changes.iter().next().expect("one file edited");
    assert!(
        uri.ends_with(".vscode/settings.json"),
        "the edit must target the workspace settings file: {uri}"
    );
    let new_text = edits[0]["newText"].as_str().expect("whole-file replace");
    let parsed: Value = serde_json::from_str(new_text).expect("settings stay valid json");
    assert_eq!(parsed["cwtools"]["errors"]["ignore"][0], "CW281");
}

#[test]
fn test_cw240_quickfix_replaces_only_the_enum_value() {
    const RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}

enums = {
    enum[mode] = { historic fantasy sandbox }
}

decision = {
    label = scalar
    mode = enum[mode]
}
"#;
    let rel_path = "common/decisions/test.txt";
    let text = "decision = { label = \"😀\" mode = histroic } # typo\n";
    let mut server = TestServer::builder()
        .rules_file("r.cwt", RULES)
        .vanilla()
        .file(rel_path, text)
        .spawn_scanned();
    let doc_uri = server.uri(rel_path);

    server.open(rel_path, text);
    let diag = server.wait_for_diag_object(rel_path, "CW240");
    let response = server.code_actions(rel_path, &diag, true);
    server.stop();

    let actions = response["result"].as_array().expect("array result");
    assert_eq!(
        actions.len(),
        2,
        "the fix plus the ignore action: {response}"
    );
    assert_eq!(actions[0]["title"], "Did you mean 'historic'?");
    let edits = actions[0]["edit"]["changes"]
        .get(&doc_uri)
        .and_then(Value::as_array)
        .expect("edits for the document");
    assert_eq!(edits.len(), 1, "one text edit expected: {response}");
    let value_start = text[..text.find("histroic").unwrap()]
        .encode_utf16()
        .count() as u64;
    assert_eq!(edits[0]["range"]["start"]["line"], 0);
    assert_eq!(edits[0]["range"]["start"]["character"], value_start);
    assert_eq!(edits[0]["range"]["end"]["line"], 0);
    assert_eq!(edits[0]["range"]["end"]["character"], value_start + 8);
    assert_eq!(edits[0]["newText"], "\"historic\"");

    let units: Vec<u16> = text[..text.find('\n').unwrap()].encode_utf16().collect();
    let start = edits[0]["range"]["start"]["character"].as_u64().unwrap() as usize;
    let end = edits[0]["range"]["end"]["character"].as_u64().unwrap() as usize;
    let mut fixed = String::from_utf16(&units[..start]).expect("start is on a char boundary");
    fixed.push_str(edits[0]["newText"].as_str().unwrap());
    fixed.push_str(&String::from_utf16(&units[end..]).expect("end is on a char boundary"));
    fixed.push('\n');
    assert_eq!(
        fixed, "decision = { label = \"😀\" mode = \"historic\" } # typo\n",
        "the UTF-16 edit must replace only the enum value"
    );
}

#[test]
fn test_cw122_quickfix_removes_only_the_quotes() {
    const RULES: &str = r#"
types = {
    type[thing] = { path = "game/common/things" }
}

thing = {
    label = scalar
    iname = localisation_inline
}
"#;
    let rel_path = "common/things/test.txt";
    let text = "thing = { label = \"😀\" iname = \"my_key\" } # unnecessary\n";
    let mut server = TestServer::builder()
        .rules_file("r.cwt", RULES)
        .vanilla()
        .file(
            "localisation/test_l_english.yml",
            b"\xEF\xBB\xBFl_english:\n my_key: \"My key\"\n",
        )
        .file(rel_path, text)
        .spawn_scanned();
    let doc_uri = server.uri(rel_path);

    server.open(rel_path, text);
    let diag = server.wait_for_diag_object(rel_path, "CW122");
    let response = server.code_actions(rel_path, &diag, true);
    server.stop();

    let actions = response["result"].as_array().expect("array result");
    assert_eq!(
        actions.len(),
        2,
        "the fix plus the ignore action: {response}"
    );
    assert_eq!(actions[0]["title"], "Remove unnecessary quotes");
    let edits = actions[0]["edit"]["changes"]
        .get(&doc_uri)
        .and_then(Value::as_array)
        .expect("edits for the document");
    assert_eq!(edits.len(), 1, "one text edit expected: {response}");
    let value_start = text[..text.find("\"my_key\"").unwrap()]
        .encode_utf16()
        .count() as u64;
    assert_eq!(edits[0]["range"]["start"]["line"], 0);
    assert_eq!(edits[0]["range"]["start"]["character"], value_start);
    assert_eq!(edits[0]["range"]["end"]["line"], 0);
    assert_eq!(edits[0]["range"]["end"]["character"], value_start + 8);
    assert_eq!(edits[0]["newText"], "my_key");

    let units: Vec<u16> = text[..text.find('\n').unwrap()].encode_utf16().collect();
    let start = edits[0]["range"]["start"]["character"].as_u64().unwrap() as usize;
    let end = edits[0]["range"]["end"]["character"].as_u64().unwrap() as usize;
    let mut fixed = String::from_utf16(&units[..start]).expect("start is on a char boundary");
    fixed.push_str(edits[0]["newText"].as_str().unwrap());
    fixed.push_str(&String::from_utf16(&units[end..]).expect("end is on a char boundary"));
    fixed.push('\n');
    assert_eq!(
        fixed, "thing = { label = \"😀\" iname = my_key } # unnecessary\n",
        "the UTF-16 edit must remove only the quotes"
    );
}

#[test]
fn test_code_action_and_diagnostic_agree_on_non_bmp_line() {
    // The published diagnostic and the quick fix hanging off it must speak the
    // same position encoding. A non-BMP char earlier on the line makes the
    // parser's char column and the client's UTF-16 column differ, so publishing
    // the raw column applied the fix one column short — corrupting the file.
    let rel_path = "common/decisions/test.txt";
    let text = "x = { name = \"😀\" limit = { } }\n";
    // No `general.positionEncodings` in the capabilities → UTF-16, as VS Code.
    let mut server = decision_server(&[(rel_path, text)]);

    server.open(rel_path, text);
    let diag = server.wait_for_diag_object(rel_path, "CW281");
    // The squiggle starts at `limit` in UTF-16 units (18), not chars (17).
    let limit_utf16 = text[..text.find("limit").unwrap()].encode_utf16().count() as u64;
    assert_eq!(
        diag["range"]["start"]["character"], limit_utf16,
        "diagnostic must use the negotiated encoding: {diag}"
    );

    let resp = server.code_actions(rel_path, &diag, false);
    server.stop();

    let e = resp["result"][0]["edit"]["changes"]
        .as_object()
        .and_then(|c| c.values().next())
        .and_then(|v| v.get(0))
        .cloned()
        .expect("one text edit");
    assert_eq!(
        e["range"]["start"], diag["range"]["start"],
        "the fix must start where the squiggle does: {resp}"
    );

    // Apply the edit the way a UTF-16 client would: splice by code units.
    let sc = e["range"]["start"]["character"].as_u64().unwrap() as usize;
    let ec = e["range"]["end"]["character"].as_u64().unwrap() as usize;
    let nl = text.find('\n').unwrap();
    let units: Vec<u16> = text[..nl].encode_utf16().collect();
    let mut fixed = String::from_utf16(&units[..sc]).expect("start is on a char boundary");
    fixed.push_str(e["newText"].as_str().unwrap());
    fixed.push_str(&String::from_utf16(&units[ec..]).expect("end is on a char boundary"));
    fixed.push_str(&text[nl..]);
    assert_eq!(
        fixed, "x = { name = \"😀\" }\n",
        "applied edit must delete exactly the empty limit"
    );
}
