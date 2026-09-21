use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::Backend;
use crate::lines::{DocLines, index_snapshots};
use crate::paths::logical_path_from_uri;

use super::{
    at_var_at_cursor, code_token_cols_in_line, prepare_rename_range, rename_refused,
    word_at_position,
};
use crate::navigation::helpers::{TokenCase, loc_ref_key_cols_in_line, loc_root};

impl Backend {
    pub(crate) async fn prepare_rename_impl(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let uri = params.text_document.uri.to_string();
        let pos = params.position;
        let ws_prefix = self.state.config.read().workspace_prefix.clone();
        let logical_path = logical_path_from_uri(&uri, &ws_prefix);

        let text = self.file_text_for(&uri).await;
        let position_encoding = self.position_encoding();
        let lines = text
            .as_deref()
            .map(|text| DocLines::new(text, position_encoding.clone()));
        if let (Some(text), Some(lines)) = (text.as_deref(), lines.as_ref())
            && let Some((_, range)) = Self::at_var_rename_target(text, lines, pos)
        {
            return Ok(Some(PrepareRenameResponse::Range(range)));
        }

        if let Some(key_lower) = self.loc_key_at_cursor(&uri, pos, &logical_path).await {
            // `lines` is `text.as_deref().map(..)`, so it is `Some` on exactly
            // the same condition and the index answers this without the
            // document rescan `lsp_pos_to_source_in_text` would do (#471).
            let token = if let (Some(t), Some(lines)) = (text.as_deref(), lines.as_ref()) {
                let col = lines.source_column(pos);
                word_at_position(t, pos.line, col as u32).unwrap_or_else(|| key_lower.clone())
            } else {
                key_lower.clone()
            };
            let range = prepare_rename_range(text.as_deref(), pos, &token, &position_encoding);
            return Ok(Some(PrepareRenameResponse::Range(range)));
        }

        let type_ref = self.type_ref_at_cursor(&uri, pos, &logical_path);

        if let Some((_, instance_name)) = type_ref {
            let range =
                prepare_rename_range(text.as_deref(), pos, &instance_name, &position_encoding);
            return Ok(Some(PrepareRenameResponse::Range(range)));
        }
        Ok(None)
    }

    /// `lines` is built from the document's text, so it is also the line
    /// source here; there is no second `text.lines()` pass and no `text`
    /// parameter left to take (#471).
    fn rename_at_var(
        &self,
        uri: &str,
        lines: &DocLines,
        name: &str,
        new_name: &str,
    ) -> Result<Option<WorkspaceEdit>> {
        let edits: Vec<TextEdit> = lines
            .iter()
            .flat_map(|(line0, line)| {
                code_token_cols_in_line(line, name, TokenCase::Sensitive)
                    .into_iter()
                    .map(move |col| TextEdit {
                        range: lines.token_range(line0, col, name),
                        new_text: new_name.to_string(),
                    })
            })
            .collect();
        if edits.is_empty() {
            return Ok(None);
        }
        let by_uri = vec![(uri.to_string(), edits)];
        if let Some(refused) = self.first_refused_edit_target(&by_uri, uri) {
            return Err(refused);
        }
        Ok(Some(self.build_workspace_edit(by_uri)))
    }

    fn at_var_rename_target(
        text: &str,
        lines: &DocLines,
        pos: Position,
    ) -> Option<(String, Range)> {
        let col = lines.source_column(pos);
        let (name, start_col) = at_var_at_cursor(text, pos.line, col as u32)?;
        let range = lines.token_range(pos.line, start_col, &name);
        Some((name, range))
    }

    async fn rename_loc(
        &self,
        uri: &str,
        key_lower: &str,
        new_name: &str,
    ) -> Result<Option<WorkspaceEdit>> {
        let root = loc_root(key_lower);
        let trigger_suffix = key_lower.strip_prefix(&root).unwrap_or("");
        let new_lower = new_name.to_lowercase();
        let new_root_original = if !trigger_suffix.is_empty() && new_lower.ends_with(trigger_suffix)
        {
            let suffix_len = trigger_suffix.len();
            let end = new_name.len().saturating_sub(suffix_len);
            if new_name[end..].eq_ignore_ascii_case(trigger_suffix) {
                new_name[..end].to_string()
            } else {
                new_name.to_string()
            }
        } else {
            new_name.to_string()
        };
        let candidates: Vec<String> = vec![
            root.clone(),
            format!("{root}_desc"),
            format!("{root}_tooltip"),
            format!("{root}_desc_tooltip"),
            format!("{root}_tooltip_desc"),
        ];
        let mut target_to_new: HashMap<String, String> = HashMap::new();
        for cand in candidates {
            if cand == key_lower || self.is_known_loc_key(&cand) {
                let suffix = cand.strip_prefix(&root).unwrap_or("");
                let new_sib = format!("{}{}", new_root_original, suffix);
                target_to_new.entry(cand).or_insert(new_sib);
            }
        }
        if !target_to_new.contains_key(key_lower) {
            target_to_new.insert(key_lower.to_string(), new_name.to_string());
        }
        if uri.parse::<Url>().is_err() {
            return Ok(None);
        }
        let target_to_new = Arc::new(target_to_new);
        let loc_uris = self.loc_file_uris().await;
        let loc_edits = self
            .collect_loc_rename_loc_edits(Arc::clone(&target_to_new), loc_uris)
            .await;
        let script_edits = self.collect_loc_rename_usages(target_to_new).await;
        let mut by_uri: HashMap<String, Vec<TextEdit>> = HashMap::new();
        for (file_uri, edits) in loc_edits.into_iter().chain(script_edits) {
            by_uri.entry(file_uri).or_default().extend(edits);
        }
        if by_uri.is_empty() {
            return Ok(None);
        }
        let mut deduped: HashMap<String, Vec<TextEdit>> = HashMap::new();
        for (file_uri, mut edits) in by_uri {
            edits.sort_by_key(|a| a.range.start);
            edits.dedup_by(|a, b| a.range == b.range);
            deduped.insert(file_uri, edits);
        }
        let by_uri: Vec<(String, Vec<TextEdit>)> = deduped.into_iter().collect();
        if let Some(err) = self.first_refused_edit_target(&by_uri, uri) {
            return Err(err);
        }
        Ok(Some(self.build_workspace_edit(by_uri)))
    }

