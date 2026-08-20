use std::collections::HashMap;

use tower_lsp::lsp_types::*;

use cwtools_parser::ast::{SourcePos, SourceRange};
use cwtools_parser::fix::{SpanEdit, SuggestedFix, plan_file_edits};

use crate::paths::source_position_to_lsp;

/// Key under which a fix payload lives in `Diagnostic.data`. Namespaced so a
/// codeAction request only treats data it put there as a fix (future diagnostic
/// data of other shapes is ignored).
pub(super) const FIX_DATA_KEY: &str = "cwtoolsFix";

/// A single span replacement round-tripped through `Diagnostic.data`. Ranges use
/// the parser convention: 1-based line, 0-based char column.
pub(super) struct FixEdit {
    pub(super) range: SourceRange,
    pub(super) replacement: String,
}

/// A named set of edits resolving one diagnostic. `edits` is empty for a
/// "create missing localisation key" fix (CW100): it has no in-file span to
/// replace, and carries `create_loc_key` for the dedicated code action in
/// this module instead. Every other consumer here (`code_actions_from_diagnostics`,
/// `fix_all_action`) treats an empty `edits` as nothing to do.
pub(super) struct FixPayload {
    pub(super) title: String,
    pub(super) edits: Vec<FixEdit>,
    pub(super) create_loc_key: Option<String>,
}

/// Serialize a [`SuggestedFix`] into a `Diagnostic.data` value, namespaced under
/// [`FIX_DATA_KEY`]. Ranges are stored in the parser convention (1-based line,
/// 0-based char col); the LSP conversion happens in the handler where the
/// document text and the negotiated encoding are available.
pub(crate) fn fix_to_data(fix: &SuggestedFix) -> serde_json::Value {
    let edits: Vec<serde_json::Value> = fix
        .edits
        .iter()
        .map(|e| {
            serde_json::json!({
                "startLine": e.range.start.line,
                "startCol": e.range.start.col,
                "endLine": e.range.end.line,
                "endCol": e.range.end.col,
                "replacement": e.replacement,
            })
        })
        .collect();
    let mut payload = serde_json::json!({
        "title": fix.title,
        "edits": edits,
    });
    if let Some(key) = &fix.create_loc_key {
        payload["createLocKey"] = serde_json::Value::String(key.clone());
    }
    serde_json::json!({ FIX_DATA_KEY: payload })
}

/// Parse a fix payload out of a diagnostic's `data` value. `None` when the value
/// isn't a cwtools fix payload (any other diagnostic-data shape, or a
/// malformed/partial entry).
pub(super) fn fix_from_data(data: &serde_json::Value) -> Option<FixPayload> {
    let obj = data.get(FIX_DATA_KEY)?;
    let title = obj.get("title")?.as_str()?.to_string();
    let edits_json = obj.get("edits")?.as_array()?;
    let mut edits = Vec::with_capacity(edits_json.len());
    for e in edits_json {
        edits.push(FixEdit {
            range: SourceRange {
                start: SourcePos {
                    line: e.get("startLine")?.as_u64()? as u32,
                    col: e.get("startCol")?.as_u64()? as u16,
                },
                end: SourcePos {
                    line: e.get("endLine")?.as_u64()? as u32,
                    col: e.get("endCol")?.as_u64()? as u16,
                },
            },
            replacement: e.get("replacement")?.as_str()?.to_string(),
        });
    }
    let create_loc_key = obj
        .get("createLocKey")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Some(FixPayload {
        title,
        edits,
        create_loc_key,
    })
}

/// The span edits `diag`'s fix payload carries, tagged with the diagnostic's
/// code — empty when the diagnostic has no payload, its code isn't a string,
/// or (like CW100's create-key fix) it carries no span edits. Shared by the
/// `fixAllWorkspace` store (`validate::publish_filtered`) and available for
/// any future consumer that needs "what would a fix-all apply here" without
/// going through `fix_all_action`'s per-request diagnostic bookkeeping.
pub(crate) fn fixable_span_edits(diag: &Diagnostic) -> Vec<(String, SpanEdit)> {
    let Some(payload) = diag.data.as_ref().and_then(fix_from_data) else {
        return Vec::new();
    };
    let Some(NumberOrString::String(code)) = diag.code.clone() else {
        return Vec::new();
    };
    payload
        .edits
        .into_iter()
        .map(|e| {
            (
                code.clone(),
                SpanEdit {
                    range: e.range,
                    replacement: e.replacement,
                },
            )
        })
        .collect()
}

