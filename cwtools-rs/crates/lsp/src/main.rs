use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{LanguageServer, LspService, Server};

use cwtools_info::{PositionElement, ReferenceHint};
use cwtools_parser::ast::ParsedFile;
use cwtools_validation::position::rules_at_pos;

mod access;
mod cache_purge;
mod code_action;
mod color;
mod command_progress;
mod completion;
mod config;
mod cursor;
mod documentlink;
mod graph;
mod hover;
mod inlay;
mod navigation;
mod paths;
mod scan;
mod semantic;
mod state;
mod transport;
mod validate;

pub(crate) use cursor::{CursorResolution, RuleCursorInfo, hint_from_rule_right};
use cursor::{hint_from_rule_left, scope_link_key_type};
use paths::{canonicalize_uri_string, canonicalize_url};
pub(crate) use state::{
    AstSnapshot, AstSource, Backend, CompletionCacheEntry, DebounceTask, DeferredRulesMessage,
    DiskState, DocumentState, FileTextSnapshot, FixableEdits, LocLocationMap, LocOverlayWrite,
    LocTextMap, MAX_DOCUMENT_BYTES, MAX_DOCUMENT_URI_BYTES, ParsedDoc, SemanticCacheEntry,
    ValidateTrigger, remove_debounce_task,
};

// ── Custom LSP notification types ─────────────────────────────────────────────

/// `loadingBar` server→client notification (S→C).
/// Payload: `{ "enable": bool, "value": string }`.
/// Used to drive the extension's status-bar progress indicator.
enum LoadingBar {}
impl tower_lsp::lsp_types::notification::Notification for LoadingBar {
    type Params = serde_json::Value;
    const METHOD: &'static str = "loadingBar";
}

/// `updateFileList` server→client notification (S→C).
/// Payload: `{ "fileList": [{ "scope": string, "uri": string, "logicalpath": string }] }`.
/// Used to populate the extension's file explorer tree view.
enum UpdateFileList {}
impl tower_lsp::lsp_types::notification::Notification for UpdateFileList {
    type Params = serde_json::Value;
    const METHOD: &'static str = "updateFileList";
}

/// Debounce window for `did_change`: a burst of keystrokes within this window
/// coalesces into a single validation. Short enough to feel live, long enough
/// to skip the per-keystroke re-parse that made large files lag.
const DEBOUNCE_MS: u64 = 250;

/// Test-only one-shot panic switch for `CWTOOLS_VALIDATE_PANIC_ONCE` (#182),
/// the debounced-validation counterpart of the scan-side switches. Fires at
/// most once per server process so the e2e suite can prove a panicking
/// validation is logged without arming every later edit.
static VALIDATE_PANIC_ONCE: AtomicBool = AtomicBool::new(true);

// ── Custom notifications ──────────────────────────────────────────────────────

// Code actions live in `code_action.rs` (QUICKFIX from SuggestedFix); the
// techGraph / event-graph data is in `graph.rs` behind `getGraphData`.
//
// Not implemented: `getEmbeddedMetadata`, a per-file metadata bundle pushed to
// the extension on open. Nothing in cwtools-vscode asks for it, so it stays
// unbuilt until the extension side wants it.

