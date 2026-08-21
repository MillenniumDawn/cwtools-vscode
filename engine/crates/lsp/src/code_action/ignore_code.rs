//! "Ignore CWxxx in this workspace" code action: add a diagnostic's code to
//! the workspace setting `cwtools.errors.ignore` (the VS Code client maps that
//! to the server's `ignoredErrorCodes`). The edit lands in the workspace's
//! `.vscode/settings.json`, so the client's settings watcher re-sends
//! `workspace/didChangeConfiguration` and the live-update path applies it —
//! the same round trip as editing the setting by hand. The edit is built by a
//! pure function over the settings file's current text, so the handler and the
//! unit tests exercise the same mapping.

use std::collections::HashSet;
use std::path::Path;

use tower_lsp::lsp_types::*;

/// The settings section the VS Code client reads (`cwtools.errors.ignore`),
/// spelled as the JSON path from the settings root.
const SETTINGS_KEY: [&str; 3] = ["cwtools", "errors", "ignore"];

/// New text for the workspace's settings JSON after adding `code` to the
/// ignore list. `existing` is the file's current text (`None` when the file
/// does not exist yet, or is empty). `None` when the existing text is not
/// JSON, not an object, or already holds the key as something other than an
/// array — rewriting any of those would destroy the user's settings, so the
/// action declines instead.
pub(super) fn updated_settings_json(code: &str, existing: Option<&str>) -> Option<String> {
    let mut root: serde_json::Value = match existing {
        None => serde_json::json!({}),
        Some(text) if text.trim().is_empty() => serde_json::json!({}),
        Some(text) => serde_json::from_str(text).ok()?,
    };
    let mut cursor = root.as_object_mut()?;
    for (i, key) in SETTINGS_KEY.iter().enumerate() {
        let is_last = i == SETTINGS_KEY.len() - 1;
        if is_last {
            let ignore = cursor.entry(*key).or_insert_with(|| serde_json::json!([]));
            let list = ignore.as_array_mut()?;
            if !list
                .iter()
                .any(|v| v.as_str().is_some_and(|s| s.eq_ignore_ascii_case(code)))
            {
                list.push(serde_json::Value::String(code.to_string()));
            }
        } else {
            cursor = cursor
                .entry(*key)
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()?;
        }
    }
    let mut out = serde_json::to_string_pretty(&root).ok()?;
    out.push('\n');
    Some(out)
}

