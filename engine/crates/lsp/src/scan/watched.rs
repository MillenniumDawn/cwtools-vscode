use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tower_lsp::lsp_types::*;

use crate::Backend;
use crate::paths::{is_resource_file, is_stray_loc_file, uri_to_path_str};

const WATCHED_FOLDER_EXPANSION_MAX_ENTRIES: usize = 10_000;

use super::{
    WATCHED_BATCH_PANIC_ONCE, WATCHED_DEBOUNCE_MS, env_flag, resolve_watched_deletes,
    spawn_logging_panics, watched_batch_over_cap, watched_batch_slot_is_ours, watched_stat_sig,
};

/// What a watched delete pass changed, so the batch can scope its dependent sweep
/// without revalidating files the pass could not have touched.
struct WatchedDeleteOutcome {
    removed_file_index: bool,
    changed_loc_keys: HashSet<String>,
}

impl Backend {
    /// Handle external file changes (e.g. a git checkout), which would otherwise leave stale
    /// index entries until a window reload (#52). Events are coalesced into one trailing
    /// window (#90) so a branch switch doesn't stall the message future on per-file work.
    #[tracing::instrument(skip_all)]
    pub(crate) async fn did_change_watched_files_impl(&self, params: DidChangeWatchedFilesParams) {
        let mut enqueued = false;
        let mut baseline_changed = false;
        let baseline_files = self.state.baseline.lock().metadata_files();
        for event in params.changes {
            let event_path = event.uri.to_file_path().ok();
            if let Some(path) = event_path.as_ref() {
                let roots = self.state.config.read().editable_roots.clone();
                if let Some((_, configured)) = baseline_files.iter().find(|(target, _)| {
                    target.file_name() == path.file_name()
                        && crate::access::editable_target(path, &roots).ok().as_ref()
                            == Some(target)
                }) {
                    self.state.baseline.lock().invalidate_capture(Some(path));
                    baseline_changed |= configured;
                    continue;
                }
            }
            let uri = event.uri.to_string();
            if is_stray_loc_file(&uri) {
                tracing::debug!(%uri, "watched localisation file outside a localisation folder; skipping");
                continue;
            }
            self.state
                .baseline
                .lock()
                .invalidate_capture(event_path.as_deref());
            match event.typ {
                FileChangeType::DELETED => {
                    tracing::debug!(%uri, "watched file deleted; queued");
                    self.state.watched_deleted.lock().insert(uri);
                    // The cached loc discovery may be stale; drop it so the next code-action
                    // request re-walks (#134). A spurious clear costs one re-walk.
                    *self.state.loc_discovery_cache.lock() = None;
                    enqueued = true;
                }
                FileChangeType::CREATED => {
                    tracing::debug!(%uri, "watched file created; queued");
                    // Marked first, so the batch that drains the URI always finds its marker.
                    self.state.watched_created.lock().insert(uri.clone());
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
        if baseline_changed {
            let setting = self.state.baseline.lock().setting.clone();
            if self.reload_baseline(setting).await {
                self.refresh_baseline_diagnostics().await;
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

    /// Spawn the debounce-window task without the `is_finished()` gate `arm_watched_batch`
    /// applies, which would never let the panic-recovery retry run. Events are requeued after
    /// a panic (#155); `retried` bounds that to ONE retry so a deterministic panic can't loop.
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
            let requeue_created: HashSet<String> = {
                let created = state.watched_created.lock();
                changes
                    .iter()
                    .filter(|uri| created.contains(*uri))
                    .cloned()
                    .collect()
            };
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
            state.watched_created.lock().extend(requeue_created);
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
        let created: HashSet<String> = {
            let mut markers = self.state.watched_created.lock();
            changes
                .iter()
                .filter(|uri| markers.remove(*uri))
                .cloned()
                .collect()
        };
        let (changes, folder_expansion_over_budget) =
            self.with_created_folder_contents(changes, &created);
        let deletes = self.with_deleted_folder_contents(deletes, &changes);
        // Resources skip the rescan cap, so a texture folder copy stays cheap (#1023).
        let (resource_changes, changes): (HashSet<String>, HashSet<String>) =
            changes.into_iter().partition(|uri| is_resource_file(uri));
        let (resource_deletes, deletes): (Vec<String>, Vec<String>) =
            deletes.into_iter().partition(|uri| is_resource_file(uri));
        // A resource delete only releases the workspace's own file-index entry (#1067). It
        // runs before the creates claim: the index ignores case, so DELETED Thing.dds plus
        // CREATED thing.dds in one window must end owned, not released.
        let resource_rels: Vec<String> = resource_deletes
            .iter()
            .filter_map(|uri| self.workspace_rel_for_deleted(uri))
            .collect();
        let mut index_changed = self.remove_deleted_rels_from_file_index(resource_rels);
        let resources_changed = self.sync_watched_resources(&resource_changes);
        let only_resources = changes.is_empty() && deletes.is_empty();
        let mut lost_scan_cas = false;
        if folder_expansion_over_budget || watched_batch_over_cap(changes.len(), deletes.len()) {
            tracing::info!(
                changes = changes.len(),
                deletes = deletes.len(),
                "watched batch over cap; full rescan"
            );
            if !self.validate_entire_workspace(true).await {
                if folder_expansion_over_budget {
                    // The folder is requeued whole, so it must still count as created.
                    self.state.watched_created.lock().extend(created);
                }
                let mut pending = self.state.watched_pending.lock();
                pending.extend(changes);
                if folder_expansion_over_budget {
                    pending.extend(resource_changes);
                }
                self.state
                    .watched_deleted
                    .lock()
                    .extend(deletes.into_iter().chain(resource_deletes));
                lost_scan_cas = true;
            }
        } else {
            // No batch-wide permit: each file below takes its own slot, so a
            // keystroke validation queued behind this batch waits for one file,
            // not the whole batch (#477).
            // open-doc edit path's job (#90).
            let mut changed_loc_keys: HashSet<String> = HashSet::new();
            if !deletes.is_empty() {
                let outcome = self.process_watched_deletes(&deletes).await;
                changed_loc_keys.extend(outcome.changed_loc_keys);
                index_changed |= outcome.removed_file_index;
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
                            // A changed/created file claims workspace membership (#1067).
                            index_changed |= type_index.file_index.claim_workspace(&rel);
                        }
                    }
                }
            }
            for uri in changes {
                #[cfg(test)]
                self.hold_from_async(crate::state::HoldPoint::WatchedFile);
                if self.is_ignored_uri(&uri) {
                    self.clear_ignored_file_state(&uri);
                    if let Ok(uri_obj) = Url::parse(&uri) {
                        self.publish_if_closed(uri_obj, Vec::new(), None).await;
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
                        let Some(_permit) = self.validation_permit().await else {
                            return;
                        };
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
                            self.publish_gated_if_closed(
                                uri_obj,
                                diagnostics,
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
            let sweep_pending = !changed_loc_keys.is_empty()
                || !self.state.pending_changed_names.lock().is_empty()
                || resources_changed
                || index_changed;
            if sweep_pending {
                // The cross-file sweeps are one more unit of work under one slot.
                let Some(_permit) = self.validation_permit().await else {
                    return;
                };
                if !changed_loc_keys.is_empty() {
                    self.refresh_after_watched_loc_changes(&changed_loc_keys)
                        .await;
                }
                let queued: HashSet<String> =
                    { self.state.pending_changed_names.lock().drain().collect() };
                if resources_changed || index_changed || !queued.is_empty() {
                    let generation = self.state.edit_generation.load(Ordering::Relaxed);
                    // Any filepath can resolve differently after a resource change or an added
                    // or dropped index entry, so those sweeps have no name scope (#1023, #1067).
                    let scope = (!(resources_changed || index_changed)).then_some(&queued);
                    self.revalidate_open_dependents("", generation, scope).await;
                }
            }
            if !only_resources {
                self.invalidate_all_semantic_tokens();
                self.request_semantic_refresh().await;
                self.request_code_lens_refresh().await;
            }
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

    /// Expand `created` folder events within the editable root; skip unsafe/excluded descendants and fall back to a workspace scan over budget.
    /// The canonical paths below only decide containment: URIs keep the event's own spelling,
    /// which is the one the index and open documents are keyed on.
    fn with_created_folder_contents(
        &self,
        changes: HashSet<String>,
        created: &HashSet<String>,
    ) -> (HashSet<String>, bool) {
        let Some(workspace_root) = self.state.config.read().editable_roots.first().cloned() else {
            return (changes, false);
        };
        let root = std::fs::canonicalize(&workspace_root).unwrap_or(workspace_root);
        let mut expanded = HashSet::new();
        let mut visited_entries = 0usize;
        for uri in &changes {
            if !created.contains(uri) {
                expanded.insert(uri.clone());
                continue;
            }
            let Some(path) = crate::access::file_uri_to_path(uri) else {
                expanded.insert(uri.clone());
                continue;
            };
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                expanded.insert(uri.clone());
                continue;
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                expanded.insert(uri.clone());
                continue;
            }
            let Ok(directory) = std::fs::canonicalize(&path) else {
                expanded.insert(uri.clone());
                continue;
            };
            let Some(relative) = directory.strip_prefix(&root).ok() else {
                expanded.insert(uri.clone());
                continue;
            };
            if relative.as_os_str().is_empty() {
                expanded.insert(uri.clone());
                continue;
            }
            let mut ancestor = path.as_path();
            let mut inside_root_without_symlinks = false;
            loop {
                if std::fs::symlink_metadata(ancestor)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    break;
                }
                if std::fs::canonicalize(ancestor).ok().as_deref() == Some(root.as_path()) {
                    inside_root_without_symlinks = true;
                    break;
                }
                let Some(parent) = ancestor.parent() else {
                    break;
                };
                ancestor = parent;
            }
            if !inside_root_without_symlinks {
                expanded.insert(uri.clone());
                continue;
            }
            if relative.components().any(|component| {
                component
                    .as_os_str()
                    .to_str()
                    .is_some_and(cwtools_file_manager::is_excluded_dir)
            }) || relative
                .components()
                .next()
                .and_then(|component| component.as_os_str().to_str())
                .is_some_and(cwtools_file_manager::is_excluded_root_dir)
            {
                continue;
            }
            let directory_uri = crate::paths::path_to_uri(&path);
            if self.is_ignored_uri(&format!("{directory_uri}/")) {
                continue;
            }
            let mut pending = vec![path];
            while let Some(directory) = pending.pop() {
                let Ok(entries) = std::fs::read_dir(&directory) else {
                    continue;
                };
                for entry in entries.flatten() {
                    visited_entries = visited_entries.saturating_add(1);
                    if visited_entries > WATCHED_FOLDER_EXPANSION_MAX_ENTRIES {
                        return (changes, true);
                    }
                    let path = entry.path();
                    let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                        continue;
                    };
                    if metadata.file_type().is_symlink() {
                        continue;
                    }
                    let uri = crate::paths::path_to_uri(&path);
                    if metadata.is_dir() {
                        if entry
                            .file_name()
                            .to_str()
                            .is_some_and(cwtools_file_manager::is_excluded_dir)
                            || self.is_ignored_uri(&format!("{uri}/"))
                        {
                            continue;
                        }
                        pending.push(path);
                    } else if metadata.is_file()
                        && !is_stray_loc_file(&uri)
                        && !self.is_ignored_uri(&uri)
                        && self.workspace_rel_for_file_index(&uri).is_some()
                    {
                        expanded.insert(uri);
                    }
                }
            }
        }
        (expanded, false)
    }

    /// Resource events update the file index only. True when the index content changed
    /// (#1023); ownership-only claims and unchanged re-saves stay false.
    fn sync_watched_resources(&self, changes: &HashSet<String>) -> bool {
        let mut added: Vec<String> = changes
            .iter()
            .filter(|uri| !self.is_ignored_uri(uri))
            .filter(|uri| std::path::Path::new(&uri_to_path_str(uri)).is_file())
            .filter_map(|uri| self.workspace_rel_for_file_index(uri))
            // Discovery skips a root-level `resources` folder, so the watcher does too.
            .filter(|rel| {
                !rel.split('/')
                    .next()
                    .is_some_and(cwtools_file_manager::is_excluded_root_dir)
            })
            .collect();
        if added.is_empty() {
            return false;
        }
        let mut info = self.state.info_service.write();
        if info.type_index.file_index.is_empty() {
            return false;
        }
        // A created resource claims workspace membership even when a lower layer already
        // provides the path (#1067). An already-claimed key changes nothing, so it is
        // skipped before the index Arc is cloned.
        added.retain(|rel| !info.type_index.file_index.is_workspace_claimed(rel));
        if added.is_empty() {
            return false;
        }
        let index = &mut Arc::make_mut(&mut info.type_index).file_index;
        let mut added_new = false;
        for rel in &added {
            added_new |= index.claim_workspace(rel);
        }
        drop(info);
        if added_new {
            self.bump_info_revision();
        }
        added_new
    }

    pub(crate) fn workspace_rel_for_file_index(&self, uri: &str) -> Option<String> {
        let abs = self.authorized_path(uri)?;
        self.rel_in_workspace(&abs)
    }

    /// The rel of a deleted path. Unlike `authorized_path`, a missing parent folder is no
    /// obstacle: the nearest existing ancestor is canonicalized and the tail re-joined,
    /// inside the editable roots only (#1092).
    fn workspace_rel_for_deleted(&self, uri: &str) -> Option<String> {
        let roots = self.state.config.read().editable_roots.clone();
        let abs = crate::access::editable_path(uri, &roots).ok()?;
        self.rel_in_workspace(&abs)
    }

    fn rel_in_workspace(&self, abs: &std::path::Path) -> Option<String> {
        // `abs` is canonical (`\\?\` on Windows), so the raw root never matches.
        let root = self.state.config.read().editable_roots.first().cloned()?;
        let rel = abs.strip_prefix(&root).ok()?.to_str()?;
        (!rel.is_empty()).then(|| rel.replace('\\', "/"))
    }

    /// A folder delete reaches the server as one event for the folder, because the client
    /// drops the events of the files inside it (#1092). Adds every URI the per-file delete
    /// holds state for below such a path, so each is cleared as if it had been reported.
    fn with_deleted_folder_contents(
        &self,
        deletes: Vec<String>,
        changes: &HashSet<String>,
    ) -> Vec<String> {
        let folders: HashSet<String> = {
            let rels: Vec<(&String, String)> = deletes
                .iter()
                .filter_map(|uri| Some((uri, self.workspace_rel_for_deleted(uri)?)))
                .collect();
            let info = self.state.info_service.read();
            // A file the index owns is no folder, which spares the common delete the sweep.
            rels.into_iter()
                .filter(|(_, rel)| !info.type_index.file_index.is_workspace_owned(rel))
                .map(|(uri, _)| uri.trim_end_matches('/').to_string())
                .collect()
        };
        if folders.is_empty() {
            return deletes;
        }
        let mut all: HashSet<String> = deletes.into_iter().collect();
        let mut note = |uri: &str| {
            if is_inside(uri, &folders) && !changes.contains(uri) {
                all.insert(uri.to_string());
            }
        };
        let state = &self.state;
        state
            .info_service
            .read()
            .files
            .keys()
            .for_each(|uri| note(uri));
        state.type_uses.read().keys().for_each(|uri| note(uri));
        state.loc_locations.read().uris().for_each(|uri| note(uri));
        state.loc_text.read().uris().for_each(|uri| note(uri));
        state
            .loc_live_overlay
            .read()
            .keys()
            .for_each(|uri| note(uri));
        state
            .loc_watched_overlay
            .read()
            .keys()
            .for_each(|uri| note(uri));
        state
            .watched_signatures
            .lock()
            .keys()
            .for_each(|uri| note(uri));
        all.into_iter().collect()
    }

    /// Drop the workspace's own file-index contributions of deleted rels (#1067) and of
    /// everything below a deleted folder (#1092). A key a lower layer still provides
    /// stays. Returns whether a key left the index.
    fn remove_deleted_rels_from_file_index(&self, rels: Vec<String>) -> bool {
        if rels.is_empty() {
            return false;
        }
        // A rel the workspace never owned changes nothing, so the index Arc stays shared
        // with any snapshot holding it. The sweep for a folder runs under the read lock.
        let owned = {
            let info = self.state.info_service.read();
            let file_index = &info.type_index.file_index;
            let (mut owned, folders): (Vec<String>, Vec<String>) = rels
                .into_iter()
                .partition(|rel| file_index.is_workspace_owned(rel));
            owned.extend(file_index.workspace_owned_under(&folders));
            owned
        };
        if owned.is_empty() {
            return false;
        }
        let mut info = self.state.info_service.write();
        let index = &mut Arc::make_mut(&mut info.type_index).file_index;
        let mut removed = false;
        for rel in &owned {
            removed |= index.release_workspace(rel);
        }
        drop(info);
        if removed {
            self.bump_info_revision();
        }
        removed
    }

    /// `fixAllWorkspace` entry is dropped with its diagnostics (#133).
    async fn process_watched_deletes(&self, deletes: &[String]) -> WatchedDeleteOutcome {
        let rels: Vec<String> = deletes
            .iter()
            .filter_map(|uri| self.workspace_rel_for_deleted(uri))
            .collect();
        let removed_file_index = self.remove_deleted_rels_from_file_index(rels);
        {
            let mut info = self.state.info_service.write();
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
            let mut locations = self.state.loc_locations.write();
            for uri in deletes {
                locations.remove_file(uri);
            }
        }
        {
            let mut loc_text = self.state.loc_text.write();
            for uri in deletes {
                loc_text.remove_file(uri);
            }
        }
        {
            let mut sigs = self.state.watched_signatures.lock();
            for uri in deletes {
                sigs.remove(uri);
            }
        }
        // A deleted `common/inline_scripts` file must stop expanding at its callers: queue its
        // call name so the sweep below revalidates them and reports CW274 instead of the
        // now-gone body (#259).
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
                self.publish_if_closed(uri_obj, vec![], None).await;
            }
        }
        WatchedDeleteOutcome {
            removed_file_index,
            changed_loc_keys: removed_loc_keys,
        }
    }
}

/// Whether `uri` sits inside one of `folders`, on whole path segments.
fn is_inside(uri: &str, folders: &HashSet<String>) -> bool {
    let mut parent = uri;
    while let Some((head, _)) = parent.rsplit_once('/') {
        if folders.contains(head) {
            return true;
        }
        parent = head;
    }
    false
}