impl Backend {
    /// Spawn a background validation for `uri` at `version` and register the
    /// task in `debounce_handles`, aborting any predecessor for the same URI
    /// (`did_close` aborts it too). `delay_ms` is the debounce sleep before
    /// validating: `did_change` passes `DEBOUNCE_MS` to coalesce keystrokes,
    /// open/save pass 0 to validate promptly. The task re-reads a
    /// version-checked snapshot inside `debounced_validate`, so a newer edit
    /// landing in the gap supersedes this one instead of publishing stale
    /// results.
    ///
    /// The task that joins the handle logs a panic instead of dropping it with
    /// the handle (#182); the next edit is the retry. This site keeps its own
    /// join rather than `scan::spawn_logging_panics` because it needs the
    /// `AbortHandle` and must not report an ordinary supersede/close abort as
    /// a panic.
    fn spawn_debounced_validate(
        &self,
        uri: String,
        version: i32,
        generation: u64,
        trigger: ValidateTrigger,
        delay_ms: u64,
    ) {
        let id = self.state.next_debounce_id.fetch_add(1, Ordering::Relaxed);
        let client = self.client.clone();
        let state = self.state.clone();
        let key = uri.clone();
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            if start_rx.await.is_err() {
                return;
            }
            if crate::scan::env_flag("CWTOOLS_VALIDATE_PANIC_ONCE")
                && VALIDATE_PANIC_ONCE.swap(false, Ordering::SeqCst)
            {
                panic!("CWTOOLS_VALIDATE_PANIC_ONCE: injected panic for #182 test coverage");
            }
            if delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            Backend { client, state }
                .debounced_validate(uri, version, generation, trigger)
                .await;
        });
        let abort = handle.abort_handle();
        let cleanup_state = self.state.clone();
        let cleanup_key = key.clone();
        tokio::spawn(async move {
            match handle.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {}
                Err(error) => {
                    tracing::error!(%error, uri = %cleanup_key, "document validation task panicked")
                }
            }
            let mut tasks = cleanup_state.debounce_handles.lock();
            remove_debounce_task(&mut tasks, &cleanup_key, id);
            drop(tasks);
            let _ = finished_tx.send(());
        });
        let previous = self.state.debounce_handles.lock().insert(
            key,
            DebounceTask {
                id,
                abort,
                finished: finished_rx,
            },
        );
        let _ = start_tx.send(());
        if let Some(previous) = previous {
            previous.abort.abort();
        }
    }

    /// Bump the info-revision counter. Called from every site that mutates
    /// `info_service` or `rules` (the two state sources the loc/fallback
    /// completion caches depend on), so the completion cache invalidates
    /// exactly when the inputs change. `Relaxed` is enough — the only
    /// consumer is a single-threaded `load` that tolerates missing an
    /// in-flight bump (the next request picks it up).
    pub(crate) fn bump_info_revision(&self) {
        self.state
            .info_revision
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Write access to the open-document loc overlay. See [`LocOverlayWrite`]:
    /// the revision bump the derived caches key on rides on the guard's `Drop`.
    pub(crate) fn loc_live_overlay_mut(&self) -> LocOverlayWrite<'_> {
        LocOverlayWrite {
            guard: self.state.loc_live_overlay.write(),
            revision: &self.state.loc_overlay_revision,
        }
    }

    /// Write access to the watched-file loc overlay. See [`LocOverlayWrite`].
    pub(crate) fn loc_watched_overlay_mut(&self) -> LocOverlayWrite<'_> {
        LocOverlayWrite {
            guard: self.state.loc_watched_overlay.write(),
            revision: &self.state.loc_overlay_revision,
        }
    }

    pub(crate) fn invalidate_semantic_tokens(&self, uri: &str) {
        self.state.semantic_tokens_cache.lock().remove(uri);
    }

    pub(crate) fn invalidate_all_semantic_tokens(&self) {
        self.state.semantic_tokens_cache.lock().clear();
    }

    pub(crate) async fn request_semantic_refresh(&self) {
        if !self
            .state
            .semantic_tokens_refresh_support
            .load(Ordering::Relaxed)
        {
            return;
        }
        // `workspace/semanticTokens/refresh` tells the client to re-request tokens
        // for visible editors. Ignore errors from clients that advertised support
        // but reject the request.
        let _ = self.client.semantic_tokens_refresh().await;
    }

    /// Called when the VS Code extension tells us the user switched to a file.
    /// Focus is user activity: reset the background-reindex idle clock.
    async fn on_did_focus_file(&self, _params: Value) {
        self.mark_activity();
    }

    /// Resolve the leaf under the cursor with the position resolver and
    /// classify it: the AST element, a [`ReferenceHint`] derived from the
    /// matched rule's right-hand side, the alias category the key resolves
    /// through (trigger/effect/…), and the matched rule's description +
    /// required scopes (for hover).
    ///
    /// Shared by hover, goto_definition, references, prepare_rename, and
    /// rename. Returns `None` when the cursor isn't on a leaf inside a known
    /// entity — callers fall back to `element_at_position`.
    pub(crate) fn rule_info_at_cursor(
        &self,
        uri: &str,
        pos: tower_lsp::lsp_types::Position,
        logical_path: &str,
    ) -> Option<RuleCursorInfo> {
        let CursorResolution { rctx, ruleset, .. } =
            self.resolve_at_cursor(uri, pos, logical_path)?;
        let rs = ruleset.as_ref();
        let leaf = rctx.leaf?;

        let element = if leaf.key.is_empty() {
            PositionElement::LeafValue {
                value: leaf.value.clone(),
            }
        } else {
            PositionElement::Leaf {
                key: leaf.key.clone(),
                value: leaf.value.clone(),
            }
        };

        let mut hint = ReferenceHint::Unknown;
        let mut description: Option<String> = None;
        let mut scopes: Vec<String> = Vec::new();
        for (rule_type, opts) in &rctx.value_rules {
            if description.is_none() && opts.description.is_some() {
                description = opts.description.clone();
            }
            if scopes.is_empty() && !opts.required_scopes.is_empty() {
                scopes = opts.required_scopes.clone();
            }
            if matches!(hint, ReferenceHint::Unknown) {
                hint = hint_from_rule_right(rule_type, &leaf.value, rs);
            }
        }
        // Key-position references: when the right-hand classification yields
        // nothing and the cursor is on a key (e.g. `<character> = { … }` used as
        // a scoped-trigger block, or a `type[…]` definition key), classify the
        // key against the matched rule's LEFT field so hover renders a rich
        // header and goto can resolve the definition.
        if matches!(hint, ReferenceHint::Unknown) && !leaf.key.is_empty() {
            for (rule_type, _) in &rctx.value_rules {
                let left_hint = hint_from_rule_left(rule_type, &leaf.key);
                if !matches!(left_hint, ReferenceHint::Unknown) {
                    hint = left_hint;
                    break;
                }
            }
        }
        // Scope-link key: a bare key that is a known instance of a type used as a
        // link `data_source` (e.g. a character name scoping into that character).
        // Such keys don't match a rule, so `value_rules` is empty and any
        // description that did match comes from a coincidental alias — resolve the
        // key to its type and drop the misleading description/category.
        let mut scope_link_key = false;
        let info_guard = self.state.info_service.read();
        if !leaf.in_value
            && !leaf.key.is_empty()
            && !matches!(hint, ReferenceHint::TypeRef { .. })
            && let Some(type_name) = scope_link_key_type(rs, &info_guard.type_index, &leaf.key)
        {
            hint = ReferenceHint::TypeRef {
                type_name,
                value: leaf.key.clone(),
            };
            description = None;
            scope_link_key = true;
        }
        let category = if leaf.key.is_empty() || scope_link_key {
            None
        } else {
            cwtools_validation::position::alias_category_for_key(
                rs,
                Some(&info_guard.type_index),
                &rctx.child_rules,
                &leaf.key,
            )
        };
        drop(info_guard);
        // Current scope at the cursor (the scope the containing block evaluates
        // in), so a hover shows where you are regardless of whether the rule
        // declares a required scope. The related scopes (ROOT/PREV and the FROM
        // chain) come along for the hover scope table. In every case suppress the
        // wildcards (`any`/`invalid`) and the unnamed-scope fallback (`scope_N`,
        // when no config scope is loaded): showing those is noise.
        let resolve_scope = |sc: &cwtools_game::scope_engine::ScopeContext,
                             id: cwtools_game::ScopeId| {
            let name = sc.registry.name_of(id);
            let placeholder = name == "any"
                || name == "invalid"
                || name.strip_prefix("scope_").and_then(|s| s.parse().ok()) == Some(id.0);
            (!placeholder).then_some(name)
        };
        let (current_scope, root_scope, prev_scope, from_scopes) = match rctx.scope.as_ref() {
            Some(sc) => {
                let current = resolve_scope(sc, sc.current());
                let root = resolve_scope(sc, sc.root);
                // PREV is the scope one level out: the second-from-top of the stack.
                let prev = (sc.scopes.len() >= 2)
                    .then(|| sc.scopes[sc.scopes.len() - 2])
                    .and_then(|id| resolve_scope(sc, id));
                // FROM chain: [0] = FROM, [1] = FROM.FROM, …; drop placeholders.
                let from = sc
                    .from
                    .iter()
                    .filter_map(|id| resolve_scope(sc, *id))
                    .collect();
                (current, root, prev, from)
            }
            None => (None, None, None, Vec::new()),
        };
        // The scope the hovered key resolves TO: run it through `change_scope` on
        // a clone of the cursor's context. For a scope-changing link (`owner`) or
        // a meta keyword (`FROM`/`ROOT`/`PREV`) this is the target scope; for
        // anything that doesn't change scope it stays the ambient one (and is
        // suppressed at display when it matches). Only computed when the
        // `hover.scopeDisplay = "resolved"` setting is on. (#37)
        let resolved_scope = self
            .state
            .hover_resolved_scope
            .load(Ordering::Relaxed)
            .then(|| match (rctx.scope.as_ref(), &element) {
                (Some(sc), PositionElement::Leaf { key, .. }) if !key.is_empty() => {
                    let mut probe = sc.clone();
                    probe.change_scope(key);
                    resolve_scope(&probe, probe.current())
                }
                _ => None,
            })
            .flatten();
        Some(RuleCursorInfo {
            element,
            hint,
            category,
            description,
            required_scopes: scopes,
            current_scope,
            root_scope,
            prev_scope,
            from_scopes,
            resolved_scope,
        })
    }
}

