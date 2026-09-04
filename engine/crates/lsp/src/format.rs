use std::collections::HashMap;
use std::path::PathBuf;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use cwtools_parser::ast::SourcePos;
use cwtools_parser::fix::{EOF_POS, SpanEdit};
use cwtools_parser::format::{format_edits, format_range_edits};

use crate::command_progress::CommandProgress;
use crate::lines::DocLines;
use crate::paths::{lsp_pos_to_source_in_text, path_to_uri, uri_to_path_str};
use crate::{Backend, FileTextSnapshot};

enum DiscoverOutcome {
    Cancelled,
    Failed(String),
    Files(Vec<cwtools_file_manager::file_manager::DiscoveredFile>),
}

async fn hold_format_for_tests(progress: &CommandProgress) {
    let Some(ms) = std::env::var("CWTOOLS_FORMAT_HOLD_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
    else {
        return;
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        if progress.is_cancelled() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

impl Backend {
    pub(crate) async fn formatting_impl(
        &self,
        params: DocumentFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri.to_string();
        let Some(text) = self.file_text_for(&uri).await else {
            return Ok(None);
        };
        let opts = self
            .state
            .config
            .read()
            .formatting
            .with_editor(params.options.tab_size, params.options.insert_spaces);
        let table = self.state.string_table.clone();
        let edits = tokio::task::block_in_place(|| format_edits(&text, &table, &opts));
        Ok(text_edits_or_none(&edits, &text, &self.position_encoding()))
    }

    pub(crate) async fn range_formatting_impl(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri.to_string();
        let Some(text) = self.file_text_for(&uri).await else {
            return Ok(None);
        };
        let encoding = self.position_encoding();
        let opts = self
            .state
            .config
            .read()
            .formatting
            .with_editor(params.options.tab_size, params.options.insert_spaces);
        let (sl, sc) = lsp_pos_to_source_in_text(&text, params.range.start, &encoding);
        let (el, ec) = lsp_pos_to_source_in_text(&text, params.range.end, &encoding);
        let range = cwtools_parser::ast::SourceRange {
            start: SourcePos { line: sl, col: sc },
            end: SourcePos { line: el, col: ec },
        };
        let table = self.state.string_table.clone();
        let edits = tokio::task::block_in_place(|| format_range_edits(&text, &table, &opts, range));
        Ok(text_edits_or_none(&edits, &text, &encoding))
    }

    pub(crate) async fn format_workspace_impl(&self, token: Option<ProgressToken>) -> String {
        let progress = CommandProgress::begin(self, token, "CWTools: Format workspace", true).await;
        hold_format_for_tests(&progress).await;
        if progress.is_cancelled() {
            let msg = "Cancelled; no files were changed.".to_string();
            progress.finish(Some(msg.clone())).await;
            return msg;
        }

        let snapshot = {
            let cfg = self.state.config.read();
            cfg.workspace_uri.as_ref().map(|uri| {
                (
                    PathBuf::from(uri_to_path_str(uri)),
                    cfg.ignore_file_patterns.clone(),
                    cfg.ignore_dir_patterns.clone(),
                    cfg.formatting,
                    cfg.position_encoding.clone(),
                    cfg.editable_roots.clone(),
                )
            })
        };
        let Some((root_path, extra_file_globs, extra_dir_globs, formatting, encoding, edit_roots)) =
            snapshot
        else {
            let msg = "No workspace folder.".to_string();
            progress.finish(Some(msg.clone())).await;
            return msg;
        };
        let document_changes = self
            .state
            .workspace_edit_document_changes
            .load(std::sync::atomic::Ordering::Relaxed);
        let ruleset = self.state.rules.read().ruleset.clone();
        let cancel = progress.cancel_flag();

        let discovered = match tokio::task::spawn_blocking(move || {
            if cancel.is_cancelled() {
                return DiscoverOutcome::Cancelled;
            }
            let mut fm_config =
                cwtools_driver::workspace_discovery_config(&root_path, ruleset.as_deref());
            fm_config
                .exclude_patterns
                .extend(extra_file_globs.iter().cloned());
            fm_config
                .exclude_dir_patterns
                .extend(extra_dir_globs.iter().cloned());
            match cwtools_driver::discover_workspace_files(fm_config) {
                Ok(files) => DiscoverOutcome::Files(files),
                Err(error) => DiscoverOutcome::Failed(error.to_string()),
            }
        })
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => {
                let msg = "Workspace discovery failed.".to_string();
                progress.finish(Some(msg.clone())).await;
                return msg;
            }
        };

        let files = match discovered {
            DiscoverOutcome::Cancelled => {
                let msg = "Cancelled; no files were changed.".to_string();
                progress.finish(Some(msg.clone())).await;
                return msg;
            }
            DiscoverOutcome::Failed(error) => {
                let msg = format!("Workspace discovery failed: {error}");
                progress.finish(Some(msg.clone())).await;
                return msg;
            }
            DiscoverOutcome::Files(files) => files,
        };

        let mut uris: Vec<String> = Vec::new();
        for file in &files {
            let uri = path_to_uri(&file.path);
            if crate::access::editable_path(&uri, &edit_roots).is_ok() {
                uris.push(uri);
            }
        }
        if uris.is_empty() {
            let msg = "No files needed formatting.".to_string();
            progress.finish(Some(msg.clone())).await;
            return msg;
        }

        let snapshots = self.file_text_snapshots_for(&uris).await;
        if progress.is_cancelled() {
            let msg = "Cancelled; no files were changed.".to_string();
            progress.finish(Some(msg.clone())).await;
            return msg;
        }

        let table = self.state.string_table.clone();
        let cancel = progress.cancel_flag();
        let planned = tokio::task::spawn_blocking(move || {
            let mut skipped = 0usize;
            let mut changed: Vec<(String, String, Vec<SpanEdit>)> = Vec::new();
            for uri in uris {
                if cancel.is_cancelled() {
                    return None;
                }
                let Some(snapshot) = snapshots.get(&uri) else {
                    skipped += 1;
                    continue;
                };
                match cwtools_parser::format::format_text(&snapshot.text, &table, &formatting) {
                    None => skipped += 1,
                    Some(formatted) if formatted == snapshot.text => {}
                    Some(_) => {
                        let edits = format_edits(&snapshot.text, &table, &formatting);
                        if !edits.is_empty() {
                            changed.push((uri, snapshot.text.clone(), edits));
                        }
                    }
                }
            }
            Some((changed, skipped, snapshots))
        })
        .await
        .ok()
        .flatten();

        let Some((changed, skipped, snapshots)) = planned else {
            let msg = "Cancelled; no files were changed.".to_string();
            progress.finish(Some(msg.clone())).await;
            return msg;
        };
        if changed.is_empty() {
            let msg = if skipped == 0 {
                "No files needed formatting.".to_string()
            } else {
                format!("No files needed formatting; skipped {skipped} (parse errors).")
            };
            progress.finish(Some(msg.clone())).await;
            return msg;
        }

        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        for (uri, text, edits) in &changed {
            let Ok(url) = Url::parse(uri) else { continue };
            let lines = DocLines::new(text, encoding.clone());
            let mapped: Vec<TextEdit> =
                edits.iter().map(|e| span_to_text_edit(e, &lines)).collect();
            changes.insert(url, mapped);
        }
        let files_changed = changes.len();
        if changes.is_empty() {
            let msg = "No files needed formatting.".to_string();
            progress.finish(Some(msg.clone())).await;
            return msg;
        }
        if progress.is_cancelled() {
            let msg = "Cancelled; no files were changed.".to_string();
            progress.finish(Some(msg.clone())).await;
            return msg;
        }

        let edit = workspace_edit_for_snapshots(changes, &snapshots, document_changes);
        let msg = match self.client.apply_edit(edit).await {
            Ok(resp) if resp.applied => {
                let mut msg = format!("Formatted {files_changed} file(s)");
                if skipped > 0 {
                    msg.push_str(&format!("; skipped {skipped} (parse errors)"));
                }
                msg
            }
            Ok(resp) => format!(
                "The client rejected the workspace edit{}.",
                resp.failure_reason
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
            ),
            Err(e) => format!("The client rejected the workspace edit: {e}"),
        };
        progress.finish(Some(msg.clone())).await;
        msg
    }

    /// The encoding the client negotiated at `initialize`, which every published
    pub(crate) fn position_encoding(&self) -> PositionEncodingKind {
        self.state.config.read().position_encoding.clone()
    }
}

fn text_edits_or_none(
    edits: &[SpanEdit],
    text: &str,
    encoding: &PositionEncodingKind,
) -> Option<Vec<TextEdit>> {
    if edits.is_empty() {
        None
    } else {
        let lines = DocLines::new(text, encoding.clone());
        Some(edits.iter().map(|e| span_to_text_edit(e, &lines)).collect())
    }
}

fn span_to_text_edit(edit: &SpanEdit, lines: &DocLines) -> TextEdit {
    TextEdit {
        range: Range {
            start: lines.position(
                edit.range.start.line.saturating_sub(1),
                u32::from(edit.range.start.col),
            ),
            end: if edit.range.end == EOF_POS {
                lines.document_end_position()
            } else {
                lines.position(
                    edit.range.end.line.saturating_sub(1),
                    u32::from(edit.range.end.col),
                )
            },
        },
        new_text: edit.replacement.clone(),
    }
}

fn workspace_edit_for_snapshots(
    changes: HashMap<Url, Vec<TextEdit>>,
    snapshots: &HashMap<String, FileTextSnapshot>,
    document_changes: bool,
) -> WorkspaceEdit {
    if document_changes {
        let edits = changes
            .into_iter()
            .map(|(uri, edits)| TextDocumentEdit {
                text_document: OptionalVersionedTextDocumentIdentifier {
                    version: snapshots.get(uri.as_str()).and_then(|s| s.version),
                    uri,
                },
                edits: edits.into_iter().map(OneOf::Left).collect(),
            })
            .collect();
        WorkspaceEdit {
            changes: None,
            document_changes: Some(DocumentChanges::Edits(edits)),
            change_annotations: None,
        }
    } else {
        WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_parser::ast::SourceRange;

    #[test]
    fn eof_text_edit_end_uses_the_actual_document_end_for_each_encoding() {
        let last_line = format!("{}😀", "x".repeat(usize::from(u16::MAX) + 1));
        let text = format!("first\n{last_line}");
        let edit = SpanEdit {
            range: SourceRange {
                start: SourcePos { line: 1, col: 0 },
                end: EOF_POS,
            },
            replacement: String::new(),
        };

        for (encoding, expected_character) in [
            (PositionEncodingKind::UTF16, 65_538),
            (PositionEncodingKind::UTF32, 65_537),
        ] {
            let lines = DocLines::new(&text, encoding);
            let mapped = span_to_text_edit(&edit, &lines);
            assert_eq!(mapped.range.end, Position::new(1, expected_character));
        }
    }
}
