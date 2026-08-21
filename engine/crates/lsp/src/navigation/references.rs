use std::collections::{HashMap, HashSet};

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use cwtools_info::PositionElement;

use crate::navigation::helpers::{code_token_cols_in_line_ignore_case, word_at_position};
use crate::paths::{
    loc_ref_at_cursor_with_encoding, logical_path_from_uri, lsp_pos_to_source_in_text, parse_uri,
};
use crate::{Backend, FileTextSnapshot};
use cwtools_info::ReferenceHint;

use super::scan_use_sites;
use super::{
    dedup_locations, locations_at_with_texts, source_range_in_text, source_range_without_text,
    value_col_in_line, value_start_after_eq,
};

impl Backend {
    pub(crate) fn is_known_loc_key(&self, lower: &str) -> bool {
        if let Some(idx) = self.state.loc_index.read().as_deref()
            && idx.union().contains(lower)
        {
            return true;
        }
        for set in self.state.loc_live_overlay.read().values() {
            if set.contains(lower) {
                return true;
            }
        }
        for set in self.state.loc_watched_overlay.read().values() {
            if set.contains(lower) {
                return true;
            }
        }
        false
    }

    pub(crate) async fn loc_key_at_cursor(
        &self,
        uri: &str,
        pos: Position,
        logical_path: &str,
    ) -> Option<String> {
        if crate::paths::is_loc_file(uri) {
            let text = self.file_text_for(uri).await?;
            let encoding = self.state.config.read().position_encoding.clone();
            let line = text.lines().nth(pos.line as usize).unwrap_or("");
            if let Some((key, _, _)) =
                loc_ref_at_cursor_with_encoding(line, pos.character, &encoding)
            {
                return Some(key.to_lowercase());
            }
            let (_, col) = lsp_pos_to_source_in_text(&text, pos, &encoding);
            let word = word_at_position(&text, pos.line, col as u32)?;
            let lower = word.to_lowercase();
            if self.is_known_loc_key(&lower) {
                return Some(lower);
            }
            // Fallback for unsaved keys: only treat word before `:` as key.
            if let Some(colon) = line.find(':')
                && let Some(word_col) = line.find(&word)
                && word_col < colon
            {
                return Some(lower);
            }
            return None;
        }
        // Script file: prefer rule-classified LocRef
        if let Some(info) = self.rule_info_at_cursor(uri, pos, logical_path)
            && let ReferenceHint::LocRef { key } = info.hint
        {
            return Some(key.trim_matches('"').to_lowercase());
        }
        let text = self.file_text_for(uri).await?;
        let encoding = self.state.config.read().position_encoding.clone();
        let (_, col) = lsp_pos_to_source_in_text(&text, pos, &encoding);
        let word = word_at_position(&text, pos.line, col as u32)?;
        let lower = word.to_lowercase();
        if self.is_known_loc_key(&lower) {
            Some(lower)
        } else {
            None
        }
    }

