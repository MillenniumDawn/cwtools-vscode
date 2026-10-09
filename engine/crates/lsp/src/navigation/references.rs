use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use cwtools_parser::ast::ParsedFile;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use cwtools_info::PositionElement;

use crate::lines::{DocLines, index_snapshots};
use crate::navigation::helpers::{
    REQUEST_FAILED, TokenCase, code_token_cols_in_line, loc_def_site, word_in_line,
};
use crate::paths::{loc_ref_at_cursor_with_encoding, logical_path_from_uri, parse_uri};
use crate::{Backend, FileTextSnapshot};
use cwtools_info::ReferenceHint;

use super::{BoundedUseSiteScan, scan_use_sites, scan_use_sites_bounded};
use super::{
    dedup_locations, locations_at_with_lines, source_range_without_text, value_col_in_line,
    value_start_after_eq,
};

/// Reduce use sites to the `(uri, key_location)` pairs `resolve_value_sites`
/// takes. Callers that refine the value column from file text — `references`,
/// `rename`, the graph — stay on the key position and the text scan; only the
/// code lens path uses the indexed value position directly (#472).
pub(crate) fn key_sites(
    sites: Vec<cwtools_info::UseSite>,
) -> Vec<(String, cwtools_info::SourceLocation)> {
    sites
        .into_iter()
        .map(|site| (site.file.to_string(), site.key))
        .collect()
}

fn navigation_snapshot_budget_error() -> tower_lsp::jsonrpc::Error {
    tower_lsp::jsonrpc::Error {
        code: tower_lsp::jsonrpc::ErrorCode::ServerError(REQUEST_FAILED),
        message: "Navigation cancelled: total text read budget exceeded.".into(),
        data: None,
    }
}

