use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::Backend;

use super::helpers::{SymbolCandidate, WORKSPACE_SYMBOL_LIMIT};
use super::{TopSymbols, make_symbol, symbol_rank};

impl Backend {
    pub(crate) fn symbol_impl(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let query = params.query.to_lowercase();
        let mut top = TopSymbols::new(WORKSPACE_SYMBOL_LIMIT);
        {
            let info = self.state.info_service.read();
            for (type_name, instances) in info.type_index.map() {
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
        {
            let ll = self.state.loc_locations.read();
            for (key, (file_uri, line0)) in ll.iter() {
                let Some(rank) = symbol_rank(key, &query) else {
                    continue;
                };
                if top.accepts(rank, key, file_uri, line0, 0) {
                    top.push(SymbolCandidate {
                        rank,
                        name: key.to_string(),
                        container: None,
                        kind: SymbolKind::KEY,
                        file_uri: file_uri.to_string(),
                        line0,
                        col: 0,
                    });
                }
            }
        }
        let cands = top.into_sorted_vec();

        let text_uris: Vec<String> = cands.iter().map(|c| c.file_uri.clone()).collect();
        let texts = self.open_text_snapshots_for(&text_uris);
        let indexed = crate::lines::index_snapshots(&texts, &self.position_encoding());
        let mut symbols: Vec<SymbolInformation> = Vec::with_capacity(cands.len());
        let fallback = Url::parse("file:///unknown").expect("static URI");
        for c in cands {
            symbols.push(make_symbol(
                c.name.clone(),
                c.kind,
                self.source_location_with_lines(
                    &c.file_uri,
                    c.line0,
                    c.col,
                    &c.name,
                    &fallback,
                    indexed.get(c.file_uri.as_str()),
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use cwtools_info::{FileInfo, SourceLocation};

    use super::*;
    use crate::access::{MAX_URI_READ_BYTES, read_authorized_text, recorded_reads_under};
    use crate::paths::path_to_uri;
    use crate::testing::{open_document, workspace_backend};

    fn define_variable(backend: &Backend, uri: &str, name: &str, line: u32, col: u16) {
        let location = SourceLocation {
            line,
            col,
            end: (line, col),
        };
        backend.state.info_service.write().files.insert(
            uri.to_string(),
            FileInfo {
                defined_variables: HashMap::from([(name.to_string(), location)]),
                ..FileInfo::default()
            },
        );
    }

    #[test]
    fn closed_files_get_their_ranges_without_a_read() {
        let (backend, workspace) = workspace_backend();
        let write = |name: &str, text: &str| {
            let path = workspace.path().join(name);
            std::fs::write(&path, text).expect("write fixture");
            path_to_uri(&path)
        };
        let ascii = write("ascii.txt", "ascii_var = 1\n");
        // Accented and CJK text is one UTF-16 unit per char, so the index
        // column is already exact after it.
        let bmp = write("bmp.txt", "caf\u{e9}\u{4e16} bmp_var = 1\n");
        let open = path_to_uri(&workspace.path().join("open.txt"));
        open_document(&backend, &open, "\u{1f600} open_var = 1\n");
        define_variable(&backend, &ascii, "ascii_var", 1, 0);
        define_variable(&backend, &bmp, "bmp_var", 1, 6);
        define_variable(&backend, &open, "open_var", 1, 2);
        let before = recorded_reads_under(workspace.path());

        let symbols = backend
            .symbol_impl(WorkspaceSymbolParams {
                query: "_var".to_string(),
                ..WorkspaceSymbolParams::default()
            })
            .expect("workspace symbol request")
            .expect("three matching symbols");

        let range = |name: &str| {
            let symbol = symbols.iter().find(|s| s.name == name).expect(name);
            (symbol.location.range.start, symbol.location.range.end)
        };
        assert_eq!(
            range("ascii_var"),
            (Position::new(0, 0), Position::new(0, 9))
        );
        assert_eq!(
            range("bmp_var"),
            (Position::new(0, 6), Position::new(0, 13))
        );
        assert_eq!(
            range("open_var"),
            (Position::new(0, 3), Position::new(0, 11))
        );
        assert_eq!(
            recorded_reads_under(workspace.path()),
            before,
            "workspace symbol must answer from the index, not the disk"
        );

        // A read of the closed file is recorded, so the equality above cannot
        // hold because the instrumentation went quiet.
        let roots = backend.state.config.read().authorized_roots.clone();
        assert!(read_authorized_text(&ascii, &roots, MAX_URI_READ_BYTES).is_some());
        assert_eq!(recorded_reads_under(workspace.path()), before + 1);
    }
}