impl Backend {
    /// Snapshot the document AST for `uri`, plus whether it came from the
    /// current document version. When there is no cached AST, re-parse the live
    /// text so hover/goto/completion still resolve a context mid-edit. The fresh
    /// AST is not written back to the document. The debounced validate owns the
    /// long-term one. The `documents` mutex is held only for the snapshot, never
    /// across the parse.
    ///
    /// The fresh parse is memoized by `(uri, version)` in `fresh_ast_cache`
    /// (#87): a document has no stored AST from `did_open` until its first
    /// successful validate, and every feature request landing in that window
    /// used to re-parse the whole file for itself.
    pub(crate) fn ast_snapshot_for(&self, uri: &str) -> Option<AstSnapshot> {
        let (text, version) = {
            let docs = self.state.documents.lock();
            let doc = docs.get(uri)?;
            if let Some(ast) = &doc.ast {
                let source = if doc.ast_version == Some(doc.version) {
                    AstSource::StoredCurrent
                } else {
                    AstSource::StoredStale
                };
                return Some(AstSnapshot {
                    ast: ast.clone(),
                    source,
                });
            }
            (doc.text.clone(), doc.version)
        };
        let cached = {
            let guard = self.state.fresh_ast_cache.lock();
            guard
                .as_ref()
                .filter(|(cached_uri, cached_version, _)| {
                    cached_uri == uri && *cached_version == version
                })
                .map(|(_, _, ast)| Arc::clone(ast))
        };
        if let Some(ast) = cached {
            return Some(AstSnapshot {
                ast,
                source: AstSource::FreshParse,
            });
        }
        let table = self.state.string_table.clone();
        tokio::task::block_in_place(|| {
            let ast = Arc::new(cwtools_parser::parser::parse_string(&text, &table));
            *self.state.fresh_ast_cache.lock() = Some((uri.to_string(), version, Arc::clone(&ast)));
            Some(AstSnapshot {
                ast,
                source: AstSource::FreshParse,
            })
        })
    }

    /// Snapshot the document AST for `uri`, preserving the existing behavior for
    /// hover/goto callers that do not need freshness metadata.
    pub(crate) fn ast_for(&self, uri: &str) -> Option<Arc<ParsedFile>> {
        self.ast_snapshot_for(uri).map(|snapshot| snapshot.ast)
    }

    /// The classified element under the cursor via `element_at_position`, run on
    /// the snapshotted AST (with the mid-edit re-parse fallback from `ast_for`).
    /// Shared by hover and goto's heuristic fallbacks.
    pub(crate) fn element_at_cursor(
        &self,
        uri: &str,
        pos: tower_lsp::lsp_types::Position,
    ) -> Option<PositionElement> {
        let ast = self.ast_for(uri)?;
        let text = {
            let docs = self.state.documents.lock();
            docs.get(uri).map(|doc| doc.text.clone())
        }?;
        let position_encoding = self.state.config.read().position_encoding.clone();
        let (line, col) = crate::paths::lsp_pos_to_source_in_text(&text, pos, &position_encoding);
        cwtools_info::element_at_position(&ast, line, col, &self.state.string_table)
    }

