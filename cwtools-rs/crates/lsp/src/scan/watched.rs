use std::collections::HashSet;
use std::sync::atomic::Ordering;

use tower_lsp::lsp_types::*;

use crate::Backend;
use crate::paths::uri_to_path_str;

use super::{
    WATCHED_BATCH_PANIC_ONCE, WATCHED_DEBOUNCE_MS, env_flag, resolve_watched_deletes,
    spawn_logging_panics, watched_batch_over_cap, watched_batch_slot_is_ours, watched_stat_sig,
};

impl Backend {
    /// Handle external file changes (create, modify, delete) from the file
    /// system — e.g. a git checkout, file move in the OS explorer, or rename
    /// outside the editor. Without this handler the index keeps stale entries
    /// for deleted/moved files until a window reload (#52).
    ///
    /// DELETED and CHANGED/CREATED events are both queued and coalesced into a
    /// single trailing window (#90) instead of applying inline on the message
    /// future — DELETEs used to run a synchronous O(whole-index) `clear_file`
    /// per file, which stalled the message future for seconds on a large
    /// branch switch. The drain applies deletions first, batched under one
    /// `info_service` write.
    #[tracing::instrument(skip_all)]
    pub(crate) async fn did_change_watched_files_impl(&self, params: DidChangeWatchedFilesParams) {
        let mut enqueued = false;
        for event in params.changes {
            let uri = event.uri.to_string();
            match event.typ {
                FileChangeType::DELETED => {
                    tracing::debug!(%uri, "watched file deleted; queued");
                    self.state.watched_deleted.lock().insert(uri);
                    // A file's existence changed, so the cached loc discovery
                    // may be stale; drop it so the next code-action request
                    // re-walks (#134). Clearing on any create/delete (not just
                    // loc) is fine: a spurious clear costs one re-walk, and
                    // checking whether the path is loc would need the very
                    // discovery we're caching.
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
                    // Open state is re-checked at drain time (an open editor
                    // buffer is authoritative), so just queue every event here.
                    // A CHANGED event doesn't alter which loc files exist, so
                    // the discovery cache stays valid.
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

    /// Arm a single trailing window that drains the queued watched events
    /// (`watched_pending` + `watched_deleted`). A fixed window: if one is
    /// already scheduled or running, do nothing, so a continuous event stream
    /// can't keep pushing the drain further out.
    pub(crate) fn arm_watched_batch(&self) {
        let mut guard = self.state.watched_debounce.lock();
        if guard.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        *guard = Some(self.spawn_watched_batch_window(false));
    }

    /// Spawn the debounce-window task itself (sleep, drain, hand the batch to
    /// `process_watched_batch`) and return its handle, without the
    /// `is_finished()` gate `arm_watched_batch` applies. That gate exists so a
    /// concurrent caller doesn't stack a second window on top of a running
    /// one — it can't be reused for the panic-recovery retry below: at the
    /// moment a panic is observed, this task's own handle in the slot still
    /// reads as unfinished (we're suspended awaiting `spawn_logging_panics`,
    /// not returned), so `arm_watched_batch`'s gate would always defer to
    /// itself and never actually retry.
    ///
    /// The drain happens here rather than in `process_watched_batch`, so a
    /// panic in the batch can't strand the events it was handed: they're
    /// cloned before the handoff, and if `spawn_logging_panics` reports a
    /// panic, the clones go back onto the queues (#155). `retried` bounds
    /// that recovery to ONE immediate retry — a *deterministic* panic (a
    /// validator panicking on one file's content is the realistic trigger
    /// here) would otherwise loop forever: requeue, retry, panic again,
    /// every `WATCHED_DEBOUNCE_MS`, re-running a full rescan each cycle on
    /// the over-cap path. On a second panic in a row the events are left
    /// requeued for the next natural trigger instead — `arm_watched_batch`
    /// itself (the next unrelated watched-file event), the requeue check at
    /// the end of `validate_entire_workspace`, or a periodic reindex pass.
    fn spawn_watched_batch_window(&self, retried: bool) -> tokio::task::JoinHandle<()> {
        let client = self.client.clone();
        let state = self.state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(WATCHED_DEBOUNCE_MS)).await;
            let changes: HashSet<String> = { state.watched_pending.lock().drain().collect() };
            // A URI both changed and deleted this window is treated as a change.
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
                // panics the first batch after the server starts, then clears
                // itself, so the e2e suite can exercise the recovery path above.
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
            // Inspect the slot BEFORE spawning the retry: while this window's
            // own handle is unfinished, it's the one occupying the slot — by
            // construction, nothing else can install one, since
            // `arm_watched_batch`'s own gate no-ops against a live handle.
            // Checking first means the (today unreachable) case where the
            // slot has already moved on doesn't leak a spawned-but-untracked
            // retry task.
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

    /// Apply a coalesced batch of watched events (`changes` + `deletes`,
    /// already drained by `spawn_watched_batch_window`) off the message
    /// future. A batch larger than `WATCHED_BULK_CAP` collapses into one
    /// CAS-guarded rescan instead of hundreds of per-file validations — its
    /// on-disk prune drops the deleted URIs too, so deletes need no separate
    /// handling on that path. Below the cap, deletions apply first (one
    /// `info_service` write), then per-file validation. Re-arms if new events
    /// landed while it was running.
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
                // Lost the CAS to a running scan — requeue both sides for the
                // winner to drain when it finishes. Re-arming here would retry
                // (and re-log) every window for the winner's whole duration.
                self.state.watched_pending.lock().extend(changes);
                self.state.watched_deleted.lock().extend(deletes);
                lost_scan_cas = true;
            }
        } else {
            let Ok(_validation_permit) = self.state.validation_permits.acquire().await else {
                return;
            };
            // Deletions first, so a re-created file's later change validates
            // against an index that already forgot the stale entry.
            if !deletes.is_empty() {
                self.process_watched_deletes(&deletes).await;
            }
            // Keep FileIndex current for CW113 / icon completions: newly
            // created workspace files land in the index between full scans.
            // CHANGED that is actually a fresh create is also covered; insert
            // is idempotent for existing entries. Gated on a non-empty index
            // (vanilla present) so a mod-only workspace stays silent by design.
            {
                let to_insert: Vec<String> = changes
                    .iter()
                    .filter(|uri| !self.is_ignored_uri(uri))
                    .filter_map(|uri| self.workspace_rel_for_file_index(uri))
                    .collect();
                if !to_insert.is_empty() {
                    let mut info = self.state.info_service.write();
                    if !info.type_index.file_index.is_empty() {
                        for rel in to_insert {
                            info.type_index.file_index.insert(&rel);
                        }
                    }
                }
            }
            // Loc keys added or removed across the batch's loc files, recorded
            // per file in the watched overlay and swept ONCE after the loop —
            // the per-file cross-file sweep is the open-doc edit path's job
            // (#90).
            let mut changed_loc_keys: HashSet<String> = HashSet::new();
            for uri in changes {
                if self.is_ignored_uri(&uri) {
                    self.clear_ignored_file_state(&uri);
                    if let Ok(uri_obj) = Url::parse(&uri) {
                        self.publish_filtered(uri_obj, Vec::new(), None, None).await;
                    }
                    continue;
                }
                // An open editor buffer owns its diagnostics; skip files that
                // are open now, regardless of open state when queued.
                if self.state.documents.lock().contains_key(&uri) {
                    continue;
                }
                let path = uri_to_path_str(&uri);
                // A watched event is as client-supplied as a request URI, so it
                // goes through the same boundary before anything is read or any
                // diagnostic is published.
                let Some(authorized) = self.authorized_path(&uri) else {
                    tracing::debug!(%uri, "watched file outside the access boundary; skipping");
                    continue;
                };
                // Stat-gate: a toucher that rewrote identical bytes leaves
                // size+mtime unchanged, so skip the read + revalidate. `None`
                // (vanished/unreadable, or first-ever event) falls through.
                let sig = watched_stat_sig(&authorized);
                if let Some(sig) = sig
                    && self.state.watched_signatures.lock().get(&uri) == Some(&sig)
                {
                    tracing::debug!(%uri, "watched file unchanged (stat match); skipping");
                    continue;
                }
                // Read on a blocking thread via the boundary's capped reader so
                // cp1252 script files are validated (not silently dropped) and
                // the async runtime isn't stalled on the sync read.
                let read = tokio::task::spawn_blocking(move || {
                    crate::access::read_capped_text(&authorized, crate::access::MAX_URI_READ_BYTES)
                })
                .await;
                match read {
                    Ok(Some(text)) => {
                        // Record before validating so the file's own diagnostics
                        // resolve keys it just defined.
                        if crate::paths::is_loc_file(&uri) {
                            changed_loc_keys
                                .extend(self.record_watched_loc_keys(&uri, &path, &text));
                        }
                        let (diagnostics, _) = self
                            .parse_and_validate(&uri, &text, crate::ValidateTrigger::Watched, None)
                            .await;
                        // Record only after a successful validate, so a
                        // transient read failure doesn't poison the record.
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
            // Uses added/removed by the batch (a watched change's validate, or
            // a delete above) queued names whose CW239 status may have flipped;
            // sweep the open docs that mention them. Without this the names
            // sat in `pending_changed_names` until the next unrelated edit.
            let queued: HashSet<String> =
                { self.state.pending_changed_names.lock().drain().collect() };
            if !queued.is_empty() {
                let generation = self.state.edit_generation.load(Ordering::Relaxed);
                self.revalidate_open_dependents("", generation, Some(&queued))
                    .await;
            }
            // Bulk index change (watched batch) may affect rule-driven token
            // upgrades for visible files; refresh client tokens.
            self.invalidate_all_semantic_tokens();
            self.request_semantic_refresh().await;
        }
        // Clear our slot before the final check so a producer that queued an
        // event while we ran can arm the next window (or we do it here). Setting
        // the slot to `None` only detaches this finished task, it doesn't abort.
        *self.state.watched_debounce.lock() = None;
        if lost_scan_cas {
            // The scan winner drains the requeue at its end; only if it already
            // finished (between the CAS failure and the requeue, so its drain
            // saw empty queues) do we arm on its behalf.
            if !self.state.scan_in_progress.load(Ordering::SeqCst) {
                self.arm_watched_batch();
            }
            return;
        }
        // Each guard scoped to its own `let` so the two queue locks are never
        // held at once.
        let pending_more = !self.state.watched_pending.lock().is_empty();
        let deleted_more = !self.state.watched_deleted.lock().is_empty();
        if pending_more || deleted_more {
            self.arm_watched_batch();
        }
    }

    /// Workspace-relative path for FileIndex bookkeeping, if `uri` is under
    /// the workspace root and inside the access boundary. `None` for vanilla
    /// files or URIs outside the boundary.
    pub(crate) fn workspace_rel_for_file_index(&self, uri: &str) -> Option<String> {
        let root = self.state.config.read().workspace_roots.first().cloned()?;
        let abs = self.authorized_path(uri)?;
        let rel = abs.strip_prefix(&root).ok()?;
        rel.to_str().map(|s| s.replace('\\', "/"))
    }

    /// Apply a coalesced batch of DELETE events off the message future: forget
    /// each URI from the info service (one write scope), both loc overlays, and
    /// the watched-signature record, bump the info revision once for the whole
    /// batch, then publish empty diagnostics per URI outside every lock. The
    /// empty publish goes through `publish_filtered` so the deleted file's
    /// `fixAllWorkspace` entry is dropped with its diagnostics (#133).
    async fn process_watched_deletes(&self, deletes: &[String]) {
        // Keep FileIndex current for CW113 / icon completions: remove deleted
        // workspace files. Only when the index is already populated (vanilla
        // present); a mod-only workspace has an empty index by design.
        let to_remove: Vec<String> = deletes
            .iter()
            .filter_map(|uri| self.workspace_rel_for_file_index(uri))
            .collect();
        {
            let mut info = self.state.info_service.write();
            if !to_remove.is_empty() && !info.type_index.file_index.is_empty() {
                for rel in &to_remove {
                    info.type_index.file_index.remove(rel);
                }
            }
            for uri in deletes {
                info.clear_file(uri);
            }
        }
        {
            let mut overlay = self.loc_live_overlay_mut();
            for uri in deletes {
                overlay.remove(uri);
            }
        }
        {
            let mut watched = self.loc_watched_overlay_mut();
            for uri in deletes {
                watched.remove(uri);
            }
        }
        {
            let mut sigs = self.state.watched_signatures.lock();
            for uri in deletes {
                sigs.remove(uri);
            }
        }
        // A deleted file's recorded `<type>` uses must not keep suppressing
        // CW239 on the instances it referenced. Queue the affected names; the
        // batch's closing sweep republishes their open definition files.
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
    }
}
