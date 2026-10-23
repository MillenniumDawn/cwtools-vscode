// Crate root for the cwtools-server binary and for `benches/`. The `Backend`
// implementation and the `LanguageServer` trait dispatch live in `server.rs`
// so the Rust coverage gate can measure them while keeping this file's
// entrypoint plumbing excluded (#662); `src/main.rs` is a thin `fn main` over
// `run` below and is excluded along with it, so keep this file to module
// declarations, `run`, and tests -- anything with logic belongs in a module
// the gate measures. Notification type aliases are re-exported here so
// sibling modules can keep their `crate::LoadingBar` / `crate::UpdateFileList`
// paths.

use std::sync::Arc;

use tower_lsp::{LspService, Server};

mod access;
#[cfg(test)]
mod baseline;
mod cache_purge;
mod call_hierarchy;
mod code_action;
mod codelens;
mod color;
mod command_progress;
mod completion;
mod config;
mod cursor;
mod documentlink;
mod format;
mod graph;
mod hover;
mod inlay;
mod lines;
mod loc_locations;
mod loc_text;
mod navigation;
mod paths;
mod scan;
mod semantic;
mod server;
mod state;
#[cfg(test)]
mod testing;
mod transport;
mod validate;

pub(crate) use cursor::{RuleCursorInfo, hint_from_rule_right};
pub(crate) use server::{LoadingBar, UpdateFileList};
pub(crate) use state::{
    AstSource, Backend, CompletionCacheEntry, DeferredRulesMessage, DocumentState,
    FileTextSnapshot, FixableEdits, LocLocations, LocText, SemanticCacheEntry, ValidateTrigger,
};

/// Re-exports for this crate's `benches/`, which link it as an external crate
/// and so can only see `pub` items. Not an API for anyone else: the bench
/// targets are the only consumer, which is why it is `doc(hidden)` and why the
/// items behind it stay in otherwise private modules (#471).
#[doc(hidden)]
pub mod bench_support {
    pub use crate::completion::{
        CONTEXT_CAP, CONTEXT_COMPLETE_THRESHOLD, LocKeyIndex, ValueCompletionSets,
        completions_from_rules, expanded_modifier_scopes, prepare_context_items, select_loc_keys,
        value_completions,
    };
    pub use crate::lines::DocLines;
    pub use crate::navigation::build_doc_symbols;
    pub use crate::paths::{logical_path_from_uri, workspace_prefix_of};
    pub use crate::state::LineIndex;
    pub fn semantic_tokens(
        file: &cwtools_parser::ast::ParsedFile,
        table: &cwtools_string_table::string_table::StringTable,
        text: &str,
        encoding: &tower_lsp::lsp_types::PositionEncodingKind,
        span: Option<(u32, u32)>,
    ) -> Vec<tower_lsp::lsp_types::SemanticToken> {
        let index = crate::state::LineIndex::new(text);
        crate::semantic::encode(crate::semantic::semantic_tokens_indexed(
            file, table, text, encoding, None, span, &index,
        ))
    }
    pub fn semantic_tokens_indexed(
        file: &cwtools_parser::ast::ParsedFile,
        table: &cwtools_string_table::string_table::StringTable,
        text: &str,
        encoding: &tower_lsp::lsp_types::PositionEncodingKind,
        span: Option<(u32, u32)>,
        index: &crate::state::LineIndex,
    ) -> Vec<tower_lsp::lsp_types::SemanticToken> {
        crate::semantic::encode(crate::semantic::semantic_tokens_indexed(
            file, table, text, encoding, None, span, index,
        ))
    }
    pub fn semantic_tokens_with_rules(
        file: &cwtools_parser::ast::ParsedFile,
        prepared: &cwtools_validation::Prepared<'_>,
        text: &str,
        logical_path: &str,
        encoding: &tower_lsp::lsp_types::PositionEncodingKind,
        span: Option<(u32, u32)>,
        index: &crate::state::LineIndex,
    ) -> Vec<tower_lsp::lsp_types::SemanticToken> {
        crate::semantic::encode(crate::semantic::semantic_tokens_indexed(
            file,
            prepared.table,
            text,
            encoding,
            Some((prepared, logical_path)),
            span,
            index,
        ))
    }
    pub use crate::validate::{collect_doc_tokens, loc_extra_valid_refs, parse_loc_buffer};
}