    /// Resolve the rule context at the cursor, snapshotting the AST and ruleset
    /// so neither the `documents` mutex nor the rules guard is held across
    /// `rules_at_pos`. Shared by completion and `rule_info_at_cursor` (hover /
    /// goto). `RuleContext` is owned, so all guards are released on return.
    pub(crate) fn resolve_at_cursor(
        &self,
        uri: &str,
        pos: tower_lsp::lsp_types::Position,
        logical_path: &str,
    ) -> Option<CursorResolution> {
        let (game, scope_checks, var_checks, position_encoding) = {
            let cfg = self.state.config.read();
            (
                cfg.game(),
                cfg.scope_checks,
                cfg.var_checks,
                cfg.position_encoding.clone(),
            )
        };
        let ast = self.ast_for(uri)?;
        let (ruleset, modifier_keys, scope_registry) = {
            let rules_guard = self.state.rules.read();
            (
                rules_guard.ruleset.clone()?,
                rules_guard.modifier_keys.clone(),
                rules_guard.scope_registry.clone(),
            )
        };
        let document_text = {
            let docs = self.state.documents.lock();
            docs.get(uri).map(|doc| Arc::clone(&doc.text))
        };
        let (line, col) = document_text.as_deref().map_or_else(
            || crate::paths::lsp_pos_to_source(pos),
            |text| crate::paths::lsp_pos_to_source_in_text(text, pos, &position_encoding),
        );
        // info_service read is held only for the resolve; `rules_at_pos` returns
        // owned data, so it is dropped before the caller runs.
        let info_guard = self.state.info_service.read();
        let prepared = crate::validate::make_prepared(
            &ruleset,
            &self.state.string_table,
            game,
            &info_guard.type_index,
            &modifier_keys,
            None,
            None,
            scope_registry.as_ref(),
            scope_checks,
            var_checks,
        );
        let rctx = rules_at_pos(&ast, logical_path, &prepared, line, col, false)?;
        drop(info_guard);
        Some(CursorResolution { rctx, ruleset })
    }

    /// The `$KEY$` loc reference under the cursor in an open `.yml` document, plus
    /// its `[start, end)` range in the negotiated position encoding. `None` when
    /// the cursor isn't on a reference (or the document isn't open). Shared by
    /// hover and goto.
    pub(crate) fn loc_ref_at_cursor_doc(
        &self,
        uri: &str,
        pos: tower_lsp::lsp_types::Position,
    ) -> Option<(String, u32, u32)> {
        let position_encoding = self.state.config.read().position_encoding.clone();
        let docs = self.state.documents.lock();
        let doc = docs.get(uri)?;
        let line = doc.text.lines().nth(pos.line as usize)?;
        crate::paths::loc_ref_at_cursor_with_encoding(line, pos.character, &position_encoding)
    }
}

