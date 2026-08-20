use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::Backend;
use crate::paths::{logical_path_from_uri, lsp_pos_to_source_in_text, source_position_to_lsp};

use super::{
    brace_folding_ranges, brace_pairs, build_doc_symbols, code_token_cols_in_line,
    comment_and_region_folds, highlight_kind, make_symbol, selection_spans, source_range_in_text,
    word_at_position,
};

impl Backend {
    pub(crate) async fn folding_range_impl(
        &self,
        params: FoldingRangeParams,
    ) -> Result<Option<Vec<FoldingRange>>> {
        let uri = params.text_document.uri.to_string();
        let Some(text) = self.file_text_for(&uri).await else {
            return Ok(None);
        };
        // Brace-matched folding over the text: the parser drops the exact `}`
        // line (it consumes trailing whitespace after a clause), so a direct
        // scan is more accurate than the AST for the closing-brace line.
        let mut ranges = brace_folding_ranges(&text);
        ranges.extend(comment_and_region_folds(&text));
        if ranges.is_empty() {
            Ok(None)
        } else {
            Ok(Some(ranges))
        }
    }

    pub(crate) async fn selection_range_impl(
        &self,
        params: SelectionRangeParams,
    ) -> Result<Option<Vec<SelectionRange>>> {
        let uri = params.text_document.uri.to_string();
        let Some(text) = self.file_text_for(&uri).await else {
            return Ok(None);
        };
        let encoding = self.state.config.read().position_encoding.clone();
        let pairs = brace_pairs(&text);
        // One chain per requested position, in request order (LSP requires the
        // result to line up with `positions`).
        let out: Vec<SelectionRange> = params
            .positions
            .iter()
            .map(|pos| {
                // The conversion returns a 1-based line; only the char column
                // is needed (the request's 0-based line is used directly).
                let (_, col) = lsp_pos_to_source_in_text(&text, *pos, &encoding);
                let spans = selection_spans(&text, &pairs, pos.line, col as u32);
                let mut node: Option<SelectionRange> = None;
                for &((sl, sc), (el, ec)) in spans.iter().rev() {
                    node = Some(SelectionRange {
                        range: Range {
                            start: source_position_to_lsp(&text, sl, sc, &encoding),
                            end: source_position_to_lsp(&text, el, ec, &encoding),
                        },
                        parent: node.map(Box::new),
                    });
                }
                // Outside any token or block: an empty chain is not allowed,
                // so anchor at the cursor itself.
                node.unwrap_or(SelectionRange {
                    range: Range {
                        start: *pos,
                        end: *pos,
                    },
                    parent: None,
                })
            })
            .collect();
        Ok(Some(out))
    }

    pub(crate) async fn document_highlight_impl(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .to_string();
        let pos = params.text_document_position_params.position;
        let Some(text) = self.file_text_for(&uri).await else {
            return Ok(None);
        };
        // The identifier under the cursor: prefer the rule-resolved type-ref
        // instance name, falling back to the raw token in the text.
        let (ws_prefix, position_encoding) = {
            let cfg = self.state.config.read();
            (cfg.workspace_prefix.clone(), cfg.position_encoding.clone())
        };
        let logical_path = logical_path_from_uri(&uri, &ws_prefix);
        let (_, source_col) = lsp_pos_to_source_in_text(&text, pos, &position_encoding);
        let symbol = self
            .type_ref_at_cursor(&uri, pos, &logical_path)
            .map(|(_, name)| name)
            .or_else(|| word_at_position(&text, pos.line, source_col as u32))
            .filter(|s| !s.is_empty());
        let Some(symbol) = symbol else {
            return Ok(None);
        };
        let symbol = symbol.as_str();
        let highlights: Vec<DocumentHighlight> = text
            .lines()
            .enumerate()
            .flat_map(|(line0, line)| {
                let position_encoding = &position_encoding;
                let text = &text;
                code_token_cols_in_line(line, symbol)
                    .into_iter()
                    .map(move |col| DocumentHighlight {
                        range: source_range_in_text(
                            text,
                            line0 as u32,
                            col,
                            symbol,
                            position_encoding,
                        ),
                        kind: Some(highlight_kind(line, col, symbol)),
                    })
            })
            .collect();
        if highlights.is_empty() {
            Ok(None)
        } else {
            Ok(Some(highlights))
        }
    }