/// Serve LSP over stdin/stdout until the client closes stdin. `src/main.rs`
/// does nothing but argument handling and a call to this.
pub fn run() {
    cwtools_profiling::init_tracing();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
        .block_on(async {
            let state = Arc::new(DocumentState::new());
            if let Some(mut jobs) = state.notifications.take_receiver() {
                tokio::spawn(async move {
                    // Awaited one at a time, so the client's notification order
                    // is preserved; spawned, so a panicking handler does not
                    // take the worker down with it and a `block_in_place`
                    // inside one is on a task of its own (#470).
                    while let Some(job) = jobs.recv().await {
                        scan::spawn_logging_panics("notification handler", job).await;
                    }
                });
            }
            let (stdin, stdout) = (
                transport::BoundedLspReader::new(tokio::io::stdin()),
                tokio::io::stdout(),
            );
            let (service, socket) = LspService::build(|client| Backend {
                client,
                state: state.clone(),
            })
            .custom_method("didFocusFile", Backend::on_did_focus_file)
            .custom_method(
                "window/workDoneProgress/cancel",
                Backend::on_work_done_progress_cancel,
            )
            .finish();
            Server::new(stdin, stdout, socket)
                .concurrency_level(transport::CONCURRENCY_LEVEL)
                .serve(transport::SpawnRequests::with_notifications(
                    service,
                    state.notifications.clone(),
                ))
                .await;
            tracing::info!("LSP server shut down (stdin closed)");
        });
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use parking_lot::Mutex;
    use serde_json::Value;
    use tower_lsp::LanguageServer;
    use tower_lsp::LspService;
    use tower_lsp::lsp_types::{
        DidChangeTextDocumentParams, DidSaveTextDocumentParams, FileRename, RenameFilesParams,
        TextDocumentContentChangeEvent, TextDocumentIdentifier, Url,
        VersionedTextDocumentIdentifier,
    };

    use crate::state::{
        Backend, Config, DebounceTask, DocumentRejection, DocumentState, DocumentStore,
        LocDocumentCache, MAX_DOCUMENT_BYTES, MAX_OPEN_DOCUMENTS, MAX_RETAINED_DOCUMENT_BYTES,
        ParsedDoc, remove_debounce_task,
    };
    use crate::testing::test_backend;
    use cwtools_info::{IndexedUseSite, SourceLocation, UseSite};
    use cwtools_parser::parser::parse_string;
    use cwtools_string_table::string_table::StringTable;

    /// The base game and the rules dir are readable but never writable, so `refresh_roots` must
    /// keep them out of `editable_roots`, or a generated edit could write into the install (#160).
    #[test]
    fn refresh_roots_keeps_read_only_roots_out_of_the_edit_boundary() {
        let ws = tempfile::TempDir::new().expect("tmpdir");
        let vanilla = tempfile::TempDir::new().expect("tmpdir");
        let rules = tempfile::TempDir::new().expect("tmpdir");
        let parent_mod = tempfile::TempDir::new().expect("tmpdir");
        let canonical = |dir: &std::path::Path| std::fs::canonicalize(dir).expect("canonical");

        let mut cfg = Config::new();
        cfg.workspace_roots = vec![ws.path().to_path_buf()];
        cfg.vanilla_dir = Some(vanilla.path().to_path_buf());
        cfg.rules_dir = Some(rules.path().to_path_buf());
        cfg.parent_roots = vec![parent_mod.path().to_path_buf()];
        cfg.refresh_roots();

        assert_eq!(cfg.editable_roots.as_ref(), [canonical(ws.path())]);
        for root in [ws.path(), vanilla.path(), rules.path(), parent_mod.path()] {
            assert!(
                cfg.authorized_roots.contains(&canonical(root)),
                "{} must stay readable",
                root.display()
            );
        }
    }

    #[test]
    fn refresh_roots_does_not_authorize_a_rules_ancestor_of_the_workspace() {
        let parent = tempfile::TempDir::new().expect("parent");
        let workspace = parent.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        let vanilla = tempfile::TempDir::new().expect("vanilla");
        let outside = parent.path().join("outside.txt");
        std::fs::write(&outside, "not in the workspace\n").expect("outside");
        let canonical = |path: &std::path::Path| std::fs::canonicalize(path).expect("canonical");

        let mut cfg = Config::new();
        cfg.workspace_roots = vec![workspace.clone()];
        cfg.vanilla_dir = Some(vanilla.path().to_path_buf());
        cfg.rules_dir = Some(parent.path().to_path_buf());
        cfg.refresh_roots();

        assert_eq!(cfg.editable_roots.as_ref(), [canonical(&workspace)]);
        assert!(cfg.authorized_roots.contains(&canonical(&workspace)));
        assert!(cfg.authorized_roots.contains(&canonical(vanilla.path())));
        assert!(!cfg.authorized_roots.contains(&canonical(parent.path())));
        let outside_uri = Url::from_file_path(&outside).expect("file URI").to_string();
        assert_eq!(
            crate::access::authorized_path(&outside_uri, &cfg.authorized_roots),
            None
        );
    }

    fn document(text: &str) -> ParsedDoc {
        ParsedDoc {
            version: 1,
            text: Arc::from(text),
            ast: None,
            ast_version: None,
            ast_source_bytes: 0,
            loc_cache: None,
        }
    }

    fn indexed_site(uri: &str, name: &str) -> IndexedUseSite {
        IndexedUseSite {
            type_name: "focus".to_string(),
            name: name.to_string(),
            case_insensitive: false,
            site: UseSite {
                file: Arc::from(uri),
                key: SourceLocation {
                    line: 0,
                    col: 0,
                    end: (0, 1),
                },
                value: SourceLocation {
                    line: 0,
                    col: 2,
                    end: (0, 3),
                },
            },
        }
    }

    fn loc_document_cache(version: i32, retained_bytes: usize) -> Arc<LocDocumentCache> {
        Arc::new(LocDocumentCache {
            version,
            retained_bytes,
            files: Vec::new(),
            references: HashSet::new(),
        })
    }

    /// `text_of` hands back the stored buffer, not a copy of it. Pointer
    /// identity is the whole point: the readers that call it run at
    /// cursor-movement cadence, and a copy there is a document-sized
    /// allocation per request (#473).
    #[test]
    fn document_store_text_of_shares_the_buffer() {
        let mut store = DocumentStore::new();
        let doc = document("focus_tree = { id = a }");
        let stored = doc.text.clone();
        store.open("file:///doc".to_string(), doc).unwrap();

        for _ in 0..32 {
            let text = store.text_of("file:///doc").expect("the doc is open");
            assert!(
                Arc::ptr_eq(&stored, &text),
                "text_of copied the buffer instead of sharing it"
            );
        }

        assert_eq!(Arc::strong_count(&stored), 2, "every read must be released");
        assert!(store.text_of("file:///not-open").is_none());
    }

    #[test]
    fn document_store_bounds_a_burst_of_distinct_opens() {
        let mut store = DocumentStore::new();
        for i in 0..MAX_OPEN_DOCUMENTS {
            store
                .open(format!("file:///doc-{i}"), document(""))
                .unwrap();
        }

        assert_eq!(
            store.open("file:///one-too-many".to_string(), document("")),
            Err(DocumentRejection::TooManyOpen)
        );
        assert_eq!(store.len(), MAX_OPEN_DOCUMENTS);
    }

    #[test]
    fn document_store_accounts_for_replace_and_close() {
        let mut store = DocumentStore::new();
        store
            .open("file:///doc".to_string(), document("old"))
            .unwrap();
        store
            .change("file:///doc", 2, Arc::from("replacement"))
            .unwrap();
        assert_eq!(
            store.retained_text_bytes,
            "replacement".len() + store.line_index_of("file:///doc").unwrap().retained_bytes()
        );

        store.remove("file:///doc");
        assert_eq!(store.retained_text_bytes, 0);
    }

    #[test]
    fn document_store_versions_and_accounts_for_loc_caches() {
        let mut store = DocumentStore::new();
        store
            .open("file:///doc".to_string(), document("text"))
            .unwrap();

        assert_eq!(
            store.set_loc_cache("file:///doc", 0, loc_document_cache(0, 10)),
            Ok(false)
        );
        assert_eq!(
            store.set_loc_cache("file:///doc", 1, loc_document_cache(1, 10)),
            Ok(true)
        );
        let doc_index_bytes = store.line_index_of("file:///doc").unwrap().retained_bytes();
        assert_eq!(
            store.retained_text_bytes,
            "text".len() + 10 + doc_index_bytes
        );
        let retained_before_rejected_cache = store.retained_text_bytes;
        assert_eq!(
            store.set_loc_cache("file:///doc", 1, loc_document_cache(1, MAX_DOCUMENT_BYTES)),
            Err(DocumentRejection::TooLarge)
        );
        assert_eq!(store.retained_text_bytes, retained_before_rejected_cache);
        store
            .open("file:///cached".to_string(), document("z"))
            .unwrap();
        assert_eq!(
            store.set_loc_cache("file:///cached", 1, loc_document_cache(1, 10)),
            Ok(true)
        );
        let cached_index_bytes = store
            .line_index_of("file:///cached")
            .unwrap()
            .retained_bytes();
        assert_eq!(
            store.retained_text_bytes,
            "text".len() + 10 + doc_index_bytes + 1 + 10 + cached_index_bytes
        );
        store.remove("file:///cached");
        assert_eq!(
            store.retained_text_bytes,
            "text".len() + 10 + doc_index_bytes
        );

        store.change("file:///doc", 2, Arc::from("x")).unwrap();
        assert_eq!(
            store.retained_text_bytes,
            1 + store.line_index_of("file:///doc").unwrap().retained_bytes()
        );
        assert!(store.get("file:///doc").unwrap().loc_cache.is_none());

        store.remove("file:///doc");
        assert_eq!(store.retained_text_bytes, 0);
    }

    #[test]
    fn document_store_keeps_stale_ast_source_in_the_budget() {
        let source = "root = { value = 1 }";
        let ast = Arc::new(parse_string(source, &StringTable::new()));
        let mut store = DocumentStore::new();
        store
            .open("file:///doc".to_string(), document(source))
            .unwrap();
        assert!(store.set_ast("file:///doc", 1, ast));

        store.change("file:///doc", 2, Arc::from("x")).unwrap();

        assert_eq!(
            store.retained_text_bytes,
            source.len() + store.line_index_of("file:///doc").unwrap().retained_bytes()
        );
    }

    #[test]
    fn open_reference_overlay_changes_only_with_the_matching_ast_version() {
        let uri = "file:///doc.txt";
        let table = StringTable::new();
        let mut store = DocumentStore::new();
        store
            .open(uri.to_string(), document("old = { base = first }"))
            .unwrap();
        let first = IndexedUseSite {
            type_name: "focus".to_string(),
            name: "first".to_string(),
            case_insensitive: false,
            site: UseSite {
                file: Arc::from(uri),
                key: SourceLocation {
                    line: 0,
                    col: 7,
                    end: (0, 11),
                },
                value: SourceLocation {
                    line: 0,
                    col: 14,
                    end: (0, 19),
                },
            },
        };
        assert!(store.set_ast_with_use_sites(
            uri,
            1,
            Arc::new(parse_string("old = { base = first }", &table)),
            vec![first.clone()],
        ));
        let one_site_bytes = store.retained_text_bytes;
        assert!(one_site_bytes > "old = { base = first }".len());
        let alias = IndexedUseSite {
            name: "FIRST".to_string(),
            case_insensitive: true,
            ..first.clone()
        };
        assert!(store.replace_reference_sites(uri, 1, vec![first.clone(), alias.clone()]));
        assert!(store.retained_text_bytes > one_site_bytes);
        assert_eq!(store.reference_sites("focus", "first").len(), 2);
        assert_eq!(store.reference_sites("focus", "FIRST").len(), 1);
        assert!(store.replace_reference_sites(uri, 1, vec![first.clone()]));
        assert_eq!(store.retained_text_bytes, one_site_bytes);
        assert!(store.replace_reference_sites(uri, 1, vec![first.clone(), alias]));
        store
            .change(uri, 2, Arc::from("new = { base = second }"))
            .unwrap();
        assert!(!store.set_ast_with_use_sites(
            uri,
            1,
            Arc::new(parse_string("stale = { base = wrong }", &table)),
            Vec::new(),
        ));
        assert!(
            !store.replace_reference_sites(uri, 1, Vec::new()),
            "a post-scan alias refresh must not replace references for a stale AST version"
        );
        assert_eq!(
            store.reference_sites("focus", "FIRST").len(),
            1,
            "the last accepted overlay remains available while the new version is parsed"
        );
        let second = IndexedUseSite {
            name: "second".to_string(),
            site: UseSite {
                file: Arc::from(uri),
                key: SourceLocation {
                    line: 0,
                    col: 7,
                    end: (0, 11),
                },
                value: SourceLocation {
                    line: 0,
                    col: 14,
                    end: (0, 20),
                },
            },
            ..first.clone()
        };
        assert!(store.set_ast_with_use_sites(
            uri,
            2,
            Arc::new(parse_string("new = { base = second }", &table)),
            vec![second.clone()],
        ));
        assert!(store.replace_reference_sites(uri, 2, vec![second]));
        assert!(store.reference_sites("focus", "first").is_empty());
        assert_eq!(store.reference_sites("focus", "second").len(), 1);

        store.remove(uri);
        assert_eq!(store.retained_text_bytes, 0);
        assert!(store.reference_sites("focus", "second").is_empty());
        store.open(uri.to_string(), document("reopened")).unwrap();
        assert_eq!(
            store.retained_text_bytes,
            "reopened".len() + store.line_index_of(uri).unwrap().retained_bytes()
        );
        store.remove(uri);
        assert_eq!(store.retained_text_bytes, 0);
    }

    #[test]
    fn document_store_rejects_over_budget_index_without_partial_publication() {
        let uri = "file:///doc.txt";
        let table = StringTable::new();
        let first = indexed_site(uri, "first");
        let second = indexed_site(uri, "second");

        let mut ast_store = DocumentStore::new();
        ast_store
            .open(uri.to_string(), document("root = first"))
            .unwrap();
        ast_store.retained_text_bytes = MAX_RETAINED_DOCUMENT_BYTES - 1;
        assert!(!ast_store.set_ast_with_use_sites(
            uri,
            1,
            Arc::new(parse_string("root = first", &table)),
            vec![first.clone()],
        ));
        assert!(ast_store.get(uri).unwrap().ast.is_none());
        assert!(ast_store.reference_sites("focus", "first").is_empty());

        let mut reference_store = DocumentStore::new();
        reference_store
            .open(uri.to_string(), document("root = first"))
            .unwrap();
        assert!(reference_store.set_ast_with_use_sites(
            uri,
            1,
            Arc::new(parse_string("root = first", &table)),
            vec![first.clone()],
        ));
        reference_store.retained_text_bytes = MAX_RETAINED_DOCUMENT_BYTES - 1;
        assert!(!reference_store.replace_reference_sites(uri, 1, vec![first.clone(), second],));
        assert_eq!(reference_store.reference_sites("focus", "first").len(), 1);
        assert!(
            reference_store
                .reference_sites("focus", "second")
                .is_empty()
        );
    }

    #[test]
    fn document_store_charges_uri_arcs_and_retains_sparse_index_capacity() {
        let large_uri = format!("file:///{}", "l".repeat(1024));
        let small_uri = format!("file:///{}", "s".repeat(1024));
        let table = StringTable::new();
        let large_sites: Vec<_> = (0..3_600)
            .map(|i| indexed_site(&large_uri, &format!("large_{i}")))
            .collect();
        let small_sites: Vec<_> = (0..3_000)
            .map(|i| indexed_site(&small_uri, &format!("small_{i}")))
            .collect();

        let mut store = DocumentStore::new();
        store
            .open(large_uri.clone(), document("root = large_0"))
            .unwrap();
        store
            .open(small_uri.clone(), document("root = small_0"))
            .unwrap();
        assert!(store.set_ast_with_use_sites(
            &large_uri,
            1,
            Arc::new(parse_string("root = large_0", &table)),
            large_sites.clone(),
        ));
        assert!(store.set_ast_with_use_sites(
            &small_uri,
            1,
            Arc::new(parse_string("root = small_0", &table)),
            small_sites.clone(),
        ));
        assert!(store.retained_text_bytes > large_sites.len() * large_uri.len() * 2);

        let mut small_only = DocumentStore::new();
        small_only
            .open(small_uri.clone(), document("root = small_0"))
            .unwrap();
        assert!(small_only.set_ast_with_use_sites(
            &small_uri,
            1,
            Arc::new(parse_string("root = small_0", &table)),
            small_sites,
        ));
        let small_retained = small_only.retained_text_bytes;

        store.remove(&large_uri);
        assert!(
            store.retained_text_bytes > small_retained,
            "closing a large overlay must not discard spare index capacity"
        );
        store.remove(&small_uri);
        assert_eq!(store.retained_text_bytes, 0);
    }

    #[test]
    fn document_store_shared_reference_capacity_is_stable_across_reindexing() {
        let table = StringTable::new();
        let mut store = DocumentStore::new();
        for uri in ["file:///first.txt", "file:///second.txt"] {
            store
                .open(uri.to_string(), document("root = shared"))
                .unwrap();
            assert!(store.set_ast_with_use_sites(
                uri,
                1,
                Arc::new(parse_string("root = shared", &table)),
                vec![indexed_site(uri, "shared")],
            ));
        }
        let retained = store.retained_text_bytes;
        for _ in 0..20 {
            assert!(store.replace_reference_sites(
                "file:///first.txt",
                1,
                vec![indexed_site("file:///first.txt", "shared")],
            ));
            assert_eq!(store.retained_text_bytes, retained);
        }
        store.remove("file:///first.txt");
        store.remove("file:///second.txt");
        assert_eq!(store.retained_text_bytes, 0);
    }

    #[test]
    fn document_store_global_reference_capacity_does_not_reject_small_documents() {
        let table = StringTable::new();
        let mut store = DocumentStore::new();
        for file in 0..8 {
            let uri = format!("file:///indexed_{file}.txt");
            store.open(uri.clone(), document("root = value")).unwrap();
            let sites = (0..2_500)
                .map(|site| indexed_site(&uri, &format!("value_{file}_{site}")))
                .collect();
            assert!(store.set_ast_with_use_sites(
                &uri,
                1,
                Arc::new(parse_string("root = value", &table)),
                sites,
            ));
        }
        assert!(store.retained_text_bytes > MAX_DOCUMENT_BYTES);
        assert!(store.retained_text_bytes < MAX_RETAINED_DOCUMENT_BYTES);
        let uri = "file:///small.txt";
        store.open(uri.to_string(), document("x")).unwrap();
        store.change(uri, 2, Arc::from("y")).unwrap();
        assert!(store.set_ast(uri, 2, Arc::new(parse_string("y", &table))));
        assert!(store.replace_reference_sites(uri, 2, vec![indexed_site(uri, "value")]));
        assert_eq!(
            store.set_loc_cache(uri, 2, loc_document_cache(2, 10)),
            Ok(true)
        );
        store.open(uri.to_string(), document("reopened")).unwrap();
        for file in 0..8 {
            store.remove(&format!("file:///indexed_{file}.txt"));
        }
        store.remove(uri);
        assert_eq!(store.retained_text_bytes, 0);
    }

    #[test]
    fn workspace_change_during_admission_rechecks_authorization() {
        let state = DocumentState::new();
        let mut checks = 0;

        let result = state.open_workspace_document(
            "file:///doc".to_string(),
            document("text"),
            cwtools_cache::workspace::content_hash("text"),
            || {
                checks += 1;
                if checks == 1 {
                    state
                        .workspace_roots_generation
                        .fetch_add(1, Ordering::Release);
                    true
                } else {
                    false
                }
            },
        );

        assert_eq!(result, Err(DocumentRejection::OutsideWorkspace));
        assert!(state.documents.lock().is_empty());
    }

    /// tower-lsp holds a pending slot per server→client request and `expect`s
    /// the receiver to still be there when the client answers, so dropping the
    /// future that awaits the reply panics the whole server. Every edit aborts
    /// the previous debounced validation, and that validation ends on
    /// `code_lens_refresh` — so typing used to be enough to kill the process
    /// (#675). The reply must land safely even after the abort.
    #[tokio::test]
    async fn an_aborted_code_lens_refresh_survives_the_clients_reply() {
        use futures_util::{SinkExt, StreamExt};
        use tower::Service;
        use tower_lsp::jsonrpc;
        use tower_lsp::lsp_types::{InitializeParams, InitializeResult};

        // tower-lsp suppresses every server→client message until the service
        // has answered an `initialize` request, so without a handshake the
        // refresh never reaches the socket. Only the inbound half is stubbed:
        // driving `initialize` through the real `Backend` would put its own
        // client requests on the socket ahead of the refresh.
        struct Handshake;
        #[tower_lsp::async_trait]
        impl LanguageServer for Handshake {
            async fn initialize(&self, _: InitializeParams) -> jsonrpc::Result<InitializeResult> {
                Ok(InitializeResult::default())
            }

            async fn shutdown(&self) -> jsonrpc::Result<()> {
                Ok(())
            }
        }

        let state = Arc::new(DocumentState::new());
        state
            .code_lens_refresh_support
            .store(true, Ordering::Relaxed);
        let captured = Arc::new(Mutex::new(None));
        let slot = captured.clone();
        let (mut service, mut socket) = LspService::new(move |client| {
            *slot.lock() = Some(client);
            Handshake
        });
        std::future::poll_fn(|cx| service.poll_ready(cx))
            .await
            .expect("the service must accept the handshake");
        let handshake = service
            .call(
                jsonrpc::Request::build("initialize")
                    .params(serde_json::json!({ "capabilities": {} }))
                    .id(1_i64)
                    .finish(),
            )
            .await
            .expect("initialize must route");
        assert!(
            handshake.is_some_and(|res| res.is_ok()),
            "tower-lsp suppresses server-to-client requests until the handshake succeeds"
        );

        let backend = Backend {
            client: captured.lock().take().expect("client"),
            state,
        };

        let refresh = tokio::spawn(async move { backend.request_code_lens_refresh().await });
        let request = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await
            .expect("server never asked the client to refresh code lenses")
            .expect("socket closed");
        assert_eq!(request.method(), "workspace/codeLens/refresh");
        let id = request
            .id()
            .expect("refresh is a request, not a notification")
            .clone();

        refresh.abort();
        assert!(
            refresh.await.unwrap_err().is_cancelled(),
            "the refresh task must be cancelled while the reply is still pending"
        );

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            socket.send(tower_lsp::jsonrpc::Response::from_ok(id, Value::Null)),
        )
        .await
        .expect("routing the reply stalled")
        .expect("the reply must route without panicking the server");
    }

    #[test]
    fn document_state_configures_two_validation_permits() {
        let state = DocumentState::new();
        let _first = state.validation_permits.try_acquire().unwrap();
        let _second = state.validation_permits.try_acquire().unwrap();
        assert!(state.validation_permits.try_acquire().is_err());
    }

    #[tokio::test]
    async fn completed_debounce_task_does_not_remove_its_replacement() {
        let handle = tokio::spawn(std::future::pending::<()>());
        let abort = handle.abort_handle();
        let (_finished_tx, finished) = tokio::sync::oneshot::channel();
        let mut tasks = HashMap::from([(
            "file:///doc".to_string(),
            DebounceTask {
                id: 2,
                abort,
                finished,
            },
        )]);

        remove_debounce_task(&mut tasks, "file:///doc", 1);
        assert_eq!(tasks.len(), 1);
        remove_debounce_task(&mut tasks, "file:///doc", 2);
        assert!(tasks.is_empty());
        handle.abort();
    }

    #[test]
    fn document_store_rejects_unsolicited_changes() {
        let mut store = DocumentStore::new();

        assert_eq!(
            store.change("file:///missing", 1, Arc::from("text")),
            Err(DocumentRejection::NotOpen)
        );
    }

    #[tokio::test]
    async fn did_change_cannot_create_document_or_validation_state() {
        let backend = test_backend();
        let state = backend.state.clone();

        backend
            .did_change(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: Url::parse("file:///not-open.txt").unwrap(),
                    version: 1,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "root = { value = 1 }".to_string(),
                }],
            })
            .await;

        assert!(state.documents.lock().is_empty());
        assert!(state.debounce_handles.lock().is_empty());
    }

    #[tokio::test]
    async fn did_rename_clears_the_path_bound_loc_cache() {
        let backend = test_backend();
        let state = backend.state.clone();
        let old_uri = "file:///localisation/a_l_english.yml";
        let new_uri = "file:///localisation/a_l_french.yml";
        state
            .documents
            .lock()
            .open(
                old_uri.to_string(),
                document("l_english:\n KEY:0 \"value\"\n"),
            )
            .unwrap();
        assert!(
            state
                .documents
                .lock()
                .set_loc_cache(old_uri, 1, loc_document_cache(1, 10))
                .unwrap()
        );

        backend
            .did_rename_files(RenameFilesParams {
                files: vec![FileRename {
                    old_uri: old_uri.to_string(),
                    new_uri: new_uri.to_string(),
                }],
            })
            .await;

        let documents = state.documents.lock();
        assert!(!documents.contains_key(old_uri));
        assert!(documents.get(new_uri).unwrap().loc_cache.is_none());
    }

    #[tokio::test]
    async fn did_save_keeps_the_pending_validation() {
        let backend = test_backend();
        let state = backend.state.clone();
        let uri = "file:///pending.txt";
        state
            .documents
            .lock()
            .open(uri.to_string(), document("root = { value = 1 }"))
            .unwrap();
        let pending = tokio::spawn(std::future::pending::<()>());
        let (_finished_tx, finished) = tokio::sync::oneshot::channel();
        state.debounce_handles.lock().insert(
            uri.to_string(),
            DebounceTask {
                id: 7,
                abort: pending.abort_handle(),
                finished,
            },
        );

        backend
            .did_save(DidSaveTextDocumentParams {
                text_document: TextDocumentIdentifier {
                    uri: Url::parse(uri).unwrap(),
                },
                text: None,
            })
            .await;

        assert_eq!(
            state.debounce_handles.lock().get(uri).map(|task| task.id),
            Some(7),
            "didSave must not replace a validation already covering this buffer"
        );
        pending.abort();
    }

    #[test]
    fn document_store_enforces_the_per_document_boundary() {
        let store = DocumentStore::new();

        assert_eq!(
            store.replacement_total(0, MAX_DOCUMENT_BYTES),
            Ok(MAX_DOCUMENT_BYTES)
        );
        assert_eq!(
            store.replacement_total(0, MAX_DOCUMENT_BYTES + 1),
            Err(DocumentRejection::TooLarge)
        );
    }

    #[test]
    fn document_store_enforces_the_aggregate_boundary() {
        let mut store = DocumentStore::new();
        store.retained_text_bytes = MAX_RETAINED_DOCUMENT_BYTES - 1;

        assert_eq!(
            store.replacement_total(0, 1),
            Ok(MAX_RETAINED_DOCUMENT_BYTES)
        );
        assert_eq!(
            store.replacement_total(0, 2),
            Err(DocumentRejection::RetainedTextLimit)
        );
    }

    #[test]
    fn test_loc_overlay_write_invalidates_the_cached_key_sets() {
        // #87 caches both overlay-derived key sets on `loc_overlay_revision`, so a write must
        // change what the next read sees, or a key just typed reads as undefined (CW225/CW122).
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let (service, _socket) = LspService::build(|client| Backend {
                client,
                state: Arc::new(DocumentState::new()),
            })
            .finish();
            let backend = service.inner();
            assert!(backend.loc_overlay_keys().is_empty());

            backend
                .loc_live_overlay_mut()
                .insert("file:///a.yml".into(), HashSet::from(["live_key".into()]));
            assert!(
                backend.loc_overlay_keys().contains("live_key"),
                "an open-doc overlay write must invalidate the cached union"
            );

            backend.loc_watched_overlay_mut().insert(
                "file:///b.yml".into(),
                HashSet::from(["watched_key".into()]),
            );
            let keys = backend.loc_overlay_keys();
            assert!(
                keys.contains("live_key") && keys.contains("watched_key"),
                "a watched overlay write must invalidate it too, got: {keys:?}"
            );

            backend.loc_live_overlay_mut().remove("file:///a.yml");
            assert!(
                !backend.loc_overlay_keys().contains("live_key"),
                "a removal must invalidate it as well, not just an insert"
            );
        });
    }

    #[test]
    fn test_unchanged_loc_key_set_keeps_the_cached_union() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let (service, _socket) = LspService::build(|client| Backend {
                client,
                state: Arc::new(DocumentState::new()),
            })
            .finish();
            let backend = service.inner();
            let (uri, path) = ("file:///a_l_english.yml", "a_l_english.yml");

            let changed =
                backend.record_watched_loc_keys(uri, path, "l_english:\n MY_KEY:0 \"one\"\n");
            assert!(changed.contains("my_key"), "got: {changed:?}");
            let first = backend.loc_overlay_keys();

            let changed =
                backend.record_watched_loc_keys(uri, path, "l_english:\n MY_KEY:0 \"one two\"\n");
            assert!(changed.is_empty(), "got: {changed:?}");
            let second = backend.loc_overlay_keys();
            assert!(
                Arc::ptr_eq(&first, &second),
                "an unchanged key set must leave the cached union in place"
            );

            let changed = backend.record_watched_loc_keys(
                uri,
                path,
                "l_english:\n MY_KEY:0 \"one\"\n OTHER_KEY:0 \"two\"\n",
            );
            assert!(changed.contains("other_key"), "got: {changed:?}");
            let third = backend.loc_overlay_keys();
            assert!(!Arc::ptr_eq(&second, &third));
            assert!(third.contains("other_key"), "got: {third:?}");
        });
    }

    #[test]
    fn test_did_focus_file_marks_activity() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let (service, _socket) = LspService::build(|client| Backend {
                client,
                state: Arc::new(DocumentState::new()),
            })
            .finish();
            let backend = service.inner();
            backend
                .state
                .last_activity_ms
                .store(u64::MAX, Ordering::Relaxed);
            backend.on_did_focus_file(Value::Null).await;
            assert_ne!(
                backend.state.last_activity_ms.load(Ordering::Relaxed),
                u64::MAX,
                "didFocusFile must reset the background-reindex idle clock"
            );
        });
    }
}
