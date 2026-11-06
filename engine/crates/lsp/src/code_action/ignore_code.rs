use std::collections::HashSet;
use std::path::Path;

use tower_lsp::lsp_types::*;

pub(super) fn ignore_code_actions(
    diagnostics: &[Diagnostic],
    ignored: &[String],
    workspace_root: Option<&Path>,
) -> Vec<CodeActionOrCommand> {
    let Some(uri) = workspace_root.and_then(|root| Url::from_file_path(root).ok()) else {
        return Vec::new();
    };
    let mut seen: HashSet<&str> = HashSet::new();
    let mut actions = Vec::new();
    for diag in diagnostics {
        let Some(NumberOrString::String(code)) = diag.code.as_ref() else {
            continue;
        };
        let bytes = code.as_bytes();
        if bytes.len() != 5
            || !bytes[..2].eq_ignore_ascii_case(b"CW")
            || !bytes[2..].iter().all(u8::is_ascii_digit)
        {
            continue;
        }
        if !seen.insert(code.as_str()) {
            continue;
        }
        if ignored.iter().any(|i| i.eq_ignore_ascii_case(code)) {
            continue;
        }
        let related: Vec<Diagnostic> = diagnostics
            .iter()
            .filter(|d| d.code.as_ref() == Some(&NumberOrString::String(code.clone())))
            .cloned()
            .collect();
        let title = cwtools_i18n::format(cwtools_i18n::Key::ActionIgnoreCode, &[code]);
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: title.clone(),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(related),
            command: Some(Command {
                title,
                command: "cwtools.ignoreDiagnosticCode".to_string(),
                arguments: Some(vec![serde_json::json!(uri), serde_json::json!(code)]),
            }),
            ..Default::default()
        }));
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_test_support::abs_path;

    fn diagnostic(code: &str) -> Diagnostic {
        Diagnostic {
            code: Some(NumberOrString::String(code.to_string())),
            ..Default::default()
        }
    }

    #[test]
    fn actions_are_built_per_unique_code() {
        let root = abs_path("ws");
        let uri = Url::from_file_path(&root).unwrap();
        let diags = vec![
            diagnostic("CW100"),
            diagnostic("CW100"),
            diagnostic("CW246"),
        ];
        let actions = ignore_code_actions(&diags, &[], Some(&root));
        assert_eq!(actions.len(), 2, "one action per distinct code");
        let CodeActionOrCommand::CodeAction(first) = &actions[0] else {
            panic!("expected a CodeAction");
        };
        assert_eq!(first.title, "Ignore CW100 in this workspace");
        assert_eq!(first.kind, Some(CodeActionKind::QUICKFIX));
        assert_eq!(first.diagnostics.as_ref().unwrap().len(), 2);
        assert!(first.edit.is_none(), "the client owns settings edits");
        let command = first.command.as_ref().expect("client command");
        assert_eq!(command.command, "cwtools.ignoreDiagnosticCode");
        assert_eq!(command.title, first.title);
        assert_eq!(
            command.arguments,
            Some(vec![serde_json::json!(uri), serde_json::json!("CW100")])
        );
    }

    #[test]
    fn numeric_codes_and_existing_ignores_yield_no_action() {
        let numeric = Diagnostic {
            code: Some(NumberOrString::Number(100)),
            ..Default::default()
        };
        let root = abs_path("ws");
        let actions = ignore_code_actions(&[numeric], &[], Some(&root));
        assert!(actions.is_empty(), "numeric codes have no name to ignore");

        let already =
            ignore_code_actions(&[diagnostic("CW100")], &["cw100".to_string()], Some(&root));
        assert!(already.is_empty(), "already-ignored codes are skipped");
    }

    #[test]
    fn foreign_codes_yield_no_actions() {
        let root = abs_path("ws");
        let diags = [
            diagnostic("yaml-schema"),
            diagnostic(""),
            diagnostic("CW1234"),
            diagnostic("cw100"),
        ];
        let actions = ignore_code_actions(&diags, &[], Some(&root));
        assert_eq!(actions.len(), 1);
        let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
            panic!("expected action")
        };
        assert_eq!(action.title, "Ignore cw100 in this workspace");
    }

    #[test]
    fn no_workspace_root_yields_no_actions() {
        let actions = ignore_code_actions(&[diagnostic("CW100")], &[], None);
        assert!(actions.is_empty());
    }
}
