use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tower_lsp::lsp_types::*;

use crate::Backend;
use crate::paths::uri_to_path_str;

use super::{
    WATCHED_BATCH_PANIC_ONCE, WATCHED_DEBOUNCE_MS, env_flag, resolve_watched_deletes,
    spawn_logging_panics, watched_batch_over_cap, watched_batch_slot_is_ours, watched_stat_sig,
};

impl Backend {
    /// for deleted/moved files until a window reload (#52).
    /// single trailing window (#90) instead of applying inline on the message
    #[tracing::instrument(skip_all)]
    pub(crate) async fn did_change_watched_files_impl(&self, params: DidChangeWatchedFilesParams) {
        let mut enqueued = false;
        for event in params.changes {
            let uri = event.uri.to_string();
            match event.typ {
                FileChangeType::DELETED => {
                    tracing::debug!(%uri, "watched file deleted; queued");
                    self.state.watched_deleted.lock().insert(uri);
                    // re-walks (#134). Clearing on any create/delete (not just
                    *self.state.loc_discovery_cache.lock() = None;
                    enqueued = true;
                }
                FileChangeType::CREATED => {
                    tracing::debug!(%uri, "watched file created; queued");
                    self.state.watched_pending.lock().insert(uri);
                    *self.state.loc_discovery_cache.lock() = None;
                    enqueued = true;
                }
                FileChangeType::CHANGED => {
                    self.state.watched_pending.lock().insert(uri);
                    enqueued = true;
                }
                _ => {}
            }
        }
        if enqueued {
            self.arm_watched_batch();
        }
    }

    pub(crate) fn arm_watched_batch(&self) {
        let mut guard = self.state.watched_debounce.lock();
        if guard.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        *guard = Some(self.spawn_watched_batch_window(false));
    }

