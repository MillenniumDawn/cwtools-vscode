use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use cwtools_parser::ast::ParsedFile;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use cwtools_info::PositionElement;

use crate::lines::{DocLines, index_snapshots};
use crate::navigation::helpers::{code_token_cols_in_line_ignore_case, word_in_line};
use crate::paths::{loc_ref_at_cursor_with_encoding, logical_path_from_uri, parse_uri};
use crate::{Backend, FileTextSnapshot};
use cwtools_info::ReferenceHint;

use super::scan_use_sites;
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
            let lines = DocLines::new(text, encoding.clone());
            for file in files {
                for entry in file.entries {
                    let lower = entry.key.to_lowercase();
                    if !keys.contains(&lower) {
                        continue;
                    }
                    let line0 = (entry.position.line.saturating_sub(1)) as u32;
                    let line_text = lines.line(line0);
                    let col = line_text
                        .find(&entry.key)
                        .map(|b| line_text[..b].chars().count() as u32)
                        .unwrap_or(0);
                    let fallback_url = Url::parse(&uri).unwrap_or_else(|_| fallback.clone());
                    out.push(Location {
                        uri: fallback_url,
                        range: lines.token_range(line0, col, &entry.key),
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
            let lines = DocLines::new(text, encoding.clone());
            // Over the index rather than a second `text.lines()` pass over
            // the same text, once per workspace file (#471).
            for (line0, line) in lines.iter() {
                for key_lower in keys.iter() {
                    for col in code_token_cols_in_line_ignore_case(line, key_lower) {
                        out.push(Location {
                            uri: fallback_url.clone(),
                            range: lines.token_range(line0, col, key_lower),
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
            let texts = self.file_text_snapshots_for(&text_uris).await;
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
            let texts = self.file_text_snapshots_for(&text_uris).await;
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
}