// CursorResolution / RuleCursorInfo / hint helpers live in `cursor`.

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        self.initialize_impl(params).await
    }

    async fn initialized(&self, _params: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "CWTools server initialized!")
            .await;

        // Notifications are only forwarded from here on.
        self.state
            .handshake_complete
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let deferred = std::mem::take(&mut *self.state.deferred_rule_diagnostics.lock());
        for (uri, diags) in deferred {
            if let Ok(url) = uri.parse() {
                self.client.publish_diagnostics(url, diags, None).await;
            }
        }
        let deferred_msgs = std::mem::take(&mut *self.state.deferred_rules_messages.lock());
        for msg in deferred_msgs {
            match msg {
                DeferredRulesMessage::Log(text) => {
                    self.client.log_message(MessageType::ERROR, text).await;
                }
                DeferredRulesMessage::Toast(text) => {
                    self.client.show_message(MessageType::ERROR, text).await;
                }
            }
        }

        // Workspace-wide initial validation spawned in background so the LSP
        // handshake returns promptly.
        let client = self.client.clone();
        let state = self.state.clone();
        let watch_state = self.state.clone();
        let handle = tokio::spawn(async move {
            let backend = Backend { client, state };
            backend.validate_entire_workspace(false).await;
        });
        // Log if the workspace scan panics — without this, a panic is silently
        // swallowed (the JoinHandle is dropped) and the server runs in a
        // degraded state with no diagnostics.
        tokio::spawn(async move {
            if let Err(e) = handle.await {
                tracing::error!("validate_entire_workspace panicked: {}", e);
                // The scan didn't reach the point where it flips index_ready, so
                // diagnostics would stay suppressed forever. Release the gate so
                // per-file validation still publishes (degraded but not silent).
                watch_state
                    .index_ready
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                // The bar-off the panic skipped is `ScanGuard`'s job — it
                // unwinds with the scan and closes both progress channels.
            }
        });

        // Periodic quiet re-scan so a long-running session doesn't accumulate
        // stale index state. Off by default (background_reindex_interval_minutes
        // == 0); runs only while the user is idle, and every notification the
        // scan would normally send to the status bar is suppressed.
        let reindex_client = self.client.clone();
        let reindex_state = self.state.clone();
        tokio::spawn(async move {
            // Each pass is already panic-safe on its own
            // (`crate::scan::Backend::run_reindex_pass`); this additionally
            // wraps the loop's own task, so a panic in its scaffolding
            // (interval/idle-window bookkeeping) is logged instead of
            // silently ending periodic reindexing with no trace (#155).
            // Log-only — unlike the per-pass wrapper, there's no single bad
            // pass to retry here, just the loop itself dying.
            crate::scan::spawn_logging_panics("background reindex loop", async move {
                Backend {
                    client: reindex_client,
                    state: reindex_state,
                }
                .background_reindex_loop()
                .await;
            })
            .await;
        });
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        self.did_change_configuration_impl(params).await
    }

    // --- Text document sync ---
    #[tracing::instrument(skip_all)]
    async fn did_open(&self, mut params: DidOpenTextDocumentParams) {
        self.mark_activity();
        canonicalize_url(&mut params.text_document.uri);
        let uri = params.text_document.uri.to_string();
        let text = params.text_document.text;
        let version = params.text_document.version;
        tracing::debug!(%uri, version, bytes = text.len(), "did_open");

        if uri.len() > MAX_DOCUMENT_URI_BYTES {
            tracing::warn!(bytes = uri.len(), "ignoring didOpen with an oversized URI");
            return;
        }
        if text.len() > MAX_DOCUMENT_BYTES {
            tracing::warn!(%uri, bytes = text.len(), "ignoring oversized didOpen document");
            return;
        }
        let text: Arc<str> = Arc::from(text);
        let admission = self.state.open_workspace_document(
            uri.clone(),
            ParsedDoc {
                version,
                text,
                ast: None,
                ast_version: None,
                ast_source_bytes: 0,
            },
            || self.is_workspace_document(&uri),
        );
        if let Err(rejection) = admission {
            tracing::warn!(%uri, reason = rejection.reason(), "ignoring didOpen");
            return;
        }

        // The open overlay owns this file's keys from here; a stale watched
        // entry left behind could resurrect keys the buffer removed.
        if crate::paths::is_loc_file(&uri) {
            self.loc_watched_overlay_mut().remove(&uri);
        }

        if self.is_ignored_uri(&uri) {
            self.clear_ignored_file_state(&uri);
            self.update_doc_tokens(&uri, None);
            self.invalidate_semantic_tokens(&uri);
            if let Ok(url) = Url::parse(&uri) {
                self.publish_filtered(url, Vec::new(), Some(version), None)
                    .await;
            }
            return;
        }

        // Offload validation off the message future so a burst of opens can't
        // hold the bounded request queue (#90). `debounced_validate`'s
        // export-diff-gated dependent sweep replaces the old inline sweep here:
        // opening a file whose exports match what's already indexed skips the
        // sweep entirely, and a changed export refreshes only real dependents.
        // Bump the edit counter so that sweep is tagged and a later edit
        // supersedes it.
        let generation = self.state.edit_generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.invalidate_semantic_tokens(&uri);
        self.spawn_debounced_validate(uri, version, generation, ValidateTrigger::DidOpen, 0);
    }

    #[tracing::instrument(skip_all)]
    async fn did_change(&self, mut params: DidChangeTextDocumentParams) {
        self.mark_activity();
        canonicalize_url(&mut params.text_document.uri);
        let uri = params.text_document.uri.to_string();
        let version = params.text_document.version;
        if uri.len() > MAX_DOCUMENT_URI_BYTES {
            tracing::warn!(
                bytes = uri.len(),
                "ignoring didChange with an oversized URI"
            );
            return;
        }

        // FULL-sync spec requires last-wins; use the last change in the batch.
        let Some(change) = params.content_changes.into_iter().last() else {
            return;
        };
        let text = change.text;
        tracing::debug!(%uri, version, bytes = text.len(), "did_change");
        if text.len() > MAX_DOCUMENT_BYTES {
            tracing::warn!(%uri, bytes = text.len(), "ignoring oversized didChange document");
            return;
        }

        // Store the new text+version immediately (keep the prior AST until we
        // revalidate). The debounced task checks the version to know whether this
        // is still the latest edit. An unsolicited didChange cannot create a new
        // retained document.
        let admission = self
            .state
            .documents
            .lock()
            .change(&uri, version, Arc::from(text));
        if let Err(rejection) = admission {
            tracing::warn!(%uri, reason = rejection.reason(), "ignoring didChange");
            return;
        }

        if self.is_ignored_uri(&uri) {
            self.clear_ignored_file_state(&uri);
            self.update_doc_tokens(&uri, None);
            self.invalidate_semantic_tokens(&uri);
            if let Ok(url) = Url::parse(&uri) {
                self.publish_filtered(url, Vec::new(), Some(version), None)
                    .await;
            }
            return;
        }

        // Bump the global edit counter so any in-flight dependent sweep from an
        // earlier edit knows it has been superseded and can stop early.
        let generation = self.state.edit_generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.invalidate_semantic_tokens(&uri);

        // Validate in the background after a short debounce so a burst of
        // keystrokes coalesces into one validation and the handler returns
        // immediately (no per-keystroke re-parse lag). The helper aborts any
        // pending sleeper for this file so a burst can't stack hundreds of
        // debounce tasks (#47).
        self.spawn_debounced_validate(
            uri,
            version,
            generation,
            ValidateTrigger::DidChange,
            DEBOUNCE_MS,
        );
    }

    async fn did_save(&self, mut params: DidSaveTextDocumentParams) {
        canonicalize_url(&mut params.text_document.uri);
        let uri = params.text_document.uri.to_string();
        self.invalidate_semantic_tokens(&uri);
        if self.is_ignored_uri(&uri) {
            let version = {
                let docs = self.state.documents.lock();
                docs.get(&uri).map(|d| d.version)
            };
            self.clear_ignored_file_state(&uri);
            self.update_doc_tokens(&uri, None);
            if let (Some(version), Ok(url)) = (version, Url::parse(&uri)) {
                self.publish_filtered(url, Vec::new(), Some(version), None)
                    .await;
            } else if let Ok(url) = Url::parse(&uri) {
                self.publish_filtered(url, Vec::new(), None, None).await;
            }
            return;
        }
        // A save isn't an edit, so don't bump the edit counter, just re-read the
        // current version and generation. Offload the validation like did_change
        // (#90); the entry version guard in `debounced_validate` makes a racing
        // did_change safe. The export-diff-gated dependent sweep also refreshes
        // callers when a save changed this file's exports.
        let Some(version) = ({
            let docs = self.state.documents.lock();
            docs.get(&uri).map(|d| d.version)
        }) else {
            return;
        };
        let generation = self.state.edit_generation.load(Ordering::Relaxed);
        self.spawn_debounced_validate(uri, version, generation, ValidateTrigger::DidSave, 0);
    }

    #[tracing::instrument(skip_all)]
    async fn did_close(&self, mut params: DidCloseTextDocumentParams) {
        canonicalize_url(&mut params.text_document.uri);
        let uri = params.text_document.uri.to_string();
        self.invalidate_semantic_tokens(&uri);
        tracing::debug!(%uri, "did_close");
        let Some(closed_doc) = self.state.documents.lock().remove(&uri) else {
            return;
        };
        let pending_validation = self.state.debounce_handles.lock().remove(&uri);
        if let Some(validation) = pending_validation {
            validation.abort.abort();
            let _ = validation.finished.await;
        }

        if self.state.documents.lock().contains_key(&uri) {
            return;
        }
        let Ok(_validation_permit) = self.state.validation_permits.acquire().await else {
            return;
        };

        let disk_loc_text = if crate::paths::is_loc_file(&uri) {
            let roots = self.state.config.read().authorized_roots.clone();
            let uri_for_read = uri.clone();
            tokio::task::spawn_blocking(move || {
                crate::access::read_authorized_text(
                    &uri_for_read,
                    &roots,
                    crate::access::MAX_URI_READ_BYTES,
                )
            })
            .await
            .ok()
            .flatten()
        } else {
            None
        };

        let (exports_before, names_before) = {
            let info = self.state.info_service.read();
            (info.export_fingerprint(&uri), info.export_names(&uri))
        };
        let disk_ast = if !crate::paths::has_loc_ext(&uri) && !crate::paths::is_cwt_file(&uri) {
            let roots = self.state.config.read().authorized_roots.clone();
            let table = self.state.string_table.clone();
            let uri = uri.clone();
            let buffer_text = Arc::clone(&closed_doc.text);
            tokio::task::spawn_blocking(move || {
                use crate::access::{FileRead, MAX_URI_READ_BYTES, read_authorized};
                match read_authorized(&uri, &roots, MAX_URI_READ_BYTES) {
                    FileRead::Text(text) => DiskState::Parsed {
                        parsed: cwtools_parser::parser::parse_string(&text, &table),
                        discarded_edits: text != *buffer_text,
                    },
                    FileRead::Missing | FileRead::Refused => DiskState::Absent,
                }
            })
            .await
            .unwrap_or(DiskState::Absent)
        } else {
            DiskState::Absent
        };

        let (exports_after, names_after, generation) = {
            let mut doc_tokens = self.state.doc_tokens.write();
            let documents = self.state.documents.lock();
            if documents.contains_key(&uri) {
                return;
            }

            match &disk_ast {
                DiskState::Parsed { parsed, .. } => self.index_parsed_file(&uri, parsed, None),
                DiskState::Absent => {
                    self.state.info_service.write().clear_file(&uri);
                    self.bump_info_revision();
                    // The file is gone from disk too, so its recorded `<type>` uses
                    // must not keep suppressing CW239 on the instances it referenced.
                    // Queue those names so the sweep below revalidates their
                    // definition files. (A file that still exists keeps its entry,
                    // refreshed from the disk AST below when the close discarded
                    // unsaved edits.)
                    if let Some(uses) = self.state.type_uses.write().remove(&uri) {
                        let dropped = uses.changed_names(&Default::default());
                        if !dropped.is_empty() {
                            self.state
                                .type_uses_revision
                                .fetch_add(1, Ordering::Release);
                            self.state
                                .pending_changed_names
                                .lock()
                                .extend(dropped.into_iter());
                        }
                    }
                }
            }
            doc_tokens.remove(&uri);
            self.loc_live_overlay_mut().remove(&uri);
            self.state.completion_generation.lock().remove(&uri);
            {
                let mut fresh = self.state.fresh_ast_cache.lock();
                if fresh.as_ref().is_some_and(|(cached, _, _)| cached == &uri) {
                    *fresh = None;
                }
            }

            let info = self.state.info_service.read();
            (
                info.export_fingerprint(&uri),
                info.export_names(&uri),
                self.state.edit_generation.fetch_add(1, Ordering::Relaxed) + 1,
            )
        };

        // A close that discarded unsaved edits leaves `type_uses` describing text
        // that never reached disk, and nothing else refreshes it: no watched event
        // fires for a file that didn't change. The index was just rebuilt from the
        // disk AST, so rebuild the recorded uses from it too, or a reference the
        // discard restored keeps a false CW239 on its definition until the next
        // full scan (#133). Queued names land in the sweep below.
        if let DiskState::Parsed {
            parsed,
            discarded_edits: true,
        } = &disk_ast
            && !self.state.documents.lock().contains_key(&uri)
        {
            // block_in_place: a whole-file validate, the same sync CPU work the
            // keystroke path fences (#87).
            tokio::task::block_in_place(|| self.refresh_type_uses_from_parsed(&uri, parsed));
        }

        cwtools_profiling::log_rss("did_close");
        if !self.state.documents.lock().contains_key(&uri) {
            self.publish_filtered(params.text_document.uri, vec![], None, None)
                .await;
            if let Some(text) = disk_loc_text
                && !self.state.documents.lock().contains_key(&uri)
            {
                let (diagnostics, _) = self
                    .parse_and_validate(&uri, &text, ValidateTrigger::DidClose, None)
                    .await;
                if !self.state.documents.lock().contains_key(&uri)
                    && let Ok(uri_obj) = Url::parse(&uri)
                {
                    self.publish_gated(
                        uri_obj,
                        diagnostics,
                        None,
                        Some(cwtools_cache::workspace::content_hash(&text)),
                    )
                    .await;
                }
            }
        }

        let mut changed_names: HashSet<String> = names_before
            .symmetric_difference(&names_after)
            .cloned()
            .collect();
        changed_names.extend(self.state.pending_changed_names.lock().drain());
        if exports_before != exports_after || !changed_names.is_empty() {
            self.revalidate_open_dependents(
                &uri,
                generation,
                (!changed_names.is_empty()).then_some(&changed_names),
            )
            .await;
        }
    }

    // --- Language features ---

    async fn hover(&self, mut params: HoverParams) -> Result<Option<Hover>> {
        self.mark_activity();
        canonicalize_url(&mut params.text_document_position_params.text_document.uri);
        self.hover_impl(params).await
    }

    async fn completion(&self, mut params: CompletionParams) -> Result<Option<CompletionResponse>> {
        canonicalize_url(&mut params.text_document_position.text_document.uri);
        let mut response = self.completion_impl(params).await;
        // Origin labels are stamped once here so every completion path (game
        // script, loc, .cwt) is covered, gated on the client capability.
        if self
            .state
            .completion_label_details
            .load(std::sync::atomic::Ordering::Relaxed)
            && let Ok(Some(resp)) = response.as_mut()
        {
            match resp {
                CompletionResponse::Array(items) => crate::completion::apply_label_details(items),
                CompletionResponse::List(list) => {
                    crate::completion::apply_label_details(&mut list.items)
                }
            }
        }
        response
    }

    async fn completion_resolve(&self, item: CompletionItem) -> Result<CompletionItem> {
        Ok(self.completion_resolve_impl(item))
    }

    async fn goto_definition(
        &self,
        mut params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        self.mark_activity();
        canonicalize_url(&mut params.text_document_position_params.text_document.uri);
        self.goto_definition_impl(params).await
    }

    async fn references(&self, mut params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        self.mark_activity();
        canonicalize_url(&mut params.text_document_position.text_document.uri);
        self.references_impl(params).await
    }

    async fn document_symbol(
        &self,
        mut params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        self.mark_activity();
        canonicalize_url(&mut params.text_document.uri);
        self.document_symbol_impl(params).await
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        self.mark_activity();
        self.symbol_impl(params).await
    }

    async fn folding_range(
        &self,
        mut params: FoldingRangeParams,
    ) -> Result<Option<Vec<FoldingRange>>> {
        canonicalize_url(&mut params.text_document.uri);
        self.folding_range_impl(params).await
    }

    async fn document_highlight(
        &self,
        mut params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        canonicalize_url(&mut params.text_document_position_params.text_document.uri);
        self.document_highlight_impl(params).await
    }

    async fn selection_range(
        &self,
        mut params: SelectionRangeParams,
    ) -> Result<Option<Vec<SelectionRange>>> {
        canonicalize_url(&mut params.text_document.uri);
        self.selection_range_impl(params).await
    }

    async fn document_link(
        &self,
        mut params: DocumentLinkParams,
    ) -> Result<Option<Vec<DocumentLink>>> {
        canonicalize_url(&mut params.text_document.uri);
        self.document_link_impl(params).await
    }

    async fn prepare_rename(
        &self,
        mut params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        canonicalize_url(&mut params.text_document.uri);
        self.prepare_rename_impl(params).await
    }

    async fn rename(&self, mut params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        canonicalize_url(&mut params.text_document_position.text_document.uri);
        self.rename_impl(params).await
    }

    async fn code_action(
        &self,
        mut params: CodeActionParams,
    ) -> Result<Option<CodeActionResponse>> {
        self.mark_activity();
        canonicalize_url(&mut params.text_document.uri);
        self.code_action_impl(params).await
    }

    async fn inlay_hint(&self, mut params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        canonicalize_url(&mut params.text_document.uri);
        self.inlay_hint_impl(params).await
    }

    async fn semantic_tokens_full(
        &self,
        mut params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        canonicalize_url(&mut params.text_document.uri);
        self.semantic_tokens_full_impl(params).await
    }

    async fn semantic_tokens_full_delta(
        &self,
        mut params: SemanticTokensDeltaParams,
    ) -> Result<Option<SemanticTokensFullDeltaResult>> {
        canonicalize_url(&mut params.text_document.uri);
        self.semantic_tokens_full_delta_impl(params).await
    }

    async fn semantic_tokens_range(
        &self,
        mut params: SemanticTokensRangeParams,
    ) -> Result<Option<SemanticTokensRangeResult>> {
        canonicalize_url(&mut params.text_document.uri);
        self.semantic_tokens_range_impl(params).await
    }

    async fn document_color(
        &self,
        mut params: DocumentColorParams,
    ) -> Result<Vec<ColorInformation>> {
        canonicalize_url(&mut params.text_document.uri);
        self.document_color_impl(params).await
    }

    async fn color_presentation(
        &self,
        mut params: ColorPresentationParams,
    ) -> Result<Vec<ColorPresentation>> {
        canonicalize_url(&mut params.text_document.uri);
        self.color_presentation_impl(params).await
    }

    async fn execute_command(&self, params: ExecuteCommandParams) -> Result<Option<Value>> {
        self.execute_command_impl(params).await
    }

    async fn did_change_watched_files(&self, mut params: DidChangeWatchedFilesParams) {
        for change in &mut params.changes {
            canonicalize_url(&mut change.uri);
        }
        self.did_change_watched_files_impl(params).await;
    }

    async fn did_create_files(&self, mut params: CreateFilesParams) {
        for f in &mut params.files {
            canonicalize_uri_string(&mut f.uri);
            self.invalidate_semantic_tokens(f.uri.as_str());
        }
        self.request_semantic_refresh().await;
    }

    async fn did_rename_files(&self, mut params: RenameFilesParams) {
        for f in &mut params.files {
            canonicalize_uri_string(&mut f.old_uri);
            canonicalize_uri_string(&mut f.new_uri);
            let old = f.old_uri.as_str();
            let new = f.new_uri.as_str();
            // Move open-document state if the renamed file was open.
            let moved = {
                let mut docs = self.state.documents.lock();
                docs.remove(old)
                    .map(|doc| (old.to_string(), new.to_string(), doc))
            };
            if let Some((old_uri, new_uri, doc)) = moved {
                let _ = self.state.documents.lock().open(new_uri.clone(), doc);
                // Move cached tokens to the new URI so delta resumes. Bound to a
                // `let` so the take's guard drops before the insert re-locks the
                // same non-reentrant mutex (#334).
                let moved_tokens = self.state.semantic_tokens_cache.lock().remove(&old_uri);
                if let Some(entry) = moved_tokens {
                    self.state
                        .semantic_tokens_cache
                        .lock()
                        .insert(new_uri, entry);
                } else {
                    self.invalidate_semantic_tokens(&new_uri);
                }
                self.invalidate_semantic_tokens(&old_uri);
            } else {
                self.invalidate_semantic_tokens(old);
                self.invalidate_semantic_tokens(new);
            }
        }
        self.request_semantic_refresh().await;
    }

    async fn did_delete_files(&self, mut params: DeleteFilesParams) {
        for f in &mut params.files {
            canonicalize_uri_string(&mut f.uri);
            self.invalidate_semantic_tokens(f.uri.as_str());
        }
        self.request_semantic_refresh().await;
    }

    async fn did_change_workspace_folders(&self, params: DidChangeWorkspaceFoldersParams) {
        self.did_change_workspace_folders_impl(params).await;
    }
}

