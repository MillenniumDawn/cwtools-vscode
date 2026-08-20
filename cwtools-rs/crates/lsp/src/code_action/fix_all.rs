use std::collections::HashMap;

use tower_lsp::lsp_types::*;

use cwtools_parser::fix::{SpanEdit, plan_file_edits};

use crate::Backend;

use super::payload::source_range_to_lsp;

/// One URI's resolved fix-all-workspace edits: the survivors after overlap
/// resolution (`plan_file_edits`), and how many were dropped for overlapping
/// another kept edit.
struct PlannedFileFixes {
    uri: String,
    kept: Vec<SpanEdit>,
    skipped: usize,
}

/// Resolve every URI's stored fixable edits against its current text via
/// `plan_file_edits` — the same overlap resolution `source.fixAll` and the
/// CLI `fix` subcommand use, so all three agree on what a fixed workspace
/// looks like. `texts` maps URI -> current text; a URI missing from it (the
/// file couldn't be read when the command ran) is dropped. Pure: the caller
/// does the reads and hands the results in.
fn plan_workspace_fixes(
    snapshot: &HashMap<String, Vec<(String, SpanEdit)>>,
    texts: &HashMap<String, String>,
) -> Vec<PlannedFileFixes> {
    snapshot
        .iter()
        .filter_map(|(uri, entries)| {
            let text = texts.get(uri)?;
            let (kept, skipped) = plan_file_edits(text, entries.clone());
            Some(PlannedFileFixes {
                uri: uri.clone(),
                kept,
                skipped: skipped.len(),
            })
        })
        .collect()
}

/// The `workspace/applyEdit` `changes` map for every file with at least one
/// surviving edit, converted to LSP `TextEdit`s with the negotiated encoding
/// against each file's own text. A URI that fails to parse (never observed —
/// it round-tripped through `publish_filtered` as a valid URI) is dropped
/// rather than panicking.
fn workspace_edit_changes(
    planned: &[PlannedFileFixes],
    texts: &HashMap<String, String>,
    encoding: &PositionEncodingKind,
) -> HashMap<Url, Vec<TextEdit>> {
    let mut changes = HashMap::new();
    for pf in planned {
        if pf.kept.is_empty() {
            continue;
        }
        let Ok(uri) = Url::parse(&pf.uri) else {
            continue;
        };
        let text = texts.get(&pf.uri).map(String::as_str).unwrap_or("");
        let edits: Vec<TextEdit> = pf
            .kept
            .iter()
            .map(|e| TextEdit {
                range: source_range_to_lsp(e.range, text, encoding),
                new_text: e.replacement.clone(),
            })
            .collect();
        changes.insert(uri, edits);
    }
    changes
}