    pub(crate) async fn loc_file_uris(&self) -> Vec<String> {
        let mut uris: std::collections::HashSet<String> = std::collections::HashSet::new();
        // Open loc docs
        for uri in self.state.documents.lock().keys() {
            if crate::paths::is_loc_file(uri) {
                uris.insert(uri.clone());
            }
        }
        // Discovered loc files under workspace roots
        let (roots, ignore_files, ignore_dirs): (
            Vec<std::path::PathBuf>,
            Vec<String>,
            Vec<String>,
        ) = {
            let cfg = self.state.config.read();
            let roots = if !cfg.workspace_roots.is_empty() {
                cfg.workspace_roots.clone()
            } else if let Some(ws) = &cfg.workspace_uri {
                if let Ok(url) = Url::parse(ws)
                    && let Ok(p) = url.to_file_path()
                {
                    vec![p]
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            (
                roots,
                cfg.ignore_file_patterns.clone(),
                cfg.ignore_dir_patterns.clone(),
            )
        };
        if !roots.is_empty() {
            let discovered = tokio::task::block_in_place(|| {
                let refs: Vec<&std::path::Path> = roots.iter().map(|p| p.as_path()).collect();
                cwtools_localization::LocService::discover_files_filtered(
                    &refs,
                    cwtools_file_manager::file_manager::ScanBudget::default(),
                    &ignore_files,
                    &ignore_dirs,
                )
            });
            for path in discovered {
                uris.insert(crate::paths::path_to_uri(&path));
            }
        }
        // Vanilla not under workspace roots; discovered set is complete.
        uris.into_iter().collect()
    }

    pub(crate) async fn collect_loc_definitions(
        &self,
        keys: &std::collections::HashSet<String>,
        fallback: &Url,
    ) -> Vec<Location> {
        if keys.is_empty() {
            return Vec::new();
        }
        let loc_uris = self.loc_file_uris().await;
        if loc_uris.is_empty() {
            return Vec::new();
        }
        let texts = self.file_text_snapshots_for(&loc_uris).await;
        let encoding = self.state.config.read().position_encoding.clone();
        let mut out = Vec::new();
        for uri in loc_uris {
            let Some(snapshot) = texts.get(&uri) else {
                continue;
            };
            let text = &snapshot.text;
            let path = crate::paths::uri_to_path_str(&uri);
            let files =
                cwtools_localization::parse_loc_files(&path, text, None).unwrap_or_default();
            for file in files {
                for entry in file.entries {
                    let lower = entry.key.to_lowercase();
                    if !keys.contains(&lower) {
                        continue;
                    }
                    let line0 = (entry.position.line.saturating_sub(1)) as u32;
                    let line_text = text.lines().nth(line0 as usize).unwrap_or("");
                    let col = line_text
                        .find(&entry.key)
                        .map(|b| line_text[..b].chars().count() as u32)
                        .unwrap_or(0);
                    let fallback_url = Url::parse(&uri).unwrap_or_else(|_| fallback.clone());
                    out.push(Location {
                        uri: fallback_url,
                        range: crate::navigation::helpers::source_range_in_text(
                            text, line0, col, &entry.key, &encoding,
                        ),
                    });
                }
            }
        }
        out
    }

    pub(crate) async fn collect_loc_script_usages(
        &self,
        keys: &std::collections::HashSet<String>,
        fallback: &Url,
    ) -> Vec<Location> {
        if keys.is_empty() {
            return Vec::new();
        }
        // Gather candidate script files: indexed files + open docs
        let mut script_uris: std::collections::HashSet<String> = std::collections::HashSet::new();
        {
            let info = self.state.info_service.read();
            for uri in info.files.keys() {
                if crate::paths::is_script_file(uri) {
                    script_uris.insert(uri.clone());
                }
            }
        }
        for uri in self.state.documents.lock().keys() {
            if crate::paths::is_script_file(uri) {
                script_uris.insert(uri.clone());
            }
        }
        if script_uris.is_empty() {
            return Vec::new();
        }
        let script_uris: Vec<String> = script_uris.into_iter().collect();
        let texts = self.file_text_snapshots_for(&script_uris).await;
        let encoding = self.state.config.read().position_encoding.clone();
        let mut out = Vec::new();
        for uri in script_uris {
            let Some(snapshot) = texts.get(&uri) else {
                continue;
            };
            let text = &snapshot.text;
            let fallback_url = Url::parse(&uri).unwrap_or_else(|_| fallback.clone());
            for (line0, line) in text.lines().enumerate() {
                for key_lower in keys.iter() {
                    for col in code_token_cols_in_line_ignore_case(line, key_lower) {
                        out.push(Location {
                            uri: fallback_url.clone(),
                            range: crate::navigation::helpers::source_range_in_text(
                                text,
                                line0 as u32,
                                col,
                                key_lower,
                                &encoding,
                            ),
                        });
                    }
                }
            }
        }
        out
    }

    pub(crate) async fn references_impl(
        &self,
        params: ReferenceParams,
    ) -> Result<Option<Vec<Location>>> {
        let pos = params.text_document_position.position;
        let uri = params.text_document_position.text_document.uri.to_string();

        let ws_prefix = self.state.config.read().workspace_prefix.clone();
        let logical_path = logical_path_from_uri(&uri, &ws_prefix);

        // Loc key: find definitions in .yml plus script usages.
        if let Some(key_lower) = self.loc_key_at_cursor(&uri, pos, &logical_path).await {
            let include_declaration = params.context.include_declaration;
            let fallback = &params.text_document_position.text_document.uri;
            let mut keys: HashSet<String> = HashSet::new();
            keys.insert(key_lower.clone());
            let mut all_locs: Vec<Location> = Vec::new();
            if include_declaration {
                all_locs.extend(self.collect_loc_definitions(&keys, fallback).await);
            }
            all_locs.extend(self.collect_loc_script_usages(&keys, fallback).await);
            let all_locs = dedup_locations(all_locs);
            if !all_locs.is_empty() {
                return Ok(Some(all_locs));
            }
        }

        // Rule-aware: identify a TypeRef at cursor, then gather every location
        // where that instance is defined or used. Definitions come from the
        // TypeIndex; use sites from the live AST of open docs plus the workspace
        // reverse index for closed files. Use-site columns are resolved from
        // text (the parser records the leaf key, not the value, precisely).
        let type_ref = self.type_ref_at_cursor(&uri, pos, &logical_path);

        let include_declaration = params.context.include_declaration;

        if let Some((type_name, instance_name)) = type_ref {
            let fallback = &params.text_document_position.text_document.uri;
            let definitions = if include_declaration {
                let info = self.state.info_service.read();
                info.type_index
                    .instances(&type_name)
                    .iter()
                    .filter(|(_, inst)| inst.name == instance_name)
                    .map(|(file_uri, inst)| (file_uri.to_string(), inst.location))
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };

            // 2. Use-sites (open docs via live AST + closed files via index).
            let sites = self.collect_use_sites(&type_name, &instance_name);
            let mut text_uris: Vec<String> = definitions
                .iter()
                .map(|(file_uri, _)| file_uri.clone())
                .collect();
            text_uris.extend(sites.iter().map(|(file_uri, _)| file_uri.clone()));
            let texts = self.file_text_snapshots_for(&text_uris).await;
            let mut all_locs: Vec<Location> =
                locations_at_with_texts(self, definitions, &instance_name, fallback, &texts);
            for (file_uri, line0, col, _) in
                self.resolve_value_sites(&sites, &instance_name, &texts)
            {
                all_locs.push(Location {
                    uri: parse_uri(&file_uri, fallback),
                    range: self.source_range_with_text(
                        texts.get(&file_uri).map(|snapshot| snapshot.text.as_str()),
                        line0,
                        col,
                        &instance_name,
                    ),
                });
            }

            let all_locs = dedup_locations(all_locs);
            if !all_locs.is_empty() {
                return Ok(Some(all_locs));
            }
        }

        // Fallback: heuristic-based approach
        if let Some(element) = self.element_at_cursor(&uri, pos) {
            let symbol = match &element {
                PositionElement::Leaf { key, .. } => key.clone(),
                PositionElement::LeafValue { value } => value.clone(),
            };
            let fallback = &params.text_document_position.text_document.uri;
            let (definitions, references) = {
                let info = self.state.info_service.read();
                (
                    if include_declaration {
                        info.find_definitions(&symbol).cloned().unwrap_or_default()
                    } else {
                        Vec::new()
                    },
                    info.find_references(&symbol).unwrap_or_default(),
                )
            };
            let mut pairs = definitions;
            pairs.extend(references);
            let text_uris: Vec<String> =
                pairs.iter().map(|(file_uri, _)| file_uri.clone()).collect();
            let texts = self.file_text_snapshots_for(&text_uris).await;
            let all_locs = locations_at_with_texts(self, pairs, &symbol, fallback, &texts);
            if !all_locs.is_empty() {
                return Ok(Some(all_locs));
            }
        }
        Ok(None)
    }

    /// Gather all use sites `(file_uri, key location)` of `instance_name` as a
    /// `type_name` reference: open docs from their live AST, closed files from
    /// the workspace reverse index. Open docs are taken only from the live scan
    /// (their index entry can lag a keystroke), so the reverse-index half skips
    /// them.
    pub(crate) fn collect_use_sites(
        &self,
        type_name: &str,
        instance_name: &str,
    ) -> Vec<(String, cwtools_info::SourceLocation)> {
        let mut sites: Vec<(String, cwtools_info::SourceLocation)> = Vec::new();
        let open_uris: HashSet<String> = {
            let docs = self.state.documents.lock();
            let rules_guard = self.state.rules.read();
            let ws_prefix = self.state.config.read().workspace_prefix.clone();
            if let Some(rs) = rules_guard.ruleset.as_ref() {
                sites.extend(scan_use_sites(
                    type_name,
                    instance_name,
                    &docs,
                    rs,
                    &ws_prefix,
                    &self.state.string_table,
                ));
            }
            docs.keys().cloned().collect()
        };
        {
            let info = self.state.info_service.read();
            for (file_uri, loc) in info.reference_index.references(type_name, instance_name) {
                if !open_uris.contains(file_uri.as_ref()) {
                    sites.push((file_uri.to_string(), loc));
                }
            }
        }
        sites
    }

    /// Resolve each `(file_uri, key_loc)` use site to `(file_uri, value_line0,
    /// value_col, resolved)`. Reads each file once (open-doc text or disk) and
    /// locates `name` as a whole token on the key line (falling back to the next
    /// line). When the value can't be located, `resolved` is false and the key
    /// position is returned unchanged.
    pub(crate) fn resolve_value_sites(
        &self,
        sites: &[(String, cwtools_info::SourceLocation)],
        name: &str,
        texts: &HashMap<String, FileTextSnapshot>,
    ) -> Vec<(String, u32, u32, bool)> {
        let mut by_file: HashMap<&str, Vec<cwtools_info::SourceLocation>> = HashMap::new();
        for (uri, loc) in sites {
            by_file.entry(uri.as_str()).or_default().push(*loc);
        }
        let mut out = Vec::new();
        for (uri, locs) in by_file {
            let lines: Option<Vec<&str>> = texts
                .get(uri)
                .map(|snapshot| snapshot.text.lines().collect());
            for loc in locs {
                let key_line0 = loc.line.saturating_sub(1);
                let key_col = loc.col as u32;
                let mut resolved = None;
                if let Some(lines) = &lines {
                    // Value on the key line, after the `=` that follows the key.
                    if let Some(line) = lines.get(key_line0 as usize)
                        && let Some(from) = value_start_after_eq(line, key_col)
                        && let Some(col) = value_col_in_line(line, name, from)
                    {
                        resolved = Some((key_line0, col));
                    }
                    // Fallback: `key =` with the value on the next line.
                    if resolved.is_none()
                        && let Some(line) = lines.get(key_line0 as usize + 1)
                        && let Some(col) = value_col_in_line(line, name, 0)
                    {
                        resolved = Some((key_line0 + 1, col));
                    }
                }
                match resolved {
                    Some((line0, col)) => out.push((uri.to_string(), line0, col, true)),
                    None => out.push((uri.to_string(), key_line0, key_col, false)),
                }
            }
        }
        out
    }

    /// The current text of `uri`: the open-doc buffer if open, else read from
    /// disk through the access boundary on Tokio's blocking pool.
    pub(crate) async fn file_text_for(&self, uri: &str) -> Option<String> {
        {
            let docs = self.state.documents.lock();
            if let Some(doc) = docs.get(uri) {
                return Some(doc.text.to_string());
            }
        }
        let roots = self.state.config.read().authorized_roots.clone();
        let uri = uri.to_string();
        tokio::task::spawn_blocking(move || {
            crate::access::read_authorized_text(&uri, &roots, crate::access::MAX_URI_READ_BYTES)
        })
        .await
        .ok()
        .flatten()
    }

    pub(crate) async fn file_text_snapshots_for(
        &self,
        uris: &[String],
    ) -> HashMap<String, FileTextSnapshot> {
        let mut snapshots = HashMap::new();
        let mut closed = Vec::new();
        let mut seen_closed = HashSet::new();
        {
            let docs = self.state.documents.lock();
            for uri in uris {
                if let Some(doc) = docs.get(uri) {
                    let text = doc.text.to_string();
                    snapshots.insert(
                        uri.clone(),
                        FileTextSnapshot {
                            content_hash: cwtools_cache::workspace::content_hash(&text),
                            text,
                            version: Some(doc.version),
                        },
                    );
                } else if seen_closed.insert(uri.clone()) {
                    closed.push(uri.clone());
                }
            }
        }
        if closed.is_empty() {
            return snapshots;
        }
        let roots = self.state.config.read().authorized_roots.clone();
        // Parallel because rename and references batch one read per file naming
        // the symbol, which is hundreds of files for a widely-used one.
        use rayon::prelude::*;
        if let Ok(read) = tokio::task::spawn_blocking(move || {
            closed
                .into_par_iter()
                .filter_map(|uri| {
                    let text = crate::access::read_authorized_text(
                        &uri,
                        &roots,
                        crate::access::MAX_URI_READ_BYTES,
                    )?;
                    Some((
                        uri,
                        FileTextSnapshot {
                            content_hash: cwtools_cache::workspace::content_hash(&text),
                            text,
                            version: None,
                        },
                    ))
                })
                .collect::<HashMap<_, _>>()
        })
        .await
        {
            snapshots.extend(read);
        }
        snapshots
    }

    pub(crate) fn source_range_with_text(
        &self,
        text: Option<&str>,
        line: u32,
        column: u32,
        token: &str,
    ) -> Range {
        let encoding = self.state.config.read().position_encoding.clone();
        text.map_or_else(
            || source_range_without_text(line, column, token, &encoding),
            |text| source_range_in_text(text, line, column, token, &encoding),
        )
    }

    pub(crate) fn source_location_with_text(
        &self,
        uri: &str,
        line: u32,
        column: u32,
        token: &str,
        fallback: &Url,
        text: Option<&str>,
    ) -> Location {
        Location {
            uri: parse_uri(uri, fallback),
            range: self.source_range_with_text(text, line, column, token),
        }
    }
}
