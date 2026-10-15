use serde_json::{Value, json};

use crate::common::{EDITOR_RULES, TestServer, path_uri};

const OLD_EVENT: &str = "old_event = { id = old_event }\n";
const MOVED_FOCUS: &str = "moved_focus = {\n    id = moved_focus\n}\n";
const TREE: &str = "common/national_focus/tree.txt";

/// A second workspace folder holding a focus file.
fn second_folder() -> tempfile::TempDir {
    let second = tempfile::tempdir().unwrap();
    let p = second.path().join(TREE);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, MOVED_FOCUS).unwrap();
    second
}

fn is_tree_publish(frame: &Value) -> bool {
    frame["method"] == "textDocument/publishDiagnostics"
        && frame["params"]["uri"]
            .as_str()
            .is_some_and(|u| u.ends_with(TREE))
}

#[test]
fn test_did_change_workspace_folders_repoints_and_rescans() {
    let second = second_folder();
    let mut server = TestServer::builder()
        .rules_file("editor_rules.cwt", EDITOR_RULES)
        .file("events/old.txt", OLD_EVENT)
        .spawn_uninitialized();
    let first_uri = path_uri(server.workspace());
    server.initialize_merging(json!({
        "workspaceFolders": [{ "uri": first_uri, "name": "first" }],
    }));
    server.initialized();
    server.open("events/old.txt", OLD_EVENT);

    // Swap the primary folder for one that holds a focus file.
    server.notify(
        "workspace/didChangeWorkspaceFolders",
        json!({
            "event": {
                "added": [{ "uri": path_uri(second.path()), "name": "second" }],
                "removed": [{ "uri": first_uri, "name": "first" }],
            }
        }),
    );

    // The re-scan must index the new root: its file gets diagnostics published.
    let publish = server.read_until("the rescan's diagnostics for the new root", is_tree_publish);
    println!(
        "rescan published diagnostics for {}",
        publish["params"]["uri"]
    );
    let old_uri = server.uri("events/old.txt");
    let symbols = server.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": old_uri } }),
    );
    assert!(
        symbols.is_null(),
        "the new workspace was not scanned or the removed workspace document stayed open"
    );
}

#[test]
fn test_did_change_workspace_folders_keeps_a_remaining_root_primary() {
    // #661: a removal-only event that takes out the primary promotes the
    // first survivor, which is rescanned and stays usable.
    let second = second_folder();
    let mut server = TestServer::builder()
        .rules_file("editor_rules.cwt", EDITOR_RULES)
        .file("events/old.txt", OLD_EVENT)
        .no_root()
        .spawn_uninitialized();
    let first_uri = path_uri(server.workspace());
    server.initialize_merging(json!({
        "workspaceFolders": [
            { "uri": first_uri, "name": "first" },
            { "uri": path_uri(second.path()), "name": "second" },
        ],
    }));
    server.initialized();
    // Wait for the startup scan (covers only `first`) so a publish for
    // `second`'s file can only come from the survivor rescan.
    server.wait_for_scan_done();

    // Remove only the primary folder; nothing is added.
    server.notify(
        "workspace/didChangeWorkspaceFolders",
        json!({
            "event": {
                "added": [],
                "removed": [{ "uri": first_uri, "name": "first" }],
            }
        }),
    );

    // The startup scan never touched `second`, so this publish is the rescan.
    server.read_until(
        "the survivor rescan's diagnostics for its file",
        is_tree_publish,
    );
    let symbols = server.request("workspace/symbol", json!({ "query": "moved_focus" }));
    // Indexed only if the survivor was scanned as the primary.
    let indexed = symbols.as_array().is_some_and(|syms| {
        syms.iter().any(|s| {
            s["name"] == "moved_focus"
                && s["location"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with(TREE))
        })
    });
    assert!(
        indexed,
        "workspace/symbol found no symbol from the surviving root; it was never scanned as primary"
    );
    server.send_request(
        100,
        "workspace/executeCommand",
        json!({ "command": "formatWorkspace", "arguments": [] }),
    );
    let (response, _) = server.read_response_answering_apply_edit(100);
    let format_msg = response["result"]
        .as_str()
        .unwrap_or_else(|| panic!("formatWorkspace returned no message: {response}"));
    assert!(
        format_msg.starts_with("Formatted ")
            || format_msg.starts_with("No files needed formatting"),
        "formatWorkspace must return a successful result for the surviving primary"
    );
}

#[test]
fn test_did_change_workspace_folders_with_no_root_left_reports_no_workspace() {
    // #661: removing the last folder clears the primary on purpose, so the
    // opened document leaves the workspace and commands report no-folder.
    let text = "my_focus = {\n    mood = sleepy\n}\n";
    let mut server = TestServer::builder()
        .rules_file("editor_rules.cwt", EDITOR_RULES)
        .file(TREE, text)
        .no_root()
        .spawn_uninitialized();
    let first_uri = path_uri(server.workspace());
    server.initialize_merging(json!({
        "workspaceFolders": [{ "uri": first_uri, "name": "first" }],
    }));
    server.initialized();
    server.open_and_wait(TREE, text);

    // Remove the only folder; nothing is added.
    server.notify(
        "workspace/didChangeWorkspaceFolders",
        json!({
            "event": {
                "added": [],
                "removed": [{ "uri": first_uri, "name": "first" }],
            }
        }),
    );

    // The document left the workspace, so its diagnostics clear.
    server.read_until(
        "the cleared diagnostics for the removed folder's document",
        |v| {
            is_tree_publish(v)
                && v["params"]["diagnostics"]
                    .as_array()
                    .is_some_and(Vec::is_empty)
        },
    );
    let result = server.request(
        "workspace/executeCommand",
        json!({ "command": "formatWorkspace", "arguments": [] }),
    );
    assert_eq!(
        result.as_str(),
        Some("No workspace folder."),
        "the removal must clear the primary so workspace commands report the no-folder state"
    );
}