/// One action per distinct string code in `diagnostics`, each editing the
/// workspace settings file under `workspace_root` to ignore that code. Codes
/// already in `ignored` are skipped (a live diagnostic can't be suppressed
/// already, but a stale client-side one could arrive). Actions whose edit
/// would corrupt the settings file are skipped via [`updated_settings_json`].
pub(super) fn ignore_code_actions(
    diagnostics: &[Diagnostic],
    ignored: &[String],
    workspace_root: Option<&Path>,
    settings_content: Option<&str>,
) -> Vec<CodeActionOrCommand> {
    let Some(root) = workspace_root else {
        return Vec::new();
    };
    let Ok(settings_uri) = Url::from_file_path(root.join(".vscode").join("settings.json")) else {
        return Vec::new();
    };
    let mut seen: HashSet<&str> = HashSet::new();
    let mut actions = Vec::new();
    for diag in diagnostics {
        let Some(NumberOrString::String(code)) = diag.code.as_ref() else {
            continue;
        };
        if !seen.insert(code.as_str()) {
            continue;
        }
        if ignored.iter().any(|i| i.eq_ignore_ascii_case(code)) {
            continue;
        }
        let Some(updated) = updated_settings_json(code, settings_content) else {
            continue;
        };
        let related: Vec<Diagnostic> = diagnostics
            .iter()
            .filter(|d| d.code.as_ref() == Some(&NumberOrString::String(code.clone())))
            .cloned()
            .collect();
        // Whole-document replace, the LSP idiom for an edit that rewrites a
        // file end to end.
        let whole_file = TextEdit {
            range: Range::new(Position::new(0, 0), Position::new(u32::MAX, u32::MAX)),
            new_text: updated,
        };
        let mut changes = std::collections::HashMap::new();
        changes.insert(settings_uri.clone(), vec![whole_file]);
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: cwtools_i18n::format(cwtools_i18n::Key::ActionIgnoreCode, &[code]),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(related),
            edit: Some(WorkspaceEdit {
                changes: Some(changes),
                document_changes: None,
                change_annotations: None,
            }),
            ..Default::default()
        }));
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostic(code: &str) -> Diagnostic {
        Diagnostic {
            code: Some(NumberOrString::String(code.to_string())),
            ..Default::default()
        }
    }

    // `/ws` is not absolute on Windows; Url::from_file_path rejects it.
    fn abs(tail: &str) -> std::path::PathBuf {
        if cfg!(windows) {
            std::path::PathBuf::from(format!("C:/{tail}"))
        } else {
            std::path::PathBuf::from(format!("/{tail}"))
        }
    }

    #[test]
    fn new_settings_file_gets_the_section() {
        let updated = updated_settings_json("CW100", None).expect("new file builds");
        let parsed: serde_json::Value = serde_json::from_str(&updated).expect("valid json");
        assert_eq!(parsed["cwtools"]["errors"]["ignore"][0], "CW100");
    }

    #[test]
    fn empty_settings_file_gets_the_section() {
        let updated = updated_settings_json("CW100", Some("")).expect("empty file builds");
        let parsed: serde_json::Value = serde_json::from_str(&updated).expect("valid json");
        assert_eq!(parsed["cwtools"]["errors"]["ignore"][0], "CW100");
    }

    #[test]
    fn existing_list_is_appended_and_deduped_case_insensitively() {
        let existing = r#"{
  "cwtools": { "errors": { "ignore": ["cw100", "CW246"] } }
}"#;
        let updated = updated_settings_json("CW107", Some(existing)).expect("appends");
        let parsed: serde_json::Value = serde_json::from_str(&updated).expect("valid json");
        assert_eq!(
            parsed["cwtools"]["errors"]["ignore"].as_array().unwrap(),
            &["cw100", "CW246", "CW107"]
        );
        let dup = updated_settings_json("CW246", Some(&updated)).expect("dedupes");
        let parsed: serde_json::Value = serde_json::from_str(&dup).expect("valid json");
        assert_eq!(
            parsed["cwtools"]["errors"]["ignore"]
                .as_array()
                .unwrap()
                .len(),
            3,
            "a code already present must not be added twice"
        );
    }

    #[test]
    fn unrelated_settings_survive() {
        let existing = r#"{
  "editor.formatOnSave": true
}"#;
        let updated = updated_settings_json("CW100", Some(existing)).expect("builds");
        let parsed: serde_json::Value = serde_json::from_str(&updated).expect("valid json");
        // The existing file used a flat dotted key; the action must leave it
        // exactly as written, not restructure it into a nested object.
        assert_eq!(parsed["editor.formatOnSave"], true);
        assert_eq!(parsed["cwtools"]["errors"]["ignore"][0], "CW100");
    }

    #[test]
    fn malformed_or_non_object_settings_decline() {
        assert!(updated_settings_json("CW100", Some("not json")).is_none());
        assert!(updated_settings_json("CW100", Some("[1, 2]")).is_none());
        assert!(updated_settings_json("CW100", Some("null")).is_none());
    }

    #[test]
    fn non_array_ignore_key_declines() {
        let existing = r#"{ "cwtools": { "errors": { "ignore": "CW100" } } }"#;
        assert!(updated_settings_json("CW246", Some(existing)).is_none());
    }

    #[test]
    fn actions_are_built_per_unique_code() {
        let root = abs("ws");
        let diags = vec![
            diagnostic("CW100"),
            diagnostic("CW100"),
            diagnostic("CW246"),
        ];
        let actions = ignore_code_actions(&diags, &[], Some(&root), None);
        assert_eq!(actions.len(), 2, "one action per distinct code");
        let CodeActionOrCommand::CodeAction(first) = &actions[0] else {
            panic!("expected a CodeAction");
        };
        assert_eq!(first.title, "Ignore CW100 in this workspace");
        assert_eq!(first.kind, Some(CodeActionKind::QUICKFIX));
        assert_eq!(first.diagnostics.as_ref().unwrap().len(), 2);
        let changes = first
            .edit
            .as_ref()
            .and_then(|e| e.changes.as_ref())
            .expect("edit carries changes");
        let (uri, edits) = changes.iter().next().expect("one file edited");
        assert!(uri.path().ends_with(".vscode/settings.json"));
        assert_eq!(edits.len(), 1, "whole-file replace");
    }

    #[test]
    fn numeric_codes_and_existing_ignores_yield_no_action() {
        let numeric = Diagnostic {
            code: Some(NumberOrString::Number(100)),
            ..Default::default()
        };
        let root = abs("ws");
        let actions = ignore_code_actions(&[numeric], &[], Some(&root), None);
        assert!(actions.is_empty(), "numeric codes have no name to ignore");

        let already = ignore_code_actions(
            &[diagnostic("CW100")],
            &["cw100".to_string()],
            Some(&root),
            None,
        );
        assert!(already.is_empty(), "already-ignored codes are skipped");
    }

    #[test]
    fn no_workspace_root_yields_no_actions() {
        let actions = ignore_code_actions(&[diagnostic("CW100")], &[], None, None);
        assert!(actions.is_empty());
    }
}