/// Build the negotiated workspace edit from the exact snapshots used to
/// calculate its ranges. Versioned document changes prevent a supported client
/// from applying edits to a newer open-document version.
fn workspace_edit_for_snapshots(
    changes: HashMap<Url, Vec<TextEdit>>,
    snapshots: &HashMap<String, crate::FileTextSnapshot>,
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

/// The command's result message when every fixable file was refused by the
/// edit boundary. Distinct from the empty-store message: the problems are real
/// and the user can see them in the panel, they just aren't in a file cwtools
/// will write to.
fn outside_workspace_summary(refused: usize) -> String {
    format!("Skipped {refused} file(s) with auto-fixable problems: they are outside the workspace.")
}

fn fixable_edits_match(
    expected: &crate::FixableEdits,
    current: Option<&crate::FileTextSnapshot>,
) -> bool {
    let Some(current) = current else {
        return false;
    };
    expected.version.map_or_else(
        || {
            expected
                .content_hash
                .is_some_and(|hash| hash == current.content_hash)
        },
        |version| current.version == Some(version),
    )
}

/// The command's result message on success. Stale and overlapping edits are
/// reported separately from files refused by the workspace edit boundary.
fn fix_all_workspace_summary(
    edits_applied: usize,
    files_changed: usize,
    skipped_stale: usize,
    skipped_overlapping: usize,
    skipped_outside: usize,
) -> String {
    let mut msg = format!("Applied {edits_applied} fix(es) across {files_changed} file(s)");
    let skipped = skipped_stale + skipped_overlapping;
    if skipped > 0 {
        let reason = match (skipped_stale > 0, skipped_overlapping > 0) {
            (true, true) => format!("{skipped_stale} stale, {skipped_overlapping} overlapping"),
            (true, false) => "stale".to_string(),
            (false, true) => "overlapping".to_string(),
            (false, false) => unreachable!(),
        };
        msg.push_str(&format!("; {skipped} skipped ({reason})"));
    }
    if skipped_outside > 0 {
        msg.push_str(&format!(
            "; {skipped_outside} file(s) skipped (outside workspace)"
        ));
    }
    msg
}

impl Backend {
    /// `fixAllWorkspace` execute-command handler. Returns the message shown to
    /// the user (no result payload otherwise): a "nothing to do" message when
    /// the store is empty or every entry resolved to zero edits, a "skipped"
    /// message when the only fixable files were outside the workspace, stale
    /// edits are dropped and counted, an error message when the client rejects
    /// the `workspace/applyEdit`, else the summary from
    /// [`fix_all_workspace_summary`].
    pub(crate) async fn fix_all_workspace_impl(&self) -> String {
        let mut snapshot = self.state.fixable_edits.lock().clone();
        if snapshot.is_empty() {
            return "No auto-fixable problems in the workspace.".to_string();
        }

        // The edit boundary is workspace-only and performs synchronous
        // canonicalization, so keep it off the request worker too (#160).
        let edit_roots = self.state.config.read().editable_roots.clone();
        let candidate_uris: Vec<String> = snapshot.keys().cloned().collect();
        let editable = tokio::task::spawn_blocking(move || {
            candidate_uris
                .into_iter()
                .filter(|uri| crate::access::editable_path(uri, &edit_roots).is_ok())
                .collect::<std::collections::HashSet<_>>()
        })
        .await
        .unwrap_or_default();
        let fixable = snapshot.len();
        snapshot.retain(|uri, _| editable.contains(uri));
        let refused = fixable - snapshot.len();
        if snapshot.is_empty() {
            return outside_workspace_summary(refused);
        }

        let uris: Vec<String> = snapshot.keys().cloned().collect();
        let current = self.file_text_snapshots_for(&uris).await;
        let mut stale_entries = Vec::new();
        snapshot.retain(|uri, expected| {
            let matches = fixable_edits_match(expected, current.get(uri));
            if !matches {
                stale_entries.push((uri.clone(), expected.clone()));
            }
            matches
        });
        let skipped_stale: usize = stale_entries
            .iter()
            .map(|(_, entry)| entry.entries.len())
            .sum();
        if !stale_entries.is_empty() {
            let mut store = self.state.fixable_edits.lock();
            for (uri, expected) in stale_entries {
                if store.get(&uri) == Some(&expected) {
                    store.remove(&uri);
                }
            }
        }
        if snapshot.is_empty() {
            return fix_all_workspace_summary(0, 0, skipped_stale, 0, refused);
        }

        let edits_snapshot: HashMap<String, Vec<(String, SpanEdit)>> = snapshot
            .into_iter()
            .map(|(uri, entry)| (uri, entry.entries))
            .collect();
        let texts: HashMap<String, String> = current
            .iter()
            .filter(|(uri, _)| edits_snapshot.contains_key(*uri))
            .map(|(uri, snapshot)| (uri.clone(), snapshot.text.clone()))
            .collect();
        let planned = plan_workspace_fixes(&edits_snapshot, &texts);
        let encoding = self.state.config.read().position_encoding.clone();
        let skipped_overlapping: usize = planned.iter().map(|pf| pf.skipped).sum();
        let changes = workspace_edit_changes(&planned, &texts, &encoding);
        if changes.is_empty() {
            if skipped_stale > 0 || skipped_overlapping > 0 {
                return fix_all_workspace_summary(
                    0,
                    0,
                    skipped_stale,
                    skipped_overlapping,
                    refused,
                );
            }
            return "No auto-fixable problems in the workspace.".to_string();
        }
        let edits_applied: usize = changes.values().map(Vec::len).sum();
        let files_changed = changes.len();
        let document_changes = self
            .state
            .workspace_edit_document_changes
            .load(std::sync::atomic::Ordering::Relaxed);
        let edit = workspace_edit_for_snapshots(changes, &current, document_changes);
        match self.client.apply_edit(edit).await {
            Ok(resp) if resp.applied => fix_all_workspace_summary(
                edits_applied,
                files_changed,
                skipped_stale,
                skipped_overlapping,
                refused,
            ),
            Ok(resp) => format!(
                "The client rejected the workspace edit{}.",
                resp.failure_reason
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
            ),
            Err(e) => format!("The client rejected the workspace edit: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_parser::ast::{SourcePos, SourceRange};

    fn range(sl: u32, sc: u16, el: u32, ec: u16) -> SourceRange {
        SourceRange {
            start: SourcePos { line: sl, col: sc },
            end: SourcePos { line: el, col: ec },
        }
    }

    fn span(sl: u32, sc: u16, el: u32, ec: u16, repl: &str) -> SpanEdit {
        SpanEdit {
            range: range(sl, sc, el, ec),
            replacement: repl.to_string(),
        }
    }

    #[test]
    fn plan_workspace_fixes_drops_uris_with_no_readable_text() {
        let mut snapshot = HashMap::new();
        snapshot.insert(
            "file:///a.txt".to_string(),
            vec![("CW253".to_string(), span(1, 0, 1, 4, "X"))],
        );
        snapshot.insert(
            "file:///unreadable.txt".to_string(),
            vec![("CW253".to_string(), span(1, 0, 1, 4, "X"))],
        );
        let mut texts = HashMap::new();
        texts.insert("file:///a.txt".to_string(), "aaaa bbbb\n".to_string());
        let planned = plan_workspace_fixes(&snapshot, &texts);
        assert_eq!(planned.len(), 1, "the unreadable URI is dropped");
        assert_eq!(planned[0].uri, "file:///a.txt");
        assert_eq!(planned[0].kept.len(), 1);
        assert_eq!(planned[0].skipped, 0);
    }

    #[test]
    fn plan_workspace_fixes_reports_overlap_skips_per_file() {
        let mut snapshot = HashMap::new();
        // "aaaa b" and "bbbb" share column 5 — the same overlap fixture as
        // `plan_file_edits`'s own test.
        snapshot.insert(
            "file:///a.txt".to_string(),
            vec![
                ("A".to_string(), span(1, 0, 1, 6, "X")),
                ("B".to_string(), span(1, 5, 1, 9, "Y")),
            ],
        );
        let mut texts = HashMap::new();
        texts.insert("file:///a.txt".to_string(), "aaaa bbbb\n".to_string());
        let planned = plan_workspace_fixes(&snapshot, &texts);
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].kept.len(), 1);
        assert_eq!(planned[0].skipped, 1);
    }

    #[test]
    fn workspace_edit_changes_skips_files_with_nothing_kept() {
        let planned = vec![
            PlannedFileFixes {
                uri: "file:///a.txt".to_string(),
                kept: vec![SpanEdit {
                    range: range(1, 0, 1, 4),
                    replacement: "X".to_string(),
                }],
                skipped: 0,
            },
            PlannedFileFixes {
                uri: "file:///b.txt".to_string(),
                kept: Vec::new(),
                skipped: 1,
            },
        ];
        let mut texts = HashMap::new();
        texts.insert("file:///a.txt".to_string(), "aaaa bbbb\n".to_string());
        texts.insert("file:///b.txt".to_string(), "cccc\n".to_string());
        let changes = workspace_edit_changes(&planned, &texts, &PositionEncodingKind::UTF16);
        assert_eq!(changes.len(), 1, "only the file with a surviving edit");
        let uri: Url = "file:///a.txt".parse().unwrap();
        assert_eq!(changes[&uri][0].new_text, "X");
    }

    #[test]
    fn workspace_edit_for_snapshots_uses_captured_versions() {
        let uri: Url = "file:///a.txt".parse().unwrap();
        let mut changes = HashMap::new();
        changes.insert(
            uri.clone(),
            vec![TextEdit {
                range: Range::new(Position::new(0, 0), Position::new(0, 4)),
                new_text: "X".to_string(),
            }],
        );
        let mut snapshots = HashMap::new();
        snapshots.insert(
            uri.to_string(),
            crate::FileTextSnapshot {
                text: "aaaa\n".to_string(),
                version: Some(7),
                content_hash: 0,
            },
        );

        let edit = workspace_edit_for_snapshots(changes, &snapshots, true);
        let Some(DocumentChanges::Edits(edits)) = edit.document_changes else {
            panic!("expected versioned document changes");
        };
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].text_document.uri, uri);
        assert_eq!(edits[0].text_document.version, Some(7));
        assert!(edit.changes.is_none());
    }

    #[test]
    fn fix_all_workspace_summary_wording() {
        assert_eq!(
            fix_all_workspace_summary(3, 2, 0, 0, 0),
            "Applied 3 fix(es) across 2 file(s)"
        );
        assert_eq!(
            fix_all_workspace_summary(3, 2, 0, 1, 0),
            "Applied 3 fix(es) across 2 file(s); 1 skipped (overlapping)"
        );
        assert_eq!(
            fix_all_workspace_summary(3, 2, 0, 0, 1),
            "Applied 3 fix(es) across 2 file(s); 1 file(s) skipped (outside workspace)"
        );
    }

    #[test]
    fn fix_all_workspace_rejects_stale_versions_and_content() {
        let edit = SpanEdit {
            range: range(1, 0, 1, 4),
            replacement: "X".to_string(),
        };
        let versioned = crate::FixableEdits {
            entries: vec![("CW281".to_string(), edit.clone())],
            version: Some(1),
            content_hash: None,
        };
        let open_v2 = crate::FileTextSnapshot {
            text: "aaaa\n".to_string(),
            version: Some(2),
            content_hash: 1,
        };
        assert!(!fixable_edits_match(&versioned, Some(&open_v2)));

        let closed = crate::FixableEdits {
            entries: vec![("CW281".to_string(), edit)],
            version: None,
            content_hash: Some(7),
        };
        let changed = crate::FileTextSnapshot {
            text: "bbbb\n".to_string(),
            version: None,
            content_hash: 8,
        };
        assert!(!fixable_edits_match(&closed, Some(&changed)));
        let unchanged = crate::FileTextSnapshot {
            content_hash: 7,
            ..changed
        };
        assert!(fixable_edits_match(&closed, Some(&unchanged)));
    }

    /// "Nothing to fix" and "everything fixable is off-limits" are different
    /// answers: the second leaves problems the user can still see in the panel,
    /// so saying there are none would read as a bug in the command.
    #[test]
    fn outside_workspace_summary_does_not_claim_there_is_nothing_to_fix() {
        let msg = outside_workspace_summary(2);
        assert_eq!(
            msg,
            "Skipped 2 file(s) with auto-fixable problems: they are outside the workspace."
        );
        assert!(!msg.contains("No auto-fixable problems"));
    }
}