    /// The edits inside loc files: each key's definition lines, and every
    /// `$key$` reference in a value. One streamed pass, each file parsed once
    /// and dropped; the definitions come from the parser rather than the loc
    /// index because a rename has to reach every language on disk, and the
    /// index only holds the configured ones (#474).
    async fn collect_loc_rename_loc_edits(
        &self,
        target_to_new: Arc<HashMap<String, String>>,
        loc_uris: Vec<String>,
    ) -> Vec<(String, Vec<TextEdit>)> {
        if loc_uris.is_empty() {
            return Vec::new();
        }
        let encoding = self.position_encoding();
        self.scan_workspace_texts(loc_uris, move |uri, text| {
            let path = crate::paths::uri_to_path_str(uri);
            let files =
                cwtools_localization::parse_loc_files(&path, text, None).unwrap_or_default();
            let lines = DocLines::new(text, encoding.clone());
            let mut edits = Vec::new();
            for entry in files.iter().flat_map(|file| &file.entries) {
                let lower = entry.key.to_lowercase();
                let Some(new_text) = target_to_new.get(&lower) else {
                    continue;
                };
                let line0 = (entry.position.line.saturating_sub(1)) as u32;
                let line_text = lines.line(line0);
                let col = line_text
                    .find(&entry.key)
                    .map(|b| line_text[..b].chars().count() as u32)
                    .unwrap_or(0);
                edits.push(TextEdit {
                    range: lines.token_range(line0, col, &entry.key),
                    new_text: new_text.clone(),
                });
            }
            let lower = text.to_ascii_lowercase();
            for ((line0, line), lower_line) in lines.iter().zip(lower.lines()) {
                if !lower_line.contains('$') {
                    continue;
                }
                for (key_lower, new_text) in target_to_new.iter() {
                    if !lower_line.contains(key_lower.as_str()) {
                        continue;
                    }
                    for col in loc_ref_key_cols_in_line(line, key_lower) {
                        edits.push(TextEdit {
                            range: lines.token_range(line0, col, key_lower),
                            new_text: new_text.clone(),
                        });
                    }
                }
            }
            if edits.is_empty() {
                Vec::new()
            } else {
                vec![(uri.to_string(), edits)]
            }
        })
        .await
    }

    /// The edits inside script files, from the same streamed scan
    /// find-references uses for its usage half.
    async fn collect_loc_rename_usages(
        &self,
        target_to_new: Arc<HashMap<String, String>>,
    ) -> Vec<(String, Vec<TextEdit>)> {
        let script_uris = self.script_uris();
        if script_uris.is_empty() {
            return Vec::new();
        }
        let encoding = self.position_encoding();
        self.scan_workspace_texts(script_uris, move |uri, text| {
            let lines = DocLines::new(text, encoding.clone());
            let lower = text.to_ascii_lowercase();
            let mut edits = Vec::new();
            for ((line0, line), lower_line) in lines.iter().zip(lower.lines()) {
                for (key_lower, new_text) in target_to_new.iter() {
                    if !lower_line.contains(key_lower.as_str()) {
                        continue;
                    }
                    for col in code_token_cols_in_line(line, key_lower, TokenCase::AsciiInsensitive)
                    {
                        edits.push(TextEdit {
                            range: lines.token_range(line0, col, key_lower),
                            new_text: new_text.clone(),
                        });
                    }
                }
            }
            if edits.is_empty() {
                Vec::new()
            } else {
                vec![(uri.to_string(), edits)]
            }
        })
        .await
    }

    /// install (#160). Dropping those quietly would apply a rename the user
    fn first_refused_edit_target(
        &self,
        by_uri: &[(String, Vec<TextEdit>)],
        own_uri: &str,
    ) -> Option<tower_lsp::jsonrpc::Error> {
        if let [(only, _)] = by_uri
            && only == own_uri
        {
            return None;
        }
        let edit_roots = self.state.config.read().editable_roots.clone();
        by_uri.iter().find_map(|(uri, _)| {
            let refusal = crate::access::editable_path(uri, &edit_roots).err()?;
            Some(rename_refused(uri, refusal))
        })
    }