fn main() {
    // Handle --help / --version before entering the LSP serve loop so the
    // binary prints useful output instead of silently blocking on stdin.
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

    // Logs/profiling go to stderr (stdout is the LSP JSON-RPC channel). Quiet
    // unless RUST_LOG or CWTOOLS_PROFILE is set. See PROFILING.md.
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
            // Use LspService::build to register the custom didFocusFile notification
            // so tower-lsp doesn't reject it with an error response.
            let (service, socket) = LspService::build(|client| Backend {
                client,
                state: state.clone(),
            })
            .custom_method("didFocusFile", Backend::on_did_focus_file)
            // `window/workDoneProgress/cancel` is the Cancel button on a
            // client-driven progress notification. tower-lsp 0.20 has no slot
            // for it on the `LanguageServer` trait (its lib.rs carries a TODO
            // to add one), so it is registered as a custom method — without
            // this the notification comes back as a "method not found" error
            // and Cancel does nothing. See `command_progress`.
            .custom_method(
                "window/workDoneProgress/cancel",
                Backend::on_work_done_progress_cancel,
            )
            .finish();
            Server::new(stdin, stdout, socket).serve(service).await;
            tracing::info!("LSP server shut down (stdin closed)");
        });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use parking_lot::Mutex;

    use super::*;
    use crate::state::{
        Config, DocumentRejection, DocumentStore, MAX_OPEN_DOCUMENTS, MAX_RETAINED_DOCUMENT_BYTES,
    };
    use cwtools_parser::parser::parse_string;
    use cwtools_string_table::string_table::StringTable;

    /// The base game and the rules dir are readable but never writable, so
    /// `refresh_roots` must keep them out of `editable_roots` while leaving
    /// them in `authorized_roots`. Collapsing the two lists would let a
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

    fn document(text: &str) -> ParsedDoc {
        ParsedDoc {
            version: 1,
            text: Arc::from(text),
            ast: None,
            ast_version: None,
            ast_source_bytes: 0,
        }
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

    // ── didFocusFile (background-reindex idle clock) ─────────────────────────

    #[test]
    fn test_loc_overlay_write_invalidates_the_cached_key_sets() {
        // #87 caches both overlay-derived key sets. The cache is keyed on
        // `loc_overlay_revision`, which `LocOverlayWrite::drop` bumps — so a
        // write through the accessor MUST change what the next read sees. If it
        // doesn't, a key the user just typed reads as undefined (CW225/CW122)
        // for as long as the stale entry lives.
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
        // The common loc keystroke edits a VALUE, not the key set. Taking the
        // overlay write guard is what invalidates the derived caches, so
        // re-recording an identical set rebuilt the ~200K-String `$ref$`
        // universe on every edit and the cache never hit while a `.yml` was
        // being typed in. Measured on a 1.29 MB Millennium Dawn loc file:
        // 65.7ms -> 13.7ms per edit once the no-op write is skipped.
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

            // Same keys, new value: nothing the derived sets depend on moved.
            let changed =
                backend.record_watched_loc_keys(uri, path, "l_english:\n MY_KEY:0 \"one two\"\n");
            assert!(changed.is_empty(), "got: {changed:?}");
            let second = backend.loc_overlay_keys();
            assert!(
                Arc::ptr_eq(&first, &second),
                "an unchanged key set must leave the cached union in place"
            );

            // A real key change must still invalidate it.
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
        // A focus switch is user activity: the handler must reset the idle
        // clock the background reindex loop watches, like edits and feature
        // requests do. Sentinel u64::MAX can never be a real elapsed value.
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