fn navigation_snapshot_read_error() -> tower_lsp::jsonrpc::Error {
    tower_lsp::jsonrpc::Error {
        code: tower_lsp::jsonrpc::ErrorCode::ServerError(REQUEST_FAILED),
        message: "Navigation cancelled: could not read text snapshots.".into(),
        data: None,
    }
}

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
            // `line` above is already `text.lines().nth(pos.line)`, which is all
            // `lsp_pos_to_source_in_text` and `word_at_position` would each go
            // and find again (#471).
            let byte = crate::paths::position_byte_index(line, pos.character, &encoding);
            let col = line[..byte].chars().count().min(u16::MAX as usize) as u32;
            let word = word_in_line(line, col)?;
            let lower = word.to_lowercase();
            if self.is_known_loc_key(&lower) {
                return Some(lower);
            }
            if let Some(colon) = line.find(':')
                && let Some(word_col) = line.find(&word)
                && word_col < colon
            {
                return Some(lower);
            }
            return None;
        }
        if let Some(info) = self.rule_info_at_cursor(uri, pos, logical_path)
            && let ReferenceHint::LocRef { key } = info.hint
        {
            return Some(key.trim_matches('"').to_lowercase());
        }
        let text = self.file_text_for(uri).await?;
        let encoding = self.state.config.read().position_encoding.clone();
        // One scan for the line, then both the column and the word off it; the
        // pair used to walk the document twice over (#471). A missing line
        // returns None here instead of at `word_at_position`'s own `?`.
        let line = text.lines().nth(pos.line as usize)?;
        let byte = crate::paths::position_byte_index(line, pos.character, &encoding);
        let col = line[..byte].chars().count().min(u16::MAX as usize) as u32;
        let word = word_in_line(line, col)?;
        let lower = word.to_lowercase();
        if self.is_known_loc_key(&lower) {
            Some(lower)
        } else {
            None
        }
    }

    pub(crate) async fn loc_file_uris(&self) -> Vec<String> {
        let mut uris: std::collections::HashSet<String> = std::collections::HashSet::new();
        for uri in self.state.documents.lock().keys() {
            if crate::paths::is_loc_file(uri) {
                uris.insert(uri.clone());
            }
        }
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
                match cwtools_driver::discover_localisation_files(
                    &roots,
                    &ignore_files,
                    &ignore_dirs,
                    cwtools_driver::DiscoveryPolicy::Workspace,
                ) {
                    Ok(discovery) => {
                        for failure in discovery.failures {
                            tracing::warn!(
                                path = %failure.path.display(),
                                error = %failure.error,
                                "localisation discovery skipped path"
                            );
                        }
                        discovery
                            .files
                            .into_iter()
                            .filter(|file| {
                                file.kind == cwtools_file_manager::FileKind::Localisation
                            })
                            .map(|file| file.path)
                            .collect()
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "localisation discovery failed");
                        Vec::new()
                    }
                }
            });
            for path in discovered {
                uris.insert(crate::paths::path_to_uri(&path));
            }
        }
        uris.into_iter().collect()
    }

    /// Where `keys` are defined: closed files from `loc_locations`, open loc
    /// documents from their buffers (the index is not written per keystroke,
    /// and an edit above the entry moves it). Only the files that hold a
    /// definition are read back, for the exact token range, instead of the
    /// whole localisation tree being read and parsed per request (#474).
    pub(crate) async fn collect_loc_definitions(
        &self,
        keys: &HashSet<String>,
        fallback: &Url,
    ) -> Result<Vec<Location>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let mut sites: Vec<(String, u32, String)> = Vec::new();
        let open_loc: Vec<(
            String,
            Arc<str>,
            Option<Arc<crate::state::LocDocumentCache>>,
        )> = {
            let docs = self.state.documents.lock();
            docs.iter()
                .filter(|(uri, _)| crate::paths::is_loc_file(uri))
                .map(|(uri, doc)| {
                    let cache = doc
                        .loc_cache
                        .clone()
                        .filter(|cache| cache.version == doc.version);
                    (uri.clone(), Arc::clone(&doc.text), cache)
                })
                .collect()
        };
        let open_uris: HashSet<&str> = open_loc.iter().map(|(uri, _, _)| uri.as_str()).collect();
        {
            let locations = self.state.loc_locations.read();
            for key in keys {
                for (uri, line0) in locations.workspace_sites(key) {
                    if !open_uris.contains(uri.as_ref()) {
                        sites.push((uri.to_string(), line0, key.clone()));
                    }
                }
            }
        }
        for (uri, text, cache) in &open_loc {
            let parsed;
            let files = match cache {
                Some(cache) => cache.files.as_slice(),
                None => {
                    let path = crate::paths::uri_to_path_str(uri);
                    parsed = cwtools_localization::parse_loc_files(&path, text, None)
                        .unwrap_or_default();
                    parsed.as_slice()
                }
            };
            for entry in files.iter().flat_map(|file| &file.entries) {
                let lower = entry.key.to_lowercase();
                if keys.contains(&lower) {
                    let line0 = (entry.position.line.saturating_sub(1)) as u32;
                    sites.push((uri.clone(), line0, lower));
                }
            }
        }
        if sites.is_empty() {
            return Ok(Vec::new());
        }
        let text_uris: Vec<String> = sites.iter().map(|(uri, _, _)| uri.clone()).collect();
        let texts = self.file_text_snapshots_for_navigation(&text_uris).await?;
        let indexed = index_snapshots(&texts, &self.position_encoding());
        sites.sort();
        let locations = sites
            .into_iter()
            .filter_map(|(uri, line0, key_lower)| {
                let lines = indexed.get(uri.as_str())?;
                let (line0, col) = loc_def_site(lines, line0, &key_lower)?;
                Some(Location {
                    uri: parse_uri(&uri, fallback),
                    range: lines.token_range(line0, col, &key_lower),
                })
            })
            .collect();
        Ok(locations)
    }

    /// Every script file the index or the editor knows.
    pub(crate) fn script_uris(&self) -> Vec<String> {
        let mut script_uris: HashSet<String> = HashSet::new();
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
        script_uris.into_iter().collect()
    }

    /// Runs `scan` over the current text of every `uri` — open buffers as
    /// they are, closed files read one at a time — on the blocking pool, and
    /// returns only what `scan` found. No file's text outlives its own `scan`
    /// call, so the request never holds the workspace's text at once (#474).
    pub(crate) async fn scan_workspace_texts<T, F>(&self, uris: Vec<String>, scan: F) -> Vec<T>
    where
        T: Send + 'static,
        F: Fn(&str, &str) -> Vec<T> + Send + Sync + 'static,
    {
        let mut open: Vec<(String, Arc<str>)> = Vec::new();
        let mut closed: Vec<String> = Vec::new();
        {
            let docs = self.state.documents.lock();
            for uri in uris {
                match docs.text_of(&uri) {
                    Some(text) => open.push((uri, text)),
                    None => closed.push(uri),
                }
            }
        }
        hold_navigation_snapshots_for_tests().await;
        if open.is_empty() && closed.is_empty() {
            return Vec::new();
        }
        let roots = self.state.config.read().authorized_roots.clone();
        tokio::task::spawn_blocking(move || {
            use rayon::prelude::*;
            let mut out: Vec<T> = open
                .par_iter()
                .flat_map_iter(|(uri, text)| scan(uri, text))
                .collect();
            out.par_extend(closed.into_par_iter().flat_map_iter(|uri| {
                crate::access::read_authorized_text(&uri, &roots, crate::access::MAX_URI_READ_BYTES)
                    .map(|text| scan(&uri, &text))
                    .unwrap_or_default()
            }));
            out
        })
        .await
        .unwrap_or_default()
    }

    /// Script-side uses of `keys`, from a streamed scan of every script file.
    pub(crate) async fn collect_loc_script_usages(
        &self,
        keys: &HashSet<String>,
        fallback: &Url,
    ) -> Vec<Location> {
        if keys.is_empty() {
            return Vec::new();
        }
        let script_uris = self.script_uris();
        if script_uris.is_empty() {
            return Vec::new();
        }
        let keys: Vec<String> = keys.iter().cloned().collect();
        let encoding = self.position_encoding();
        let fallback = fallback.clone();
        let mut out = self
            .scan_workspace_texts(script_uris, move |uri, text| {
                let uri = parse_uri(uri, &fallback);
                let lines = DocLines::new(text, encoding.clone());
                // Lines without the key skip the per-line char walk the
                // matcher does; the matcher itself folds ASCII case too.
                let lower = text.to_ascii_lowercase();
                let mut hits = Vec::new();
                for ((line0, line), lower_line) in lines.iter().zip(lower.lines()) {
                    for key_lower in &keys {
                        if !lower_line.contains(key_lower.as_str()) {
                            continue;
                        }
                        for col in
                            code_token_cols_in_line(line, key_lower, TokenCase::AsciiInsensitive)
                        {
                            hits.push(Location {
                                uri: uri.clone(),
                                range: lines.token_range(line0, col, key_lower),
                            });
                        }
                    }
                }
                hits
            })
            .await;
        out.sort_by(|a, b| (a.uri.as_str(), a.range.start).cmp(&(b.uri.as_str(), b.range.start)));
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

        if let Some(key_lower) = self.loc_key_at_cursor(&uri, pos, &logical_path).await {
            let include_declaration = params.context.include_declaration;
            let fallback = &params.text_document_position.text_document.uri;
            let mut keys: HashSet<String> = HashSet::new();
            keys.insert(key_lower.clone());
            let mut all_locs: Vec<Location> = Vec::new();
            if include_declaration {
                all_locs.extend(self.collect_loc_definitions(&keys, fallback).await?);
            }
            all_locs.extend(self.collect_loc_script_usages(&keys, fallback).await);
            let all_locs = dedup_locations(all_locs);
            if !all_locs.is_empty() {
                return Ok(Some(all_locs));
            }
        }

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

            let sites = key_sites(self.collect_use_sites(&type_name, &instance_name));
            let mut text_uris: Vec<String> = definitions
                .iter()
                .map(|(file_uri, _)| file_uri.clone())
                .collect();
            text_uris.extend(sites.iter().map(|(file_uri, _)| file_uri.clone()));
            let texts = self.file_text_snapshots_for_navigation(&text_uris).await?;
            let indexed = index_snapshots(&texts, &self.position_encoding());
            let mut all_locs: Vec<Location> =
                locations_at_with_lines(self, definitions, &instance_name, fallback, &indexed);
            for (file_uri, line0, col, _) in
                self.resolve_value_sites(&sites, &instance_name, &texts)
            {
                all_locs.push(Location {
                    uri: parse_uri(&file_uri, fallback),
                    range: self.source_range_with_lines(
                        indexed.get(file_uri.as_str()),
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
            let texts = self.file_text_snapshots_for_navigation(&text_uris).await?;
            let indexed = index_snapshots(&texts, &self.position_encoding());
            let all_locs = locations_at_with_lines(self, pairs, &symbol, fallback, &indexed);
            if !all_locs.is_empty() {
                return Ok(Some(all_locs));
            }
        }
        Ok(None)
    }

    /// Every use site of `instance_name`: open documents from their in-memory
    /// ASTs, everything else from the reference index.
    ///
    /// The open-document ASTs and the open-URI set are snapshotted under one
    /// `documents` lock and the guard released before anything is walked —
    /// every `did_change` needs that same mutex, and holding `config` across
    /// another lock is what `DocumentState`'s lock order forbids (#472).
    pub(crate) fn collect_use_sites(
        &self,
        type_name: &str,
        instance_name: &str,
    ) -> Vec<cwtools_info::UseSite> {
        let (asts, open_uris): (Vec<(String, Arc<ParsedFile>)>, HashSet<String>) = {
            let docs = self.state.documents.lock();
            (
                docs.iter()
                    .filter_map(|(uri, doc)| doc.ast.clone().map(|ast| (uri.clone(), ast)))
                    .collect(),
                docs.keys().cloned().collect(),
            )
        };
        let ruleset = self.state.rules.read().ruleset.clone();
        let ws_prefix = self.state.config.read().workspace_prefix.clone();
        let (closed_sites, definitions) = {
            let info = self.state.info_service.read();
            let mut closed = info.reference_index.references(type_name, instance_name);
            closed.extend(info.alias_key_index.references_ci(type_name, instance_name));
            let definitions = info
                .type_index
                .instances(type_name)
                .iter()
                .filter(|(_, inst)| inst.name.eq_ignore_ascii_case(instance_name))
                .map(|(file, inst)| (file.to_string(), inst.location))
                .collect::<Vec<_>>();
            (closed, definitions)
        };
        let mut sites: Vec<cwtools_info::UseSite> = Vec::new();
        if let Some(rs) = ruleset {
            sites.extend(scan_use_sites(
                type_name,
                instance_name,
                &asts,
                &rs,
                &ws_prefix,
                &self.state.string_table,
                &definitions,
            ));
        }
        sites.extend(
            closed_sites
                .into_iter()
                .filter(|site| !open_uris.contains(site.file.as_ref())),
        );
        sites
    }

    /// Collect graph use sites without materializing the complete workspace
    /// result. Open documents retain precedence over closed index entries, and
    /// the returned count includes every omitted match for graph diagnostics.
    pub(crate) fn collect_use_sites_bounded(
        &self,
        type_name: &str,
        instance_name: &str,
        limit: usize,
    ) -> (Vec<cwtools_info::UseSite>, usize) {
        let (asts, open_uris): (Vec<(String, Arc<ParsedFile>)>, HashSet<String>) = {
            let docs = self.state.documents.lock();
            (
                docs.iter()
                    .filter_map(|(uri, doc)| doc.ast.clone().map(|ast| (uri.clone(), ast)))
                    .collect(),
                docs.keys().cloned().collect(),
            )
        };
        let ruleset = self.state.rules.read().ruleset.clone();
        let ws_prefix = self.state.config.read().workspace_prefix.clone();
        let definitions = {
            let info = self.state.info_service.read();
            info.type_index
                .instances(type_name)
                .iter()
                .filter(|(_, inst)| inst.name.eq_ignore_ascii_case(instance_name))
                .map(|(file, inst)| (file.to_string(), inst.location))
                .collect::<Vec<_>>()
        };

        let open = ruleset.as_ref().map_or_else(Default::default, |rs| {
            scan_use_sites_bounded(
                BoundedUseSiteScan {
                    type_name,
                    instance_name,
                    docs: &asts,
                    ruleset: rs,
                    workspace_prefix: &ws_prefix,
                    string_table: &self.state.string_table,
                    definitions: &definitions,
                },
                limit,
            )
        });
        let mut sites = open.sites;
        let remaining = limit.saturating_sub(sites.len());
        let (closed, closed_total) = {
            let info = self.state.info_service.read();
            let (mut exact, exact_total) = info.reference_index.references_bounded(
                type_name,
                instance_name,
                remaining,
                &open_uris,
            );
            let remaining = remaining.saturating_sub(exact.len());
            let (alias, alias_total) = info.alias_key_index.references_ci_bounded(
                type_name,
                instance_name,
                remaining,
                &open_uris,
            );
            exact.extend(alias);
            (exact, exact_total + alias_total)
        };
        sites.extend(closed);
        let total = open.total + closed_total;
        (sites, total.saturating_sub(limit))
    }

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
                    if let Some(line) = lines.get(key_line0 as usize)
                        && let Some(from) = value_start_after_eq(line, key_col)
                        && let Some(col) = value_col_in_line(line, name, from)
                    {
                        resolved = Some((key_line0, col));
                    }
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
    ///
    /// `Arc<str>` rather than `String` because the callers are the requests
    /// that fire at cursor-movement and scroll cadence — code actions, inlay
    /// hints, semantic tokens — and copying the buffer for each of them cost a
    /// document-sized allocation per request. The open-doc path is a refcount
    /// bump. The disk path pays one copy wrapping the read (an `Arc` cannot
    /// adopt a `String`'s buffer), which is noise next to the read itself.
    pub(crate) async fn file_text_for(&self, uri: &str) -> Option<Arc<str>> {
        {
            // Scoped so the guard is gone before the blocking read, the same
            // way the copy it replaced was.
            let docs = self.state.documents.lock();
            if let Some(text) = docs.text_of(uri) {
                return Some(text);
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
        .map(Arc::from)
    }

    /// Snapshots navigation text under one cumulative memory budget. Unlike
    /// `file_text_snapshots_for`, this fails the whole request rather than
    /// returning an incomplete set of texts that could produce partial edits.
    pub(crate) async fn file_text_snapshots_for_navigation(
        &self,
        uris: &[String],
    ) -> Result<HashMap<String, FileTextSnapshot>> {
        #[cfg(test)]
        let limit = self
            .state
            .navigation_snapshot_budget_override
            .lock()
            .unwrap_or(crate::access::MAX_NAVIGATION_SNAPSHOT_BYTES);
        #[cfg(not(test))]
        let limit = crate::access::MAX_NAVIGATION_SNAPSHOT_BYTES;
        self.file_text_snapshots_for_navigation_with_limit(uris, limit)
            .await
    }

    async fn file_text_snapshots_for_navigation_with_limit(
        &self,
        uris: &[String],
        limit: usize,
    ) -> Result<HashMap<String, FileTextSnapshot>> {
        let budget = crate::access::ReadBudget::new(limit);
        let mut snapshots = HashMap::new();
        let mut closed = Vec::new();
        let mut seen = HashSet::with_capacity(uris.len());
        {
            let docs = self.state.documents.lock();
            for uri in uris {
                if !seen.insert(uri.as_str()) {
                    continue;
                }
                if let Some(doc) = docs.get(uri) {
                    if !budget.reserve_retained(doc.text.len()) {
                        return Err(navigation_snapshot_budget_error());
                    }
                    let text = doc.text.to_string();
                    snapshots.insert(
                        uri.clone(),
                        FileTextSnapshot {
                            content_hash: cwtools_cache::workspace::content_hash(&text),
                            text,
                            version: Some(doc.version),
                        },
                    );
                } else {
                    closed.push(uri.clone());
                }
            }
        }
        hold_navigation_snapshots_for_tests().await;
        if closed.is_empty() {
            return Ok(snapshots);
        }
        let roots = self.state.config.read().authorized_roots.clone();
        let read = tokio::task::spawn_blocking(move || {
            use rayon::prelude::*;
            closed
                .into_par_iter()
                .map(|uri| {
                    let text = crate::access::read_authorized_text_with_budget(
                        &uri,
                        &roots,
                        crate::access::MAX_URI_READ_BYTES,
                        &budget,
                    )
                    .map_err(|_| navigation_snapshot_budget_error())?;
                    Ok(text.map(|text| {
                        (
                            uri,
                            FileTextSnapshot {
                                content_hash: cwtools_cache::workspace::content_hash(&text),
                                text,
                                version: None,
                            },
                        )
                    }))
                })
                .collect::<Result<Vec<_>>>()
        })
        .await
        .map_err(|_| navigation_snapshot_read_error())??;
        snapshots.extend(read.into_iter().flatten());
        Ok(snapshots)
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

    pub(crate) fn source_range_with_lines(
        &self,
        lines: Option<&DocLines>,
        line: u32,
        column: u32,
        token: &str,
    ) -> Range {
        lines.map_or_else(
            || {
                let encoding = self.position_encoding();
                source_range_without_text(line, column, token, &encoding)
            },
            |lines| lines.token_range(line, column, token),
        )
    }

    pub(crate) fn source_location_with_lines(
        &self,
        uri: &str,
        line: u32,
        column: u32,
        token: &str,
        fallback: &Url,
        lines: Option<&DocLines>,
    ) -> Location {
        Location {
            uri: parse_uri(uri, fallback),
            range: self.source_range_with_lines(lines, line, column, token),
        }
    }
}

/// Pause after open buffers are captured so wire tests can interleave a
/// document notification before closed-file reads finish. Unset in real runs.
async fn hold_navigation_snapshots_for_tests() {
    let Ok(gate) = std::env::var("CWTOOLS_NAV_SNAPSHOT_HOLD_FILE") else {
        return;
    };
    let gate = std::path::PathBuf::from(gate);
    if tokio::fs::try_exists(&gate).await.unwrap_or(false)
        && let Ok(ready) = std::env::var("CWTOOLS_NAV_SNAPSHOT_HOLD_READY_FILE")
    {
        let _ = tokio::fs::write(ready, b"held").await;
    }
    while tokio::fs::try_exists(&gate).await.unwrap_or(false) {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{DocumentState, ParsedDoc};

    const DOC_URI: &str = if cfg!(windows) {
        "file:///C:/ws/common/national_focus/tree.txt"
    } else {
        "file:///ws/common/national_focus/tree.txt"
    };

    /// A `Backend` over a fresh state, with the `Client` captured out of the
    /// service builder — the only way to get one without a live connection.
    fn backend() -> (Backend, Arc<DocumentState>) {
        let state = Arc::new(DocumentState::new());
        let captured = Arc::new(parking_lot::Mutex::new(None));
        let slot = captured.clone();
        let server_state = state.clone();
        let (_service, _socket) = tower_lsp::LspService::new(move |client| {
            *slot.lock() = Some(client.clone());
            Backend {
                client,
                state: server_state.clone(),
            }
        });
        let client = captured.lock().take().expect("the builder ran");
        (
            Backend {
                client,
                state: state.clone(),
            },
            state,
        )
    }

    struct IndexedTypeNavigationFixture {
        backend: Backend,
        definition_uri: String,
        source_text: String,
        source_uri: Url,
        state: Arc<DocumentState>,
        _workspace: tempfile::TempDir,
        use_text: String,
        use_uris: Vec<String>,
    }

    impl IndexedTypeNavigationFixture {
        fn source_position(&self) -> Position {
            let column = self
                .source_text
                .find("my_instance")
                .expect("source contains the type instance") as u32;
            Position::new(0, column + 2)
        }

        fn references_params(&self, include_declaration: bool) -> ReferenceParams {
            ReferenceParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: self.source_uri.clone(),
                    },
                    position: self.source_position(),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: ReferenceContext {
                    include_declaration,
                },
            }
        }

        fn rename_params(&self) -> RenameParams {
            RenameParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: self.source_uri.clone(),
                    },
                    position: self.source_position(),
                },
                new_name: "renamed_instance".to_string(),
                work_done_progress_params: Default::default(),
            }
        }

        fn one_closed_file_budget(&self) -> usize {
            let definition_len = std::fs::metadata(
                Url::parse(&self.definition_uri)
                    .expect("definition URI")
                    .to_file_path()
                    .expect("definition path"),
            )
            .expect("definition metadata")
            .len() as usize;
            let use_len = self.use_text.len();
            self.source_text.len() + (definition_len.max(use_len) + 1) * 5
        }
    }

    fn indexed_type_navigation_fixture(closed_use_count: usize) -> IndexedTypeNavigationFixture {
        use crate::navigation::test_rules::type_ref_ruleset;
        use cwtools_parser::parser::parse_string;

        let (backend, state) = backend();
        let workspace = tempfile::TempDir::new().expect("tmpdir");
        let events = workspace.path().join("events");
        std::fs::create_dir(&events).expect("events directory");
        let uri_of = |path: &std::path::Path| {
            Url::from_file_path(path)
                .expect("absolute path")
                .to_string()
        };
        let definition_path = events.join("definition.txt");
        let definition_uri = uri_of(&definition_path);
        let definition_text =
            "my_type = { id = my_instance kind = alpha active = yes name = title }\n";
        std::fs::write(&definition_path, definition_text).unwrap();
        let use_text = "my_type = { base = my_instance }\n".to_string();
        let use_uris = (0..closed_use_count)
            .map(|idx| {
                let path = events.join(format!("use-{idx}.txt"));
                std::fs::write(&path, &use_text).unwrap();
                uri_of(&path)
            })
            .collect::<Vec<_>>();
        let source_path = events.join("query.txt");
        let source_uri = Url::parse(&uri_of(&source_path)).expect("source URI");
        let source_text = "my_type = { base = my_instance }\n".to_string();
        let source_ast = Arc::new(parse_string(&source_text, &state.string_table));
        let mut rules = type_ref_ruleset();
        {
            let body = rules
                .root_rules
                .iter_mut()
                .find_map(|root| match root {
                    cwtools_rules::rules_types::RootRule::TypeRule(
                        name,
                        (cwtools_rules::rules_types::RuleType::NodeRule { rules, .. }, _),
                    ) if name == "my_type" => Some(rules),
                    _ => None,
                })
                .expect("my_type root rule exists");
            let mut children = body.to_vec();
            children.push((
                cwtools_rules::rules_types::RuleType::LeafRule {
                    left: cwtools_rules::rules_types::NewField::SpecificField("base".to_string()),
                    right: cwtools_rules::rules_types::NewField::TypeField(
                        cwtools_rules::rules_types::TypeType::Simple("my_type".to_string()),
                    ),
                },
                cwtools_rules::rules_types::Options::default(),
            ));
            *body = children.into();
        }
        rules.reindex();
        state.rules.write().ruleset = Some(Arc::new(rules.clone()));
        let root = std::fs::canonicalize(workspace.path()).expect("canonical root");
        let workspace_uri = Url::from_directory_path(workspace.path()).expect("workspace URI");
        {
            let mut config = state.config.write();
            config.authorized_roots = Arc::from([root.clone()]);
            config.editable_roots = Arc::from([root.clone()]);
            config.workspace_roots = vec![root];
            config.workspace_prefix =
                Some(crate::paths::workspace_prefix_of(workspace_uri.as_str()));
        }
        {
            let mut info = state.info_service.write();
            let definition = parse_string(definition_text, &state.string_table);
            info.index_file_with_path(
                &definition_uri,
                &definition,
                &state.string_table,
                &rules,
                "events/definition.txt",
            );
            for (idx, uri) in use_uris.iter().enumerate() {
                let source = parse_string(&use_text, &state.string_table);
                info.index_file_with_path(
                    uri,
                    &source,
                    &state.string_table,
                    &rules,
                    &format!("events/use-{idx}.txt"),
                );
            }
        }
        state
            .documents
            .lock()
            .open(
                source_uri.to_string(),
                ParsedDoc {
                    version: 1,
                    text: Arc::from(source_text.as_str()),
                    ast: Some(source_ast),
                    ast_version: Some(1),
                    ast_source_bytes: source_text.len(),
                    loc_cache: None,
                },
            )
            .expect("source document opens");

        IndexedTypeNavigationFixture {
            backend,
            definition_uri,
            source_text,
            source_uri,
            state,
            _workspace: workspace,
            use_text,
            use_uris,
        }
    }

    #[test]
    fn complete_use_site_collection_stays_complete_alongside_bounded_graph_path() {
        use crate::navigation::test_rules::type_ref_ruleset;
        use cwtools_parser::parser::parse_string;

        let (backend, state) = backend();
        let rules = type_ref_ruleset();
        state.rules.write().ruleset = Some(Arc::new(rules.clone()));
        {
            let mut info = state.info_service.write();
            let definition = parse_string(
                "my_type = { id = my_instance kind = alpha active = yes name = title }\n",
                &state.string_table,
            );
            info.index_file_with_path(
                "file:///definition.txt",
                &definition,
                &state.string_table,
                &rules,
                "events/definition.txt",
            );
            for idx in 0..12 {
                let source = parse_string("foo = { base = my_instance }\n", &state.string_table);
                info.index_file_with_path(
                    &format!("file:///closed-{idx}.txt"),
                    &source,
                    &state.string_table,
                    &rules,
                    "events/caller.txt",
                );
            }
        }

        let open_uri = "file:///open.txt";
        let open_text: Arc<str> = Arc::from("foo = { base = my_instance }\n");
        let open_ast = Arc::new(parse_string(&open_text, &state.string_table));
        state
            .documents
            .lock()
            .open(
                open_uri.to_string(),
                ParsedDoc {
                    version: 1,
                    text: open_text,
                    ast: Some(open_ast),
                    ast_version: Some(1),
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .expect("the open fixture is accepted");

        let complete = backend.collect_use_sites("my_type", "my_instance");
        let (bounded, omitted) = backend.collect_use_sites_bounded("my_type", "my_instance", 3);

        assert_eq!(complete.len(), 13, "references/rename must remain complete");
        assert_eq!(bounded.len(), 3);
        assert_eq!(omitted, 10);
        assert_eq!(bounded[0].file.as_ref(), open_uri);
        assert_eq!(bounded[1].file.as_ref(), "file:///closed-0.txt");
    }

    /// The open-document path must be a refcount bump. `code_action`,
    /// `inlay_hint`, `code_lens` and the semantic-token fast paths all call
    /// this on a cursor move, so a copy here is a document-sized allocation
    /// per request; pointer identity is the proof there isn't one (#473).
    #[tokio::test]
    async fn file_text_for_open_doc_shares_the_buffer() {
        let (backend, state) = backend();
        let stored: Arc<str> = Arc::from("focus_tree = {\n\tid = tree\n}\n");
        state
            .documents
            .lock()
            .open(
                DOC_URI.to_string(),
                ParsedDoc {
                    version: 1,
                    text: stored.clone(),
                    ast: None,
                    ast_version: None,
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .expect("the store accepts one small doc");

        // A cursor-move burst: every request must hand back the same buffer.
        for _ in 0..32 {
            let text = backend
                .file_text_for(DOC_URI)
                .await
                .expect("the doc is open");
            assert!(
                Arc::ptr_eq(&stored, &text),
                "file_text_for copied the open buffer instead of sharing it"
            );
        }

        // The store's handle and ours, and nothing left over from the burst.
        assert_eq!(Arc::strong_count(&stored), 2);
    }

    /// The closed-file path still reads through the access boundary, and a
    /// path outside every authorized root still reads as absent.
    #[tokio::test]
    async fn file_text_for_reads_a_closed_file_under_an_authorized_root() {
        let (backend, state) = backend();
        let ws = tempfile::TempDir::new().expect("tmpdir");
        let file = ws.path().join("a.txt");
        std::fs::write(&file, "foo = { }\n").unwrap();
        let uri = Url::from_file_path(&file)
            .expect("absolute path")
            .to_string();
        let outside = tempfile::TempDir::new().expect("tmpdir");
        let stray = outside.path().join("b.txt");
        std::fs::write(&stray, "bar = { }\n").unwrap();
        let stray_uri = Url::from_file_path(&stray)
            .expect("absolute path")
            .to_string();

        state.config.write().authorized_roots =
            Arc::from([std::fs::canonicalize(ws.path()).expect("canonical root")]);

        assert_eq!(
            backend.file_text_for(&uri).await.as_deref(),
            Some("foo = { }\n")
        );
        assert_eq!(backend.file_text_for(&stray_uri).await, None);
    }

    /// The streamed scan sees an open buffer as the editor has it, a closed
    /// file as it is on disk, and nothing outside the access boundary; each
    /// hit names the file it came from (#474).
    #[tokio::test]
    async fn scan_workspace_texts_covers_open_and_closed_files() {
        let (backend, state) = backend();
        let ws = tempfile::TempDir::new().expect("tmpdir");
        let on_disk = ws.path().join("closed.txt");
        std::fs::write(&on_disk, "closed = needle\n").unwrap();
        let open_path = ws.path().join("open.txt");
        std::fs::write(&open_path, "stale = needle\n").unwrap();
        let outside = tempfile::TempDir::new().expect("tmpdir");
        let stray = outside.path().join("stray.txt");
        std::fs::write(&stray, "stray = needle\n").unwrap();
        let uri_of = |path: &std::path::Path| {
            Url::from_file_path(path)
                .expect("absolute path")
                .to_string()
        };
        state.config.write().authorized_roots =
            Arc::from([std::fs::canonicalize(ws.path()).expect("canonical root")]);
        state
            .documents
            .lock()
            .open(
                uri_of(&open_path),
                ParsedDoc {
                    version: 2,
                    text: Arc::from("edited = needle\n"),
                    ast: None,
                    ast_version: None,
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .expect("the store accepts one small doc");

        let mut hits = backend
            .scan_workspace_texts(
                vec![uri_of(&on_disk), uri_of(&open_path), uri_of(&stray)],
                |uri, text| {
                    text.lines()
                        .filter(|line| line.contains("needle"))
                        .map(|line| format!("{}:{}", uri.rsplit('/').next().unwrap(), line))
                        .collect()
                },
            )
            .await;
        hits.sort();
        assert_eq!(
            hits,
            vec![
                "closed.txt:closed = needle".to_string(),
                "open.txt:edited = needle".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn references_impl_returns_complete_results_or_budget_error() {
        let fixture = indexed_type_navigation_fixture(20);
        let logical_path = crate::paths::logical_path_from_uri(
            fixture.source_uri.as_str(),
            &fixture.state.config.read().workspace_prefix,
        );
        assert!(
            fixture
                .backend
                .resolve_at_cursor(
                    fixture.source_uri.as_str(),
                    fixture.source_position(),
                    &logical_path,
                )
                .is_some(),
            "cursor resolution must find the indexed open document at {logical_path}"
        );
        let hint = fixture
            .backend
            .rule_info_at_cursor(
                fixture.source_uri.as_str(),
                fixture.source_position(),
                &logical_path,
            )
            .expect("cursor resolves to a rule context")
            .hint;
        assert!(
            matches!(hint, ReferenceHint::TypeRef { .. }),
            "source must produce a type-reference hint, got {hint:?}"
        );
        let params = fixture.references_params(false);
        let complete = fixture
            .backend
            .references_impl(params.clone())
            .await
            .expect("under-cap references succeed")
            .expect("the indexed type reference resolves");
        let mut expected_uris = fixture.use_uris.iter().cloned().collect::<HashSet<_>>();
        expected_uris.insert(fixture.source_uri.to_string());
        assert_eq!(complete.len(), expected_uris.len());
        assert_eq!(
            complete
                .iter()
                .map(|location| location.uri.to_string())
                .collect::<HashSet<_>>(),
            expected_uris,
            "the result contains the open reference and every indexed closed reference"
        );

        let limit = fixture.one_closed_file_budget();
        *fixture.state.navigation_snapshot_budget_override.lock() = Some(limit);
        let one_site = fixture
            .backend
            .file_text_snapshots_for_navigation(&[
                fixture.source_uri.to_string(),
                fixture.use_uris[0].clone(),
            ])
            .await
            .expect("one closed indexed reference fits after the open buffer");
        assert_eq!(one_site.len(), 2);

        let error = fixture
            .backend
            .references_impl(params)
            .await
            .expect_err("multiple valid indexed references exceed the cumulative budget");
        assert_eq!(
            error.code,
            tower_lsp::jsonrpc::ErrorCode::ServerError(-32803)
        );
    }

    #[tokio::test]
    async fn rename_impl_returns_complete_edits_or_budget_error() {
        let fixture = indexed_type_navigation_fixture(20);
        let edit = fixture
            .backend
            .rename_impl(fixture.rename_params())
            .await
            .expect("under-cap rename succeeds")
            .expect("the indexed type instance is renameable");
        let changes = edit.changes.expect("legacy workspace edits are enabled");
        let mut expected_uris = fixture.use_uris.iter().cloned().collect::<HashSet<_>>();
        expected_uris.insert(fixture.source_uri.to_string());
        expected_uris.insert(fixture.definition_uri.clone());
        assert_eq!(changes.len(), expected_uris.len());
        assert_eq!(
            changes.keys().map(Url::to_string).collect::<HashSet<_>>(),
            expected_uris,
            "the rename includes the definition and every indexed use"
        );
        assert_eq!(
            changes.values().map(Vec::len).sum::<usize>(),
            expected_uris.len()
        );
        assert!(
            changes
                .values()
                .flatten()
                .all(|edit| edit.new_text == "renamed_instance")
        );

        let limit = fixture.one_closed_file_budget();
        *fixture.state.navigation_snapshot_budget_override.lock() = Some(limit);
        for uri in [&fixture.definition_uri, &fixture.use_uris[0]] {
            let one_site = fixture
                .backend
                .file_text_snapshots_for_navigation(&[fixture.source_uri.to_string(), uri.clone()])
                .await
                .expect("an individual indexed rename target fits");
            assert_eq!(one_site.len(), 2);
        }
        let error = fixture
            .backend
            .rename_impl(fixture.rename_params())
            .await
            .expect_err("rename refuses exhaustion rather than returning a partial edit");
        assert_eq!(
            error.code,
            tower_lsp::jsonrpc::ErrorCode::ServerError(-32803)
        );
    }

    #[tokio::test]
    async fn rename_impl_refuses_unresolvable_references_with_request_failed() {
        let fixture = indexed_type_navigation_fixture(1);
        let use_path = Url::parse(&fixture.use_uris[0])
            .expect("use URI")
            .to_file_path()
            .expect("use path");
        std::fs::remove_file(&use_path).expect("the closed use file is removed");
        let error = fixture
            .backend
            .rename_impl(fixture.rename_params())
            .await
            .expect_err("a rename whose references are unresolvable in text is refused");
        assert_eq!(
            error.code,
            tower_lsp::jsonrpc::ErrorCode::ServerError(-32803)
        );
        assert!(
            error
                .message
                .contains("reference(s) to 'my_instance' could not be located in text"),
            "got {:?}",
            error.message
        );
    }

    #[tokio::test]
    async fn rename_impl_refuses_edits_outside_the_editable_roots_with_request_failed() {
        let fixture = indexed_type_navigation_fixture(1);
        fixture.state.config.write().editable_roots = Arc::from([]);
        let error = fixture
            .backend
            .rename_impl(fixture.rename_params())
            .await
            .expect_err("a rename touching files outside the editable roots is refused");
        assert_eq!(
            error.code,
            tower_lsp::jsonrpc::ErrorCode::ServerError(-32803)
        );
        assert!(
            error
                .message
                .contains("cwtools only edits files in the workspace folders"),
            "got {:?}",
            error.message
        );
    }

    #[test]
    fn navigation_snapshot_read_error_is_request_failed() {
        assert_eq!(
            navigation_snapshot_read_error().code,
            tower_lsp::jsonrpc::ErrorCode::ServerError(-32803)
        );
    }

    #[tokio::test]
    async fn navigation_snapshots_deduplicate_and_allow_exact_budget_boundary() {
        let (backend, state) = backend();
        let ws = tempfile::TempDir::new().expect("tmpdir");
        let open_path = ws.path().join("open.txt");
        let second_open_path = ws.path().join("second-open.txt");
        let closed_path = ws.path().join("closed.txt");
        std::fs::write(&closed_path, "c").unwrap();
        let uri_of = |path: &std::path::Path| {
            Url::from_file_path(path)
                .expect("absolute path")
                .to_string()
        };
        let open_uri = uri_of(&open_path);
        let second_open_uri = uri_of(&second_open_path);
        let closed_uri = uri_of(&closed_path);
        state.config.write().authorized_roots =
            Arc::from([std::fs::canonicalize(ws.path()).expect("canonical root")]);
        state
            .documents
            .lock()
            .open(
                open_uri.clone(),
                ParsedDoc {
                    version: 1,
                    text: Arc::from("open"),
                    ast: None,
                    ast_version: None,
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .expect("the store accepts one small doc");
        state
            .documents
            .lock()
            .open(
                second_open_uri.clone(),
                ParsedDoc {
                    version: 1,
                    text: Arc::from("more"),
                    ast: None,
                    ast_version: None,
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .expect("the store accepts another small doc");

        let uris = vec![
            open_uri.clone(),
            open_uri.clone(),
            closed_uri.clone(),
            closed_uri.clone(),
        ];
        let snapshots = backend
            .file_text_snapshots_for_navigation_with_limit(&uris, 14)
            .await
            .expect("4 open bytes + 10 bytes peak closed-read allowance");
        assert_eq!(snapshots.len(), 2, "duplicate URIs get one snapshot");
        assert_eq!(snapshots[&open_uri].text, "open");
        assert_eq!(snapshots[&closed_uri].text, "c");

        let error = backend
            .file_text_snapshots_for_navigation_with_limit(&uris, 13)
            .await
            .err()
            .expect("one byte below the required peak must refuse the request");
        assert!(error.message.contains("total text read budget exceeded"));

        let open_uris = vec![
            open_uri.clone(),
            open_uri.clone(),
            second_open_uri.clone(),
            second_open_uri.clone(),
        ];
        let open_snapshots = backend
            .file_text_snapshots_for_navigation_with_limit(&open_uris, 8)
            .await
            .expect("the two unique four-byte open buffers fit exactly");
        assert_eq!(open_snapshots.len(), 2);
        assert!(
            backend
                .file_text_snapshots_for_navigation_with_limit(&open_uris, 7)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn navigation_snapshot_budget_covers_many_closed_files() {
        let (backend, state) = backend();
        let ws = tempfile::TempDir::new().expect("tmpdir");
        state.config.write().authorized_roots =
            Arc::from([std::fs::canonicalize(ws.path()).expect("canonical root")]);
        let uris = (0..10)
            .map(|idx| {
                let path = ws.path().join(format!("{idx}.txt"));
                std::fs::write(&path, "0123456789").unwrap();
                Url::from_file_path(path)
                    .expect("absolute path")
                    .to_string()
            })
            .collect::<Vec<_>>();

        let single = backend
            .file_text_snapshots_for_navigation_with_limit(&uris[..1], 55)
            .await
            .expect("one ten-byte file fits its 55-byte peak reservation");
        assert_eq!(single.len(), 1);

        let snapshots = backend
            .file_text_snapshots_for_navigation_with_limit(&uris, 550)
            .await
            .expect("all ten peak reservations fit");
        assert_eq!(snapshots.len(), 10);
        assert!(
            backend
                .file_text_snapshots_for_navigation_with_limit(&uris, 55)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn navigation_snapshot_budget_does_not_refuse_overlapping_reads_that_fit_in_turn() {
        let (backend, state) = backend();
        let ws = tempfile::TempDir::new().expect("tmpdir");
        state.config.write().authorized_roots =
            Arc::from([std::fs::canonicalize(ws.path()).expect("canonical root")]);
        let file_bytes = 64 * 1024;
        let count = rayon::current_num_threads() * 4;
        let uris = (0..count)
            .map(|idx| {
                let path = ws.path().join(format!("{idx}.txt"));
                std::fs::write(&path, "x".repeat(file_bytes)).unwrap();
                Url::from_file_path(path)
                    .expect("absolute path")
                    .to_string()
            })
            .collect::<Vec<_>>();
        // Read one at a time, the last file's peak lands on the rest retained.
        let retained = file_bytes + 1;
        let limit = (count - 1) * retained + 5 * retained;

        let snapshots = backend
            .file_text_snapshots_for_navigation_with_limit(&uris, limit)
            .await
            .expect("more files than rayon threads still fit when read in turn");
        assert_eq!(snapshots.len(), count);
    }
}
