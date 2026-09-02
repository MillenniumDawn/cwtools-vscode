use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::Backend;
use crate::lines::DocLines;
use crate::paths::{logical_path_from_uri, lsp_pos_to_source_in_text};

use super::{
    brace_folding_ranges, brace_pairs, build_doc_symbols, code_token_cols_in_line,
    comment_and_region_folds, highlight_kind, make_symbol, selection_spans, word_at_position,
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
        let encoding = self.position_encoding();
        let pairs = brace_pairs(&text);
        let lines = DocLines::new(&text, encoding.clone());
        let out: Vec<SelectionRange> = params
            .positions
            .iter()
            .map(|pos| {
                let (_, col) = lsp_pos_to_source_in_text(&text, *pos, &encoding);
                let spans = selection_spans(&text, &pairs, pos.line, col as u32);
                let mut node: Option<SelectionRange> = None;
                for &((sl, sc), (el, ec)) in spans.iter().rev() {
                    node = Some(SelectionRange {
                        range: Range {
                            start: lines.position(sl, sc),
                            end: lines.position(el, ec),
                        },
                        parent: node.map(Box::new),
                    });
                }
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
        let lines = DocLines::new(&text, position_encoding);
        let highlights: Vec<DocumentHighlight> = text
            .lines()
            .enumerate()
            .flat_map(|(line0, line)| {
                let lines = &lines;
                code_token_cols_in_line(line, symbol)
                    .into_iter()
                    .map(move |col| DocumentHighlight {
                        range: lines.token_range(line0 as u32, col, symbol),
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

        if crate::paths::is_loc_file(&uri)
            && let Some(resp) = self.loc_document_symbols(&params).await
        {
            return Ok(Some(resp));
        }

        if self
            .state
            .hierarchical_symbols
            .load(std::sync::atomic::Ordering::Relaxed)
            && let Some(ast) = self.ast_for(&uri)
        {
            let text = self.file_text_for(&uri).await.unwrap_or_default();
            // (#541).
            let lines = DocLines::new(&text, self.position_encoding());
            let syms = build_doc_symbols(
                &ast.root_children,
                &ast.arena,
                &self.state.string_table,
                &lines,
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
        let lines = text
            .as_deref()
            .map(|text| DocLines::new(text, self.position_encoding()));

        let mut symbols: Vec<SymbolInformation> = instances
            .into_iter()
            .map(|(type_name, name, loc)| {
                make_symbol(
                    name.clone(),
                    SymbolKind::STRUCT,
                    Location {
                        uri: params.text_document.uri.clone(),
                        range: self.source_range_with_lines(
                            lines.as_ref(),
                            loc.line.saturating_sub(1),
                            loc.col as u32,
                            &name,
                        ),
                    },
                    Some(type_name),
                )
            })
            .collect();

        for (name, loc) in variables {
            symbols.push(make_symbol(
                name.clone(),
                SymbolKind::CONSTANT,
                Location {
                    uri: params.text_document.uri.clone(),
                    range: self.source_range_with_lines(
                        lines.as_ref(),
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
        let lines = DocLines::new(&text, self.position_encoding());
        let hierarchical = self
            .state
            .hierarchical_symbols
            .load(std::sync::atomic::Ordering::Relaxed);
        if hierarchical {
            let mut syms: Vec<DocumentSymbol> = Vec::new();
            for file in &files {
                for entry in &file.entries {
                    let line0 = (entry.position.line.saturating_sub(1)) as u32;
                    let line_text = lines.line(line0);
                    let col = line_text
                        .find(&entry.key)
                        .map(|b| line_text[..b].chars().count() as u32)
                        .unwrap_or(0);
                    let range = lines.token_range(line0, col, &entry.key);
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
                    let line_text = lines.line(line0);
                    let col = line_text
                        .find(&entry.key)
                        .map(|b| line_text[..b].chars().count() as u32)
                        .unwrap_or(0);
                    symbols.push(make_symbol(
                        entry.key.clone(),
                        SymbolKind::KEY,
                        Location {
                            uri: params.text_document.uri.clone(),
                            range: lines.token_range(line0, col, &entry.key),
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
