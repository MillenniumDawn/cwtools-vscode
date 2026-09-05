// Process entrypoint for the cwtools-server binary. The `Backend`
// implementation and the `LanguageServer` trait dispatch live in `server.rs`
// so the Rust coverage gate can measure them while keeping this file's
// thin `fn main` excluded (#662). Notification type aliases are re-exported
// here so sibling modules can keep their `crate::LoadingBar` /
// `crate::UpdateFileList` paths.

use std::sync::Arc;

use tower_lsp::{LspService, Server};

mod access;
mod cache_purge;
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
mod navigation;
mod paths;
mod scan;
mod semantic;
mod server;
mod state;
mod transport;
mod validate;

pub(crate) use cursor::{RuleCursorInfo, hint_from_rule_right};
pub(crate) use server::{LoadingBar, UpdateFileList};
pub(crate) use state::{
    AstSource, Backend, CompletionCacheEntry, DeferredRulesMessage, DocumentState,
    FileTextSnapshot, FixableEdits, LocLocationMap, LocTextMap, ParsedDoc, SemanticCacheEntry,
    ValidateTrigger,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("cwtools-server {}", env!("CARGO_PKG_VERSION"));
        eprintln!();
        eprintln!("CWTools language server for Paradox game scripts.");
        eprintln!("Communicates over stdin/stdout using the Language Server Protocol.");
        eprintln!();
        eprintln!("USAGE:");
        eprintln!("    cwtools-server              Start the LSP server (default)");
        eprintln!("    cwtools-server --help       Show this help");
        eprintln!("    cwtools-server --version    Show version");
        std::process::exit(0);
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("cwtools-server {}", env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }

    cwtools_profiling::init_tracing();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
        .block_on(async {
            let state = Arc::new(DocumentState::new());
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
            Server::new(stdin, stdout, socket).serve(service).await;
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
    use cwtools_parser::parser::parse_string;
    use cwtools_string_table::string_table::StringTable;

    /// generated edit write into the user's game install (#160).
    #[test]
    fn refresh_roots_keeps_read_only_roots_out_of_the_edit_boundary() {
        let ws = tempfile::TempDir::new().expect("tmpdir");
        let vanilla = tempfile::TempDir::new().expect("tmpdir");
        let rules = tempfile::TempDir::new().expect("tmpdir");
        let canonical = |dir: &std::path::Path| std::fs::canonicalize(dir).expect("canonical");

        let mut cfg = Config::new();
        cfg.workspace_roots = vec![ws.path().to_path_buf()];
        cfg.vanilla_dir = Some(vanilla.path().to_path_buf());
        cfg.rules_dir = Some(rules.path().to_path_buf());
        cfg.refresh_roots();

        assert_eq!(cfg.editable_roots.as_ref(), [canonical(ws.path())]);
        for root in [ws.path(), vanilla.path(), rules.path()] {
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

    fn loc_document_cache(version: i32, retained_bytes: usize) -> Arc<LocDocumentCache> {
        Arc::new(LocDocumentCache {
            version,
            retained_bytes,
            files: Vec::new(),
            references: HashSet::new(),
        })
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
        assert_eq!(store.retained_text_bytes, "replacement".len());

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
        assert_eq!(store.retained_text_bytes, "text".len() + 10);
        assert_eq!(
            store.set_loc_cache("file:///doc", 1, loc_document_cache(1, MAX_DOCUMENT_BYTES)),
            Err(DocumentRejection::TooLarge)
        );
        store
            .open("file:///cached".to_string(), document("z"))
            .unwrap();
        assert_eq!(
            store.set_loc_cache("file:///cached", 1, loc_document_cache(1, 10)),
            Ok(true)
        );
        assert_eq!(store.retained_text_bytes, "text".len() + 10 + 1 + 10);
        store.remove("file:///cached");
        assert_eq!(store.retained_text_bytes, "text".len() + 10);

        store.change("file:///doc", 2, Arc::from("x")).unwrap();
        assert_eq!(store.retained_text_bytes, 1);
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

        assert_eq!(store.retained_text_bytes, source.len());
    }

    #[test]
    fn workspace_change_during_admission_rechecks_authorization() {
        let state = DocumentState::new();
        let mut checks = 0;

        let result =
            state.open_workspace_document("file:///doc".to_string(), document("text"), || {
                checks += 1;
                if checks == 1 {
                    state
                        .workspace_roots_generation
                        .fetch_add(1, Ordering::Release);
                    true
                } else {
                    false
                }
            });

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

        let state = Arc::new(DocumentState::new());
        state
            .code_lens_refresh_support
            .store(true, Ordering::Relaxed);
        let captured = Arc::new(Mutex::new(None));
        let slot = captured.clone();
        let st = state.clone();
        let (_service, mut socket) = LspService::new(move |client| {
            *slot.lock() = Some(client.clone());
            Backend {
                client,
                state: st.clone(),
            }
        });
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
        let state = Arc::new(DocumentState::new());
        let captured_client = Arc::new(Mutex::new(None));
        let client_slot = captured_client.clone();
        let server_state = state.clone();
        let (_service, _socket) = LspService::new(move |client| {
            *client_slot.lock() = Some(client.clone());
            Backend {
                client,
                state: server_state.clone(),
            }
        });
        let backend = Backend {
            client: captured_client.lock().take().unwrap(),
            state: state.clone(),
        };

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
        let state = Arc::new(DocumentState::new());
        let captured_client = Arc::new(Mutex::new(None));
        let client_slot = captured_client.clone();
        let server_state = state.clone();
        let (_service, _socket) = LspService::new(move |client| {
            *client_slot.lock() = Some(client.clone());
            Backend {
                client,
                state: server_state.clone(),
            }
        });
        let backend = Backend {
            client: captured_client.lock().take().unwrap(),
            state: state.clone(),
        };
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
        let state = Arc::new(DocumentState::new());
        let captured_client = Arc::new(Mutex::new(None));
        let client_slot = captured_client.clone();
        let server_state = state.clone();
        let (_service, _socket) = LspService::new(move |client| {
            *client_slot.lock() = Some(client.clone());
            Backend {
                client,
                state: server_state.clone(),
            }
        });
        let backend = Backend {
            client: captured_client.lock().take().unwrap(),
            state: state.clone(),
        };
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
        // #87 caches both overlay-derived key sets. The cache is keyed on
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