    pub(crate) async fn document_symbol_impl(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri.to_string();

        // Localisation file: per-file key outline from the loc entries.
        if crate::paths::is_loc_file(&uri)
            && let Some(resp) = self.loc_document_symbols(&params).await
        {
            return Ok(Some(resp));
        }

        // Hierarchical outline walked straight from the retained AST, when the
        // client advertises `hierarchicalDocumentSymbolSupport`. Falls through to
        // the flat instance/variable list otherwise (or when the AST is empty).
        if self
            .state
            .hierarchical_symbols
            .load(std::sync::atomic::Ordering::Relaxed)
            && let Some(ast) = self.ast_for(&uri)
        {
            let text = self.file_text_for(&uri).await.unwrap_or_default();
            let position_encoding = self.state.config.read().position_encoding.clone();
            let syms = build_doc_symbols(
                &ast.root_children,
                &ast.arena,
                &self.state.string_table,
                &text,
                &position_encoding,
            );
            if !syms.is_empty() {
                return Ok(Some(DocumentSymbolResponse::Nested(syms)));
            }
        }

        let (instances, variables) = {
            let info = self.state.info_service.read();
            let instances = info
                .type_index
                .instances_in_file(&uri)
                .into_iter()
                .map(|(type_name, inst)| (type_name.to_string(), inst.name.clone(), inst.location))
                .collect::<Vec<_>>();
            let variables = info
                .files
                .get(&uri)
                .map(|file_info| file_info.defined_variables.clone())
                .unwrap_or_default();
            (instances, variables)
        };

        let text = self.file_text_for(&uri).await;

        // Emit type instances as document symbols (one per named instance),
        // derived from the cross-file index — `FileInfo` no longer keeps a
        // per-file copy of these.
        let mut symbols: Vec<SymbolInformation> = instances
            .into_iter()
            .map(|(type_name, name, loc)| {
                make_symbol(
                    name.clone(),
                    SymbolKind::STRUCT,
                    Location {
                        uri: params.text_document.uri.clone(),
                        range: self.source_range_with_text(
                            text.as_deref(),
                            loc.line.saturating_sub(1),
                            loc.col as u32,
                            &name,
                        ),
                    },
                    Some(type_name),
                )
            })
            .collect();

        // Also include @-variables as symbols (still tracked per-file).
        for (name, loc) in variables {
            symbols.push(make_symbol(
                name.clone(),
                SymbolKind::CONSTANT,
                Location {
                    uri: params.text_document.uri.clone(),
                    range: self.source_range_with_text(
                        text.as_deref(),
                        loc.line.saturating_sub(1),
                        loc.col as u32,
                        &name,
                    ),
                },
                None,
            ));
        }

        if symbols.is_empty() {
            Ok(None)
        } else {
            Ok(Some(DocumentSymbolResponse::Flat(symbols)))
        }
    }

    async fn loc_document_symbols(
        &self,
        params: &DocumentSymbolParams,
    ) -> Option<DocumentSymbolResponse> {
        let uri = params.text_document.uri.to_string();
        let text = self.file_text_for(&uri).await?;
        if text.trim().is_empty() {
            return None;
        }
        let path = crate::paths::uri_to_path_str(&uri);
        let files = cwtools_localization::parse_loc_files(&path, &text, None).unwrap_or_default();
        if files.iter().all(|f| f.entries.is_empty()) {
            return None;
        }
        let encoding = self.state.config.read().position_encoding.clone();
        let hierarchical = self
            .state
            .hierarchical_symbols
            .load(std::sync::atomic::Ordering::Relaxed);
        if hierarchical {
            let mut syms: Vec<DocumentSymbol> = Vec::new();
            for file in &files {
                for entry in &file.entries {
                    let line0 = (entry.position.line.saturating_sub(1)) as u32;
                    let line_text = text.lines().nth(line0 as usize).unwrap_or("");
                    let col = line_text
                        .find(&entry.key)
                        .map(|b| line_text[..b].chars().count() as u32)
                        .unwrap_or(0);
                    let range = crate::navigation::helpers::source_range_in_text(
                        &text, line0, col, &entry.key, &encoding,
                    );
                    #[allow(deprecated)]
                    syms.push(DocumentSymbol {
                        name: entry.key.clone(),
                        detail: None,
                        kind: SymbolKind::KEY,
                        tags: None,
                        deprecated: None,
                        range,
                        selection_range: range,
                        children: None,
                    });
                }
            }
            if syms.is_empty() {
                None
            } else {
                Some(DocumentSymbolResponse::Nested(syms))
            }
        } else {
            let mut symbols: Vec<SymbolInformation> = Vec::new();
            for file in &files {
                for entry in &file.entries {
                    let line0 = (entry.position.line.saturating_sub(1)) as u32;
                    let line_text = text.lines().nth(line0 as usize).unwrap_or("");
                    let col = line_text
                        .find(&entry.key)
                        .map(|b| line_text[..b].chars().count() as u32)
                        .unwrap_or(0);
                    symbols.push(make_symbol(
                        entry.key.clone(),
                        SymbolKind::KEY,
                        Location {
                            uri: params.text_document.uri.clone(),
                            range: self.source_range_with_text(Some(&text), line0, col, &entry.key),
                        },
                        None,
                    ));
                }
            }
            if symbols.is_empty() {
                None
            } else {
                Some(DocumentSymbolResponse::Flat(symbols))
            }
        }
    }
}