    /// panic, the clones go back onto the queues (#155). `retried` bounds
    fn spawn_watched_batch_window(&self, retried: bool) -> tokio::task::JoinHandle<()> {
        let client = self.client.clone();
        let state = self.state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(WATCHED_DEBOUNCE_MS)).await;
            let changes: HashSet<String> = { state.watched_pending.lock().drain().collect() };
            let deletes: Vec<String> = {
                let mut deleted = state.watched_deleted.lock();
                resolve_watched_deletes(&changes, deleted.drain())
            };
            if changes.is_empty() && deletes.is_empty() {
                return;
            }
            let requeue_changes = changes.clone();
            let requeue_deletes = deletes.clone();
            let (batch_client, batch_state) = (client.clone(), state.clone());
            let ok = spawn_logging_panics("watched batch", async move {
                // Test-only panic injection (#155): CWTOOLS_WATCHED_BATCH_PANIC_ONCE
                if env_flag("CWTOOLS_WATCHED_BATCH_PANIC_ONCE")
                    && WATCHED_BATCH_PANIC_ONCE.swap(false, Ordering::SeqCst)
                {
                    panic!(
                        "CWTOOLS_WATCHED_BATCH_PANIC_ONCE: injected panic for #155 test coverage"
                    );
                }
                Backend {
                    client: batch_client,
                    state: batch_state,
                }
                .process_watched_batch(changes, deletes)
                .await;
            })
            .await;
            if ok {
                return;
            }
            let (changes_len, deletes_len) = (requeue_changes.len(), requeue_deletes.len());
            state.watched_pending.lock().extend(requeue_changes);
            state.watched_deleted.lock().extend(requeue_deletes);
            if retried {
                tracing::error!(
                    changes = changes_len,
                    deletes = deletes_len,
                    "watched batch panicked twice in a row; giving up on an immediate \
                     retry, events requeued for the next watched-file event, workspace \
                     rescan, or reindex pass"
                );
                return;
            }
            if !watched_batch_slot_is_ours(&state) {
                tracing::debug!(
                    "watched-debounce slot was already cleared or re-armed by the time \
                     this panic was handled; skipping the immediate retry, events stay \
                     requeued for whatever already owns the slot"
                );
                return;
            }
            let retry = Backend {
                client,
                state: state.clone(),
            }
            .spawn_watched_batch_window(true);
            *state.watched_debounce.lock() = Some(retry);
        })
    }

    pub(crate) async fn process_watched_batch(
        &self,
        changes: HashSet<String>,
        deletes: Vec<String>,
    ) {
        let mut lost_scan_cas = false;
        if watched_batch_over_cap(changes.len(), deletes.len()) {
            tracing::info!(
                changes = changes.len(),
                deletes = deletes.len(),
                "watched batch over cap; full rescan"
            );
            if !self.validate_entire_workspace(true).await {
                self.state.watched_pending.lock().extend(changes);
                self.state.watched_deleted.lock().extend(deletes);
                lost_scan_cas = true;
            }
        } else {
            let Ok(_validation_permit) = self.state.validation_permits.acquire().await else {
                return;
            };
            // open-doc edit path's job (#90).
            let mut changed_loc_keys: HashSet<String> = HashSet::new();
            if !deletes.is_empty() {
                changed_loc_keys.extend(self.process_watched_deletes(&deletes).await);
            }
            {
                let to_insert: Vec<String> = changes
                    .iter()
                    .filter(|uri| !self.is_ignored_uri(uri))
                    .filter_map(|uri| self.workspace_rel_for_file_index(uri))
                    .collect();
                if !to_insert.is_empty() {
                    let mut info = self.state.info_service.write();
                    if !info.type_index.file_index.is_empty() {
                        let type_index = Arc::make_mut(&mut info.type_index);
                        for rel in to_insert {
                            type_index.file_index.insert(&rel);
                        }
                    }
                }
            }
            for uri in changes {
                if self.is_ignored_uri(&uri) {
                    self.clear_ignored_file_state(&uri);
                    if let Ok(uri_obj) = Url::parse(&uri) {
                        self.publish_filtered(uri_obj, Vec::new(), None, None).await;
                    }
                    continue;
                }
                if self.state.documents.lock().contains_key(&uri) {
                    continue;
                }
                let path = uri_to_path_str(&uri);
                let Some(authorized) = self.authorized_path(&uri) else {
                    tracing::debug!(%uri, "watched file outside the access boundary; skipping");
                    continue;
                };
                let sig = watched_stat_sig(&authorized);
                if let Some(sig) = sig
                    && self.state.watched_signatures.lock().get(&uri) == Some(&sig)
                {
                    tracing::debug!(%uri, "watched file unchanged (stat match); skipping");
                    continue;
                }
                let read = tokio::task::spawn_blocking(move || {
                    crate::access::read_capped_text(&authorized, crate::access::MAX_URI_READ_BYTES)
                })
                .await;
                match read {
                    Ok(Some(text)) => {
                        if crate::paths::is_loc_file(&uri) {
                            changed_loc_keys
                                .extend(self.record_watched_loc_keys(&uri, &path, &text));
                        }
                        let (diagnostics, _) = self
                            .parse_and_validate(&uri, &text, crate::ValidateTrigger::Watched, None)
                            .await;
                        if let Some(sig) = sig {
                            self.state
                                .watched_signatures
                                .lock()
                                .insert(uri.clone(), sig);
                        }
                        if let Ok(uri_obj) = Url::parse(&uri) {
                            self.publish_gated(
                                uri_obj,
                                diagnostics,
                                None,
                                Some(cwtools_cache::workspace::content_hash(&text)),
                            )
                            .await;
                        }
                    }
                    Ok(None) => {
                        tracing::warn!("could not read watched file {}", path);
                    }
                    Err(e) => {
                        tracing::warn!("read task panicked for watched file {}: {}", path, e);
                    }
                }
            }
            if !changed_loc_keys.is_empty() {
                self.refresh_after_watched_loc_changes(&changed_loc_keys)
                    .await;
            }
            let queued: HashSet<String> =
                { self.state.pending_changed_names.lock().drain().collect() };
            if !queued.is_empty() {
                let generation = self.state.edit_generation.load(Ordering::Relaxed);
                self.revalidate_open_dependents("", generation, Some(&queued))
                    .await;
            }
            self.invalidate_all_semantic_tokens();
            self.request_semantic_refresh().await;
            self.request_code_lens_refresh().await;
        }
        *self.state.watched_debounce.lock() = None;
        if lost_scan_cas {
            if !self.state.scan_in_progress.load(Ordering::SeqCst) {
                self.arm_watched_batch();
            }
            return;
        }
        let pending_more = !self.state.watched_pending.lock().is_empty();
        let deleted_more = !self.state.watched_deleted.lock().is_empty();
        if pending_more || deleted_more {
            self.arm_watched_batch();
        }
    }

    pub(crate) fn workspace_rel_for_file_index(&self, uri: &str) -> Option<String> {
        let root = self.state.config.read().workspace_roots.first().cloned()?;
        let abs = self.authorized_path(uri)?;
        let rel = abs.strip_prefix(&root).ok()?;
        rel.to_str().map(|s| s.replace('\\', "/"))
    }

    /// `fixAllWorkspace` entry is dropped with its diagnostics (#133).
    async fn process_watched_deletes(&self, deletes: &[String]) -> HashSet<String> {
        let to_remove: Vec<String> = deletes
            .iter()
            .filter_map(|uri| self.workspace_rel_for_file_index(uri))
            .collect();
        {
            let mut info = self.state.info_service.write();
            if !to_remove.is_empty() && !info.type_index.file_index.is_empty() {
                let type_index = Arc::make_mut(&mut info.type_index);
                for rel in &to_remove {
                    type_index.file_index.remove(rel);
                }
            }
            for uri in deletes {
                info.clear_file(uri);
            }
        }
        let mut removed_loc_keys = HashSet::new();
        {
            let mut overlay = self.loc_live_overlay_mut();
            for uri in deletes {
                if let Some(keys) = overlay.remove(uri) {
                    removed_loc_keys.extend(keys);
                }
            }
        }
        {
            let mut watched = self.loc_watched_overlay_mut();
            for uri in deletes {
                if let Some(keys) = watched.remove(uri) {
                    removed_loc_keys.extend(keys);
                }
            }
        }
        {
            let mut sigs = self.state.watched_signatures.lock();
            for uri in deletes {
                sigs.remove(uri);
            }
        }
        // and reports CW274 instead of the (now-gone) body (#259).
        {
            let removed_scripts: HashSet<String> = deletes
                .iter()
                .filter_map(|uri| self.remove_inline_script(uri))
                .collect();
            if !removed_scripts.is_empty() {
                self.state
                    .pending_changed_names
                    .lock()
                    .extend(removed_scripts);
            }
        }
        {
            let mut dropped: HashSet<String> = HashSet::new();
            {
                let mut store = self.state.type_uses.write();
                for uri in deletes {
                    if let Some(uses) = store.remove(uri) {
                        dropped.extend(uses.changed_names(&Default::default()));
                    }
                }
            }
            if !dropped.is_empty() {
                self.state
                    .type_uses_revision
                    .fetch_add(1, Ordering::Release);
                self.state.pending_changed_names.lock().extend(dropped);
            }
        }
        self.bump_info_revision();
        for uri in deletes {
            if let Ok(uri_obj) = Url::parse(uri) {
                self.publish_filtered(uri_obj, vec![], None, None).await;
            }
        }
        removed_loc_keys
    }
}