/// Convert a parser [`SourceRange`] (1-based line, 0-based char col) into an LSP
/// `Range`, using `text` and the negotiated `encoding` — the same
/// `source_position_to_lsp` conversion hover/rename/navigation use, and the one
/// the diagnostic this fix hangs off went through (`validate::DocLines`), so the
/// edit lands on exactly the columns the squiggle covers.
pub(crate) fn source_range_to_lsp(
    range: SourceRange,
    text: &str,
    encoding: &PositionEncodingKind,
) -> Range {
    Range {
        start: source_position_to_lsp(
            text,
            range.start.line.saturating_sub(1),
            range.start.col as u32,
            encoding,
        ),
        end: source_position_to_lsp(
            text,
            range.end.line.saturating_sub(1),
            range.end.col as u32,
            encoding,
        ),
    }
}

/// Build QUICKFIX code actions from the request's context diagnostics: one per
/// diagnostic carrying a cwtools fix payload. Pure (no locks / IO) so the
/// handler and its test exercise the same mapping. `text`/`encoding` drive the
/// range conversion.
pub(super) fn code_actions_from_diagnostics(
    uri: &Url,
    diagnostics: &[Diagnostic],
    text: &str,
    encoding: &PositionEncodingKind,
) -> Vec<CodeActionOrCommand> {
    let mut actions = Vec::new();
    for diag in diagnostics {
        let Some(payload) = diag.data.as_ref().and_then(fix_from_data) else {
            continue;
        };
        // A create-loc-key fix (CW100) has no span edit to offer here — it
        // gets its own cross-file action from `create_loc_key_actions`.
        if payload.edits.is_empty() {
            continue;
        }
        let text_edits: Vec<TextEdit> = payload
            .edits
            .iter()
            .map(|e| TextEdit {
                range: source_range_to_lsp(e.range, text, encoding),
                new_text: e.replacement.clone(),
            })
            .collect();
        let mut changes = HashMap::new();
        changes.insert(uri.clone(), text_edits);
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: payload.title,
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diag.clone()]),
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

/// Build the single `source.fixAll` action: every fixable diagnostic in the
/// document, applied at once. `None` when fewer than one edit survives.
///
/// Overlap resolution is [`cwtools_parser::fix::plan_file_edits`] — the same
/// function the CLI `fix` subcommand uses, so "fix all" in the editor and
/// `cwtools fix --apply` produce the same file. An edit dropped for overlapping
/// keeps its own quick fix, which is the right outcome: applying it needs the
/// first edit to land and the document to be re-validated.
///
/// The diagnostics come from `context.diagnostics`, which the client scopes to
/// the requested range. `editor.codeActionsOnSave` requests the whole document,
/// so that is what "all" means here.
pub(super) fn fix_all_action(
    uri: &Url,
    diagnostics: &[Diagnostic],
    text: &str,
    encoding: &PositionEncodingKind,
) -> Option<CodeActionOrCommand> {
    // A create-loc-key payload (CW100) parses fine but carries no span edit —
    // it must not count as "fixable" here (nothing for `plan_file_edits` to
    // apply) or be claimed by the action below (its own dedicated action
    // resolves it instead).
    let payloads_with_edits: Vec<(usize, FixPayload)> = diagnostics
        .iter()
        .enumerate()
        .filter_map(|(i, d)| Some((i, d.data.as_ref().and_then(fix_from_data)?)))
        .filter(|(_, payload)| !payload.edits.is_empty())
        .collect();
    let fixable: std::collections::HashSet<usize> =
        payloads_with_edits.iter().map(|(i, _)| *i).collect();
    let planned: Vec<(usize, SpanEdit)> = payloads_with_edits
        .into_iter()
        .flat_map(|(i, payload)| {
            payload.edits.into_iter().map(move |e| {
                (
                    i,
                    SpanEdit {
                        range: e.range,
                        replacement: e.replacement,
                    },
                )
            })
        })
        .collect();
    if planned.is_empty() {
        return None;
    }
    let (kept, skipped) = plan_file_edits(text, planned);
    if kept.is_empty() {
        return None;
    }
    // Attribute the action to the diagnostics it actually resolves, so the
    // client can clear them optimistically.
    let resolved: Vec<Diagnostic> = diagnostics
        .iter()
        .enumerate()
        .filter(|(i, _)| fixable.contains(i) && !skipped.contains(i))
        .map(|(_, d)| d.clone())
        .collect();
    let text_edits: Vec<TextEdit> = kept
        .iter()
        .map(|e| TextEdit {
            range: source_range_to_lsp(e.range, text, encoding),
            new_text: e.replacement.clone(),
        })
        .collect();
    let mut changes = HashMap::new();
    changes.insert(uri.clone(), text_edits);
    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: cwtools_i18n::format(cwtools_i18n::Key::ActionFixAll, &[&kept.len().to_string()]),
        kind: Some(CodeActionKind::SOURCE_FIX_ALL),
        diagnostics: Some(resolved),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }),
        ..Default::default()
    }))
}