    fn build_workspace_edit(&self, by_uri: Vec<(String, Vec<TextEdit>)>) -> WorkspaceEdit {
        if self
            .state
            .workspace_edit_document_changes
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            let docs = self.state.documents.lock();
            let edits = by_uri
                .into_iter()
                .filter_map(|(uri, edits)| {
                    let url = uri.parse::<Url>().ok()?;
                    Some(TextDocumentEdit {
                        text_document: OptionalVersionedTextDocumentIdentifier {
                            uri: url,
                            version: docs.get(&uri).map(|d| d.version),
                        },
                        edits: edits.into_iter().map(OneOf::Left).collect(),
                    })
                })
                .collect();
            WorkspaceEdit {
                changes: None,
                document_changes: Some(DocumentChanges::Edits(edits)),
                change_annotations: None,
            }
        } else {
            let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
            for (uri, edits) in by_uri {
                if let Ok(url) = uri.parse::<Url>() {
                    changes.entry(url).or_default().extend(edits);
                }
            }
            WorkspaceEdit {
                changes: Some(changes),
                document_changes: None,
                change_annotations: None,
            }
        }
    }

    pub(crate) async fn rename_impl(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri.to_string();
        let pos = params.text_document_position.position;
        let new_name = params.new_name.clone();
        let ws_prefix = self.state.config.read().workspace_prefix.clone();
        let logical_path = logical_path_from_uri(&uri, &ws_prefix);

        let position_encoding = self.position_encoding();
        let source_text = self.file_text_for(&uri).await;
        let source_lines = source_text
            .as_deref()
            .map(|text| DocLines::new(text, position_encoding));
        if let (Some(text), Some(lines)) = (source_text.as_deref(), source_lines.as_ref())
            && let Some((name, _)) = Self::at_var_rename_target(text, lines, pos)
        {
            return self.rename_at_var(&uri, lines, &name, &new_name);
        }

        if let Some(key_lower) = self.loc_key_at_cursor(&uri, pos, &logical_path).await {
            match self.rename_loc(&uri, &key_lower, &new_name).await {
                Ok(Some(edit)) => return Ok(Some(edit)),
                Ok(None) => {}
                Err(e) => return Err(e),
            }
        }

        let type_ref = self.type_ref_at_cursor(&uri, pos, &logical_path);

        let (type_name, instance_name) = match type_ref {
            Some(r) => r,
            None => return Ok(None),
        };

        let mut edits: Vec<(String, u32, u32)> = Vec::new();

        {
            let info = self.state.info_service.read();
            let instances = info.type_index.instances(&type_name);
            for (file_uri, inst) in instances.iter().filter(|(_, i)| i.name == instance_name) {
                edits.push((
                    file_uri.to_string(),
                    inst.location.line.saturating_sub(1),
                    inst.location.col as u32,
                ));
            }
        }

        let sites = super::key_sites(self.collect_use_sites(&type_name, &instance_name));
        let mut text_uris: Vec<String> = edits.iter().map(|(uri, _, _)| uri.clone()).collect();
        text_uris.extend(sites.iter().map(|(uri, _)| uri.clone()));
        let texts = self.file_text_snapshots_for(&text_uris).await;
        let resolved = self.resolve_value_sites(&sites, &instance_name, &texts);
        let unresolved = resolved.iter().filter(|(_, _, _, ok)| !ok).count();
        if unresolved > 0 {
            return Err(tower_lsp::jsonrpc::Error {
                code: tower_lsp::jsonrpc::ErrorCode::ServerError(-32002),
                message: format!(
                    "Rename cancelled: {} reference(s) to '{}' could not be located in text; \
                     rename is limited to indexed references.",
                    unresolved, instance_name
                )
                .into(),
                data: None,
            });
        }
        for (file_uri, line0, col, _) in resolved {
            edits.push((file_uri, line0, col));
        }

        if edits.is_empty() {
            return Ok(None);
        }

        let indexed = index_snapshots(&texts, &self.position_encoding());
        let mut seen: HashSet<(String, u32, u32)> = HashSet::new();
        let mut by_uri: HashMap<String, Vec<TextEdit>> = HashMap::new();
        for (file_uri, line0, col) in edits {
            if !seen.insert((file_uri.clone(), line0, col)) {
                continue;
            }
            let edit = TextEdit {
                range: self.source_range_with_lines(
                    indexed.get(file_uri.as_str()),
                    line0,
                    col,
                    &instance_name,
                ),
                new_text: new_name.clone(),
            };
            by_uri.entry(file_uri).or_default().push(edit);
        }

        let by_uri: Vec<(String, Vec<TextEdit>)> = by_uri.into_iter().collect();
        if let Some(refused) = self.first_refused_edit_target(&by_uri, &uri) {
            return Err(refused);
        }
        Ok(Some(self.build_workspace_edit(by_uri)))
    }
}
