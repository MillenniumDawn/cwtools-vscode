use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::Backend;

use super::helpers::{SymbolCandidate, WORKSPACE_SYMBOL_LIMIT};
use super::{TopSymbols, make_symbol, symbol_rank};

impl Backend {
    pub(crate) async fn symbol_impl(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let query = params.query.to_lowercase();
        // Bounded top-k on the deterministic (rank, name, uri, line, col)
        // order: a max-heap whose root is the worst kept candidate, so a
        // non-improving match is rejected by one borrowed comparison before
        // any string is cloned. The old collect-then-sort materialized every
        // matching symbol — the whole workspace for an empty query (the
        // picker's initial list) — just to keep 500.
        let mut top = TopSymbols::new(WORKSPACE_SYMBOL_LIMIT);
        {
            let info = self.state.info_service.read();
            for (type_name, instances) in &info.type_index.map {
                for (file_uri, inst) in instances {
                    let Some(rank) = symbol_rank(&inst.name, &query) else {
                        continue;
                    };
                    let line0 = inst.location.line.saturating_sub(1);
                    let col = inst.location.col as u32;
                    if top.accepts(rank, &inst.name, file_uri, line0, col) {
                        top.push(SymbolCandidate {
                            rank,
                            name: inst.name.clone(),
                            container: Some(type_name.clone()),
                            kind: SymbolKind::STRUCT,
                            file_uri: file_uri.to_string(),
                            line0,
                            col,
                        });
                    }
                }
            }
            // `@`-constants, still tracked per-file (as in the document outline).
            for (file_uri, fi) in &info.files {
                for (name, loc) in &fi.defined_variables {
                    let Some(rank) = symbol_rank(name, &query) else {
                        continue;
                    };
                    let line0 = loc.line.saturating_sub(1);
                    let col = loc.col as u32;
                    if top.accepts(rank, name, file_uri, line0, col) {
                        top.push(SymbolCandidate {
                            rank,
                            name: name.clone(),
                            container: None,
                            kind: SymbolKind::CONSTANT,
                            file_uri: file_uri.clone(),
                            line0,
                            col,
                        });
                    }
                }
            }
        }
        // Localisation keys (stored lowercased; loc keys are conventionally
        // lowercase, so the display form matches the file).
        {
            let ll = self.state.loc_locations.read();
            for (key, (file_uri, line0)) in ll.iter() {
                let Some(rank) = symbol_rank(key, &query) else {
                    continue;
                };
                if top.accepts(rank, key, file_uri, *line0, 0) {
                    top.push(SymbolCandidate {
                        rank,
                        name: key.to_string(),
                        container: None,
                        kind: SymbolKind::KEY,
                        file_uri: file_uri.to_string(),
                        line0: *line0,
                        col: 0,
                    });
                }
            }
        }
        let cands = top.into_sorted_vec();

        let text_uris: Vec<String> = cands.iter().map(|c| c.file_uri.clone()).collect();
        let texts = self.file_text_snapshots_for(&text_uris).await;
        let mut symbols: Vec<SymbolInformation> = Vec::with_capacity(cands.len());
        // No request document to fall back to for a workspace-wide query.
        let fallback = Url::parse("file:///unknown").expect("static URI");
        for c in cands {
            symbols.push(make_symbol(
                c.name.clone(),
                c.kind,
                self.source_location_with_text(
                    &c.file_uri,
                    c.line0,
                    c.col,
                    &c.name,
                    &fallback,
                    texts
                        .get(&c.file_uri)
                        .map(|snapshot| snapshot.text.as_str()),
                ),
                c.container,
            ));
        }

        if symbols.is_empty() {
            Ok(None)
        } else {
            Ok(Some(symbols))
        }
    }
}