/// Whether the request asks for `kind`. An absent `only` means "everything";
/// LSP kinds are hierarchical, so `source` selects `source.fixAll`.
pub(super) fn wants(only: Option<&Vec<CodeActionKind>>, kind: &CodeActionKind) -> bool {
    match only {
        None => true,
        Some(only) => only.iter().any(|k| {
            kind.as_str() == k.as_str() || kind.as_str().starts_with(&format!("{}.", k.as_str()))
        }),
    }
}

#[cfg(test)]
pub(super) fn create_loc_key_diag() -> Diagnostic {
    let fix =
        SuggestedFix::create_loc_key("Create localisation key my_thing_desc", "my_thing_desc");
    Diagnostic {
        range: Range::default(),
        severity: Some(DiagnosticSeverity::WARNING),
        code: Some(NumberOrString::String("CW100".into())),
        source: Some("cwtools".into()),
        message: "my_thing is missing localisation: my_thing_desc".into(),
        data: Some(fix_to_data(&fix)),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_parser::fix::{SpanEdit, apply_edits};

    fn range(sl: u32, sc: u16, el: u32, ec: u16) -> SourceRange {
        SourceRange {
            start: SourcePos { line: sl, col: sc },
            end: SourcePos { line: el, col: ec },
        }
    }

    #[test]
    fn payload_round_trips_through_data() {
        // A fix serialized into `Diagnostic.data` and read back must reproduce
        // the title, ranges, and replacement exactly (the client hands `data`
        // back verbatim on the codeAction request).
        let fix = SuggestedFix::replace("Wrap the value in quotes", range(5, 3, 5, 8), "\"hi\"");
        let data = fix_to_data(&fix);
        let parsed = fix_from_data(&data).expect("payload parses");
        assert_eq!(parsed.title, "Wrap the value in quotes");
        assert_eq!(parsed.edits.len(), 1);
        assert_eq!(parsed.edits[0].range, range(5, 3, 5, 8));
        assert_eq!(parsed.edits[0].replacement, "\"hi\"");
        assert_eq!(parsed.create_loc_key, None);
    }

    #[test]
    fn create_loc_key_payload_round_trips_through_data() {
        // CW100's fix has no span edit, only the key; both the empty `edits`
        // and the `createLocKey` field must survive the `Diagnostic.data`
        // round trip so `create_loc_key_actions` can build its action.
        let fix =
            SuggestedFix::create_loc_key("Create localisation key my_thing_desc", "my_thing_desc");
        let data = fix_to_data(&fix);
        let parsed = fix_from_data(&data).expect("payload parses");
        assert_eq!(parsed.title, "Create localisation key my_thing_desc");
        assert!(parsed.edits.is_empty());
        assert_eq!(parsed.create_loc_key.as_deref(), Some("my_thing_desc"));
    }

    #[test]
    fn non_fix_data_is_ignored() {
        // Diagnostic data that isn't a cwtools fix payload must not parse as one.
        assert!(fix_from_data(&serde_json::json!({ "other": 1 })).is_none());
        assert!(fix_from_data(&serde_json::json!(null)).is_none());
        // Missing the replacement field → not a usable edit → whole payload None.
        let bad = serde_json::json!({
            FIX_DATA_KEY: { "title": "x", "edits": [{ "startLine": 1, "startCol": 0, "endLine": 1, "endCol": 1 }] }
        });
        assert!(fix_from_data(&bad).is_none());
    }

    #[test]
    fn diagnostic_with_fix_maps_to_quickfix_action() {
        // The handler mapping: a diagnostic carrying a fix payload becomes one
        // QUICKFIX CodeAction whose edit, applied to the source, yields the fixed
        // text. Mirrors the CW253 `set_empire_name` -> `set_name` key rename.
        let text = "set_empire_name = { }\n";
        let fix = SuggestedFix::replace("Rename to set_name", range(1, 0, 1, 15), "set_name");
        let uri: Url = "file:///mod/common/x.txt".parse().unwrap();
        let diag = Diagnostic {
            range: Range::default(),
            severity: Some(DiagnosticSeverity::WARNING),
            code: Some(NumberOrString::String("CW253".into())),
            source: Some("cwtools".into()),
            message: "renamed effect".into(),
            data: Some(fix_to_data(&fix)),
            ..Default::default()
        };

        let actions = code_actions_from_diagnostics(
            &uri,
            std::slice::from_ref(&diag),
            text,
            &PositionEncodingKind::UTF16,
        );
        assert_eq!(actions.len(), 1, "one fix -> one action");
        let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
            panic!("expected a CodeAction, not a Command");
        };
        assert_eq!(action.title, "Rename to set_name");
        assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
        assert_eq!(action.diagnostics.as_ref().unwrap()[0].code, diag.code);

        // The edit's range + new_text must reproduce the CLI `fix` result.
        let edits = action
            .edit
            .as_ref()
            .and_then(|e| e.changes.as_ref())
            .and_then(|c| c.get(&uri))
            .expect("edit targets the document");
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0].range,
            Range::new(Position::new(0, 0), Position::new(0, 15))
        );
        assert_eq!(edits[0].new_text, "set_name");

        // Apply via the engine's own applier to confirm the span is right.
        let span = SpanEdit {
            range: range(1, 0, 1, 15),
            replacement: "set_name".into(),
        };
        assert_eq!(apply_edits(text, &[span]), "set_name = { }\n");
    }

    #[test]
    fn diagnostic_range_and_fix_edit_agree_on_non_bmp_line() {
        // A non-BMP char before the token makes the parser's char column (2) and
        // the client's UTF-16 column (3) differ. The published diagnostic and the
        // quick fix it carries must land on the same range, or applying the fix
        // rewrites a different span than the one highlighted.
        let text = "😀 set_empire_name = { }\n";
        let fix = SuggestedFix::replace("Rename to set_name", range(1, 2, 1, 17), "set_name");
        let err = cwtools_validation::ValidationError {
            message: "renamed effect".into(),
            severity: cwtools_validation::ErrorSeverity::Warning,
            line: 1,
            col: 2,
            file: "f".into(),
            code: Some("CW253"),
            fix: Some(fix),
            end: Some((1, 17)),
            related: Vec::new(),
        };
        let lines = crate::validate::DocLines::new(text, PositionEncodingKind::UTF16);
        let diag = crate::validate::validation_error_to_diagnostic(&err, &lines);
        assert_eq!(
            diag.range,
            Range::new(Position::new(0, 3), Position::new(0, 18)),
            "diagnostic range must use UTF-16 columns"
        );

        let uri: Url = "file:///mod/common/x.txt".parse().unwrap();
        let actions = code_actions_from_diagnostics(
            &uri,
            std::slice::from_ref(&diag),
            text,
            &PositionEncodingKind::UTF16,
        );
        let edits = match &actions[0] {
            CodeActionOrCommand::CodeAction(a) => a
                .edit
                .as_ref()
                .and_then(|e| e.changes.as_ref())
                .and_then(|c| c.get(&uri))
                .expect("edit targets the document"),
            _ => panic!("expected a CodeAction"),
        };
        assert_eq!(
            edits[0].range, diag.range,
            "quick fix must edit exactly the highlighted span"
        );
        // The parser-space span the payload carries is the right one.
        let span = SpanEdit {
            range: range(1, 2, 1, 17),
            replacement: "set_name".into(),
        };
        assert_eq!(apply_edits(text, &[span]), "😀 set_name = { }\n");
    }

    #[test]
    fn diagnostic_without_data_yields_no_action() {
        let uri: Url = "file:///mod/common/x.txt".parse().unwrap();
        let diag = Diagnostic {
            message: "plain".into(),
            ..Default::default()
        };
        let actions = code_actions_from_diagnostics(
            &uri,
            std::slice::from_ref(&diag),
            "x = y\n",
            &PositionEncodingKind::UTF16,
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn zero_edit_payload_yields_no_plain_quickfix() {
        // CW100's fix has no span edit — `code_actions_from_diagnostics` must
        // not offer a quickfix with an empty `TextEdit` list for it; the
        // dedicated create-loc-key action (built elsewhere) covers it.
        let uri: Url = "file:///mod/common/x.txt".parse().unwrap();
        let diag = create_loc_key_diag();
        let actions = code_actions_from_diagnostics(
            &uri,
            std::slice::from_ref(&diag),
            "my_thing = { x = yes }\n",
            &PositionEncodingKind::UTF16,
        );
        assert!(actions.is_empty(), "got: {actions:?}");
    }

    #[test]
    fn fixable_span_edits_normal_payload_yields_pairs() {
        let fix = SuggestedFix::replace("Rename to set_name", range(1, 0, 1, 15), "set_name");
        let diag = Diagnostic {
            code: Some(NumberOrString::String("CW253".into())),
            data: Some(fix_to_data(&fix)),
            ..Default::default()
        };
        let pairs = fixable_span_edits(&diag);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "CW253");
        assert_eq!(pairs[0].1.range, range(1, 0, 1, 15));
        assert_eq!(pairs[0].1.replacement, "set_name");
    }

    #[test]
    fn fixable_span_edits_no_data_yields_empty() {
        let diag = Diagnostic {
            code: Some(NumberOrString::String("CW253".into())),
            ..Default::default()
        };
        assert!(fixable_span_edits(&diag).is_empty());
    }

    #[test]
    fn fixable_span_edits_numeric_code_yields_empty() {
        // `fix_from_data` parses fine, but a numeric `code` can't be stored as a
        // key in the fixAllWorkspace store, so the pair is dropped rather than
        // stringified.
        let fix = SuggestedFix::replace("Rename to set_name", range(1, 0, 1, 15), "set_name");
        let diag = Diagnostic {
            code: Some(NumberOrString::Number(253)),
            data: Some(fix_to_data(&fix)),
            ..Default::default()
        };
        assert!(fixable_span_edits(&diag).is_empty());
    }

    #[test]
    fn fixable_span_edits_create_loc_key_payload_yields_empty() {
        // The invariant that keeps CW100 out of the fixAllWorkspace store
        // (`publish_filtered`'s doc comment): a create-loc-key payload parses
        // fine but carries no span edits, so this must report nothing for it.
        let diag = create_loc_key_diag();
        assert!(fixable_span_edits(&diag).is_empty());
    }

    #[test]
    fn fix_all_action_does_not_claim_create_loc_key_diagnostics() {
        // Mix a normal span-edit fix with a CW100 create-loc-key fix.
        // `source.fixAll` must apply only the real edit and must not list the
        // CW100 diagnostic among the ones it resolves.
        let text = "set_empire_name = { }\nmy_thing = { x = yes }\n";
        let real_fix = SuggestedFix::replace("Rename to set_name", range(1, 0, 1, 15), "set_name");
        let real_diag = Diagnostic {
            range: Range::default(),
            severity: Some(DiagnosticSeverity::WARNING),
            code: Some(NumberOrString::String("CW253".into())),
            source: Some("cwtools".into()),
            message: "renamed effect".into(),
            data: Some(fix_to_data(&real_fix)),
            ..Default::default()
        };
        let loc_diag = create_loc_key_diag();
        let uri: Url = "file:///mod/common/x.txt".parse().unwrap();
        let action = fix_all_action(
            &uri,
            &[real_diag.clone(), loc_diag],
            text,
            &PositionEncodingKind::UTF16,
        )
        .expect("one real edit survives");
        let CodeActionOrCommand::CodeAction(action) = action else {
            panic!("expected a CodeAction");
        };
        assert_eq!(action.title, "Fix all (1 auto-fixable)");
        let resolved = action.diagnostics.expect("resolved diagnostics");
        assert_eq!(resolved.len(), 1, "must not claim the CW100 diagnostic");
        assert_eq!(resolved[0].code, real_diag.code);
    }
}
