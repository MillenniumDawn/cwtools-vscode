use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tower_lsp::lsp_types::*;

use cwtools_cache::workspace as workspace_cache;
use cwtools_parser::parser::parse_string_without_comments;
use cwtools_rules::rules_types::RuleSet;
use cwtools_validation::inline_ignore::{InlineIgnoreMap, extract_inline_ignored_codes};
use cwtools_validation::references::{UsedInstances, check_unused_instances, needs_use_tracking};
use cwtools_validation::validate_prepared_tracking_uses;

use crate::Backend;
use crate::command_progress::{
    CancelFlag, CommandProgress, Phase, PhaseTicker, ScanOutcome, cancel_flag_of, phase_percentage,
    start_phase,
};
use crate::lines::DocLines;
use crate::paths::{logical_path_from_uri, path_to_uri, uri_to_path_str};
use crate::validate::{
    make_prepared, parse_errors_to_diagnostics, validate_parsed_with_indexes,
    validation_error_to_diagnostic,
};

use super::{
    OpenDocSnapshot, ScanGuard, ScanSummary, ScannedFile, hold_parse_for_tests,
    hold_scan_for_tests, quiet_pass_can_skip, spawn_logging_panics, stat_signature_for,
};

#[cfg(not(test))]
const PASS2_CHUNK_SIZE: usize = 256;
#[cfg(test)]
const PASS2_CHUNK_SIZE: usize = 2;

#[cfg(not(test))]
const WORKSPACE_DIAGNOSTICS_BUDGET: usize = 2_000;
#[cfg(test)]
const WORKSPACE_DIAGNOSTICS_BUDGET: usize = 2;

#[cfg(not(test))]
const WORKSPACE_DIAGNOSTICS_CLEAR_BUDGET: usize = 2_000;
#[cfg(test)]
const WORKSPACE_DIAGNOSTICS_CLEAR_BUDGET: usize = 2;

const WORKSPACE_PUBLISH_BATCH_SIZE: usize = 50;

#[derive(Clone)]
struct WorkspacePublishThrottle {
    interval: std::time::Duration,
    #[cfg(test)]
    wait_count: Option<Arc<std::sync::atomic::AtomicUsize>>,
}

impl WorkspacePublishThrottle {
    const fn new(interval: std::time::Duration) -> Self {
        Self {
            interval,
            #[cfg(test)]
            wait_count: None,
        }
    }

    #[cfg(test)]
    fn with_wait_count(
        interval: std::time::Duration,
        wait_count: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        Self {
            interval,
            wait_count: Some(wait_count),
        }
    }

    const fn should_wait_after(&self, entry_index: usize) -> bool {
        entry_index % WORKSPACE_PUBLISH_BATCH_SIZE == WORKSPACE_PUBLISH_BATCH_SIZE - 1
    }

    async fn wait(&self) {
        #[cfg(test)]
        if let Some(wait_count) = &self.wait_count {
            wait_count.fetch_add(1, Ordering::Relaxed);
        }
        if !self.interval.is_zero() {
            tokio::time::sleep(self.interval).await;
        }
    }
}

const PRODUCTION_WORKSPACE_PUBLISH_THROTTLE: WorkspacePublishThrottle =
    WorkspacePublishThrottle::new(std::time::Duration::from_millis(1));
#[cfg(not(test))]
const DEFAULT_WORKSPACE_PUBLISH_THROTTLE: WorkspacePublishThrottle =
    PRODUCTION_WORKSPACE_PUBLISH_THROTTLE;
#[cfg(test)]
const DEFAULT_WORKSPACE_PUBLISH_THROTTLE: WorkspacePublishThrottle =
    WorkspacePublishThrottle::new(std::time::Duration::ZERO);

/// behavioral, which the parity tests below pin (#328).
/// instead, and let the invariant fail loudly at the boundary it belongs to.
struct ChunkMapper<F, Before, After> {
    map: F,
    before_chunk: Before,
    after_chunk: After,
}

async fn chunked_par_filter_map<A, B, C, D, R, F, Before, After>(
    first: &[A],
    second: &[B],
    third: &[C],
    fourth: &[D],
    chunk: usize,
    cancel: &CancelFlag,
    mapper: ChunkMapper<F, Before, After>,
) -> Option<Vec<R>>
where
    A: Sync,
    B: Sync,
    C: Sync,
    D: Sync,
    R: Send,
    F: Fn(&A, &B, &C, &D) -> Option<R> + Send + Sync,
    Before: Fn(),
    After: Fn(),
{
    use rayon::prelude::*;

    let ChunkMapper {
        map,
        before_chunk,
        after_chunk,
    } = mapper;
    assert_eq!(first.len(), second.len());
    assert_eq!(first.len(), third.len());
    assert_eq!(first.len(), fourth.len());
    let mut out: Vec<R> = Vec::with_capacity(first.len());
    for (((a, b), c), d) in first
        .chunks(chunk)
        .zip(second.chunks(chunk))
        .zip(third.chunks(chunk))
        .zip(fourth.chunks(chunk))
    {
        before_chunk();
        if cancel.is_cancelled() {
            return None;
        }
        let mapped: Vec<R> = a
            .par_iter()
            .zip(b.par_iter())
            .zip(c.par_iter())
            .zip(d.par_iter())
            .filter_map(|(((a, b), c), d)| map(a, b, c, d))
            .collect();
        out.extend(mapped);
        after_chunk();
        tokio::task::yield_now().await;
        if cancel.is_cancelled() {
            return None;
        }
    }
    Some(out)
}

impl Backend {
    pub(crate) async fn validate_entire_workspace(&self, quiet: bool) -> bool {
        matches!(
            self.validate_entire_workspace_tracked(quiet, None).await,
            ScanOutcome::Ran
        )
    }

    pub(crate) async fn validate_entire_workspace_tracked(
        &self,
        quiet: bool,
        progress: Option<&CommandProgress>,
    ) -> ScanOutcome {
        if self
            .state
            .scan_in_progress
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            tracing::debug!("workspace scan already in progress; skipping");
            return ScanOutcome::Busy;
        }
        let guard = ScanGuard::for_scan(self, quiet);
        let completed = self
            .validate_entire_workspace_inner(quiet, progress, DEFAULT_WORKSPACE_PUBLISH_THROTTLE)
            .await;
        guard.finish().await;
        let requeued_pending = !self.state.watched_pending.lock().is_empty();
        let requeued_deleted = !self.state.watched_deleted.lock().is_empty();
        if requeued_pending || requeued_deleted {
            self.arm_watched_batch();
        }
        if completed {
            ScanOutcome::Ran
        } else {
            ScanOutcome::Cancelled
        }
    }

    async fn enter_phase(
        &self,
        ticker: &mut PhaseTicker,
        progress: Option<&CommandProgress>,
        quiet: bool,
        phase: Phase,
        total: usize,
    ) {
        self.end_phase(ticker).await;
        let token = progress.and_then(CommandProgress::token);
        if quiet && token.is_none() {
            return;
        }
        if quiet {
            if let Some(cp) = progress {
                cp.report_phase(phase).await;
            }
        } else {
            self.send_loading_bar_pct(
                progress,
                true,
                phase.label(),
                Some(phase_percentage(phase, 0, total)),
            )
            .await;
        }
        *ticker = start_phase(self, progress, quiet, phase, total);
    }

    async fn end_phase(&self, ticker: &mut PhaseTicker) {
        if let Some(summary) = std::mem::replace(ticker, PhaseTicker::inert()).stop() {
            self.client.log_message(MessageType::INFO, summary).await;
        }
    }

    pub(crate) fn spawn_deferred_revalidation(&self, context: &'static str) {
        let client = self.client.clone();
        let state = self.state.clone();
        tokio::spawn(async move {
            spawn_logging_panics("deferred revalidation", async move {
                let backend = Backend { client, state };
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_secs(180);
                let mut revalidated = backend.validate_entire_workspace(false).await;
                while !revalidated && std::time::Instant::now() < deadline {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    revalidated = backend.validate_entire_workspace(false).await;
                }
                if !revalidated {
                    backend
                        .client
                        .log_message(
                            MessageType::WARNING,
                            format!(
                                "{context}: deferred re-validation gave up; a scan held the workspace the whole time"
                            ),
                        )
                        .await;
                }
            })
            .await;
        });
    }

    #[tracing::instrument(skip_all)]
    async fn validate_entire_workspace_inner(
        &self,
        quiet: bool,
        progress: Option<&CommandProgress>,
        publish_throttle: WorkspacePublishThrottle,
    ) -> bool {
        let cancel = cancel_flag_of(progress);
        cwtools_profiling::log_rss("workspace_scan_start");
        // sit on "Validating workspace… 70%" for the whole of pass 2 (#221).
        let mut phase = PhaseTicker::inert();
        self.enter_phase(&mut phase, progress, quiet, Phase::Discover, 0)
            .await;
        hold_scan_for_tests().await;

        let workspace_uri = self.state.config.read().workspace_uri.clone();

        let root_path = match workspace_uri {
            Some(ref uri) => std::path::PathBuf::from(uri_to_path_str(uri)),
            None => {
                self.client
                    .log_message(
                        MessageType::WARNING,
                        "No workspace folder; skipping full-workspace validation.",
                    )
                    .await;
                self.state
                    .index_ready
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                return true;
            }
        };

        let (extra_file_globs, extra_dir_globs) = {
            let cfg = self.state.config.read();
            (
                cfg.ignore_file_patterns.clone(),
                cfg.ignore_dir_patterns.clone(),
            )
        };
        let ruleset = self.state.rules.read().ruleset.clone();

        // layering in one place (#284).
        let discovery = tokio::task::block_in_place(|| {
            let mut fm_config =
                cwtools_driver::workspace_discovery_config(&root_path, ruleset.as_deref());
            fm_config
                .exclude_patterns
                .extend(extra_file_globs.iter().cloned());
            fm_config
                .exclude_dir_patterns
                .extend(extra_dir_globs.iter().cloned());
            cwtools_driver::discover_workspace_files(fm_config)
        });
        let files_to_validate = match discovery {
            Ok(files) => files.into_iter().map(|f| f.path).collect(),
            Err(error) => {
                tracing::error!(path = %root_path.display(), error = %error, "workspace discovery failed");
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!(
                            "error: discovery failed for {}: {}",
                            root_path.display(),
                            error
                        ),
                    )
                    .await;
                Vec::new()
            }
        };

        let scan_fingerprint =
            tokio::task::block_in_place(|| stat_signature_for(&files_to_validate));
        let scan_generation = self
            .state
            .settings_generation
            .load(std::sync::atomic::Ordering::SeqCst);
        if quiet_pass_can_skip(
            quiet,
            files_to_validate.is_empty(),
            (scan_fingerprint, scan_generation),
            *self.state.last_scan_fingerprint.lock(),
        ) {
            tracing::info!(
                files = files_to_validate.len(),
                "quiet scan: workspace fingerprint unchanged, skipping reindex"
            );
            return true;
        }

        if cancel.is_cancelled() {
            return false;
        }

        let scan_files: Vec<ScannedFile> = files_to_validate
            .into_iter()
            .map(|path| ScannedFile {
                uri: path_to_uri(&path),
                path,
            })
            .collect();

        self.client
            .log_message(
                MessageType::INFO,
                format!(
                    "Validating {} workspace files under {:?} ...",
                    scan_files.len(),
                    root_path
                ),
            )
            .await;

        let (cache_info, cache_status) = {
            let (cache_dir, language) = {
                let cfg = self.state.config.read();
                (cfg.cache_dir.clone(), cfg.language.clone())
            };
            match cache_dir {
                Some(cd) => {
                    let fp = workspace_cache::settings_fingerprint(&language, &root_path);
                    match workspace_cache::validate_or_clear(&cd, fp) {
                        Ok(true) => (Some((cd, fp)), "Parse cache: hit (settings match)"),
                        Ok(false) => (Some((cd, fp)), "Parse cache: settings changed, cleared"),
                        Err(error) => {
                            tracing::warn!(dir = %cd.display(), error = %error, "parse cache unavailable");
                            (None, "Parse cache: unavailable")
                        }
                    }
                }
                None => (None, "Parse cache: disabled"),
            }
        };
        self.client
            .log_message(MessageType::INFO, cache_status)
            .await;

        self.enter_phase(&mut phase, progress, quiet, Phase::Parse, scan_files.len())
            .await;
        hold_parse_for_tests().await;
        let open_uris: HashSet<String> = {
            let docs = self.state.documents.lock();
            docs.keys().cloned().collect()
        };

        let mut cache_hits = 0u64;
        let mut cache_misses = 0u64;
        use rayon::prelude::*;
        type ParseOutcome = (
            bool,
            cwtools_parser::ast::ParsedFile,
            Option<u64>,
            InlineIgnoreMap,
        );
        let scan_bytes = cwtools_file_manager::file_manager::ScanBytes::new();
        let outcomes: Vec<Option<ParseOutcome>> = tokio::task::block_in_place(|| {
            scan_files
                .par_iter()
                .map(|file| {
                    if cancel.is_cancelled() {
                        return None;
                    }
                    phase.tick();
                    if open_uris.contains(&file.uri) {
                        return None;
                    }
                    if let Some((ref cd, fp)) = cache_info
                        && let Some((parsed, source_key)) = workspace_cache::load_path(
                            cd,
                            fp,
                            &file.path,
                            &self.state.string_table,
                        )
                    {
                        let (source_hash, inline_ignored) =
                            match cwtools_file_manager::file_manager::read_text_capped(
                                &file.path,
                                crate::access::MAX_URI_READ_BYTES,
                            ) {
                                Ok((text, _)) => (
                                    (workspace_cache::source_cache_key(&file.path).as_ref()
                                        == Some(&source_key))
                                    .then(|| cwtools_cache::workspace::content_hash(&text)),
                                    extract_inline_ignored_codes(&text),
                                ),
                                Err(_) => (None, InlineIgnoreMap::new()),
                            };
                        return Some((true, parsed, source_hash, inline_ignored));
                    }
                    let source_key = (workspace_cache::PATH_METADATA_CACHE_SUPPORTED)
                        .then(|| workspace_cache::source_cache_key(&file.path))
                        .flatten();
                    let use_content_cache =
                        !workspace_cache::PATH_METADATA_CACHE_SUPPORTED || source_key.is_none();
                    let text = match cwtools_file_manager::file_manager::read_text_capped(
                        &file.path,
                        crate::access::MAX_URI_READ_BYTES,
                    ) {
                        Ok((t, n)) => {
                            if !scan_bytes.try_reserve(
                                n,
                                cwtools_file_manager::file_manager::ScanBudget::default().max_bytes,
                            ) {
                                tracing::warn!(
                                    path = %file.path.display(),
                                    "scan: skipping file, byte budget exceeded"
                                );
                                return None;
                            }
                            t
                        }
                        Err(e) => {
                            tracing::warn!(path = %file.path.display(), error = %e, "scan: skipping unreadable file");
                            return None;
                        }
                    };
                    if use_content_cache
                        && let Some((cd, fp)) = cache_info.as_ref()
                        && let Some(parsed) = workspace_cache::load(
                            cd,
                            *fp,
                            &text,
                            &self.state.string_table,
                        )
                    {
                        return Some((
                            true,
                            parsed,
                            Some(cwtools_cache::workspace::content_hash(&text)),
                            extract_inline_ignored_codes(&text),
                        ));
                    }
                    let parsed = parse_string_without_comments(&text, &self.state.string_table);
                    if let Some((cd, fp)) = cache_info.as_ref() {
                        if let Some(source_key) = source_key.as_ref() {
                            workspace_cache::store_path(
                                cd,
                                *fp,
                                &file.path,
                                source_key,
                                &parsed,
                                &self.state.string_table,
                            );
                        } else {
                            workspace_cache::store(
                                cd,
                                *fp,
                                &text,
                                &parsed,
                                &self.state.string_table,
                            );
                        }
                    }
                    Some((
                        false,
                        parsed,
                        Some(cwtools_cache::workspace::content_hash(&text)),
                        extract_inline_ignored_codes(&text),
                    ))
                })
                .collect()
        });
        if cancel.is_cancelled() {
            return false;
        }
        let wrote_cache = outcomes
            .iter()
            .any(|outcome| outcome.as_ref().is_some_and(|(hit, _, _, _)| !hit));
        if wrote_cache && let Some((cache_dir, fingerprint)) = cache_info.as_ref() {
            workspace_cache::prune(cache_dir, *fingerprint);
        }

        let mut parsed_files: Vec<Option<cwtools_parser::ast::ParsedFile>> =
            Vec::with_capacity(scan_files.len());
        let mut source_hashes: Vec<Option<u64>> = Vec::with_capacity(scan_files.len());
        let mut inline_ignores: Vec<InlineIgnoreMap> = Vec::with_capacity(scan_files.len());
        for (i, (file, outcome)) in scan_files.iter().zip(outcomes).enumerate() {
            let parsed = match outcome {
                Some((cache_hit, parsed, source_hash, inline_ignored)) => {
                    self.index_parsed_file(&file.uri, &parsed, None);
                    if cache_hit {
                        cache_hits += 1;
                    } else {
                        cache_misses += 1;
                    }
                    source_hashes.push(source_hash);
                    inline_ignores.push(inline_ignored);
                    Some(parsed)
                }
                None => {
                    source_hashes.push(None);
                    inline_ignores.push(InlineIgnoreMap::new());
                    None
                }
            };
            parsed_files.push(parsed);
            if quiet && i % 64 == 63 {
                tokio::task::yield_now().await;
            }
            if i % 64 == 63 && cancel.is_cancelled() {
                return false;
            }
        }

        self.client
            .log_message(
                MessageType::INFO,
                format!(
                    "Indexing pass: {} cache hits, {} misses",
                    cache_hits, cache_misses
                ),
            )
            .await;

        let discovered_uris: HashSet<String> =
            scan_files.iter().map(|file| file.uri.clone()).collect();
        let removed_uris: Vec<String> = if scan_files.is_empty() {
            Vec::new()
        } else {
            let mut info = self.state.info_service.write();
            let stale: Vec<String> = info
                .files
                .keys()
                .filter(|&uri| {
                    uri.starts_with("file://")
                        && !discovered_uris.contains(uri)
                        && !open_uris.contains(uri)
                })
                .cloned()
                .collect();
            for uri in &stale {
                info.clear_file(uri);
            }
            stale
        };
        if !removed_uris.is_empty() {
            {
                let mut overlay = self.loc_live_overlay_mut();
                for uri in &removed_uris {
                    overlay.remove(uri);
                }
            }
            {
                let mut watched = self.loc_watched_overlay_mut();
                for uri in &removed_uris {
                    watched.remove(uri);
                }
            }
            self.bump_info_revision();
            self.client
                .log_message(
                    MessageType::INFO,
                    format!(
                        "Pruned {} file(s) no longer on disk from the index",
                        removed_uris.len()
                    ),
                )
                .await;
            for uri in &removed_uris {
                if let Ok(uri_obj) = Url::parse(uri) {
                    self.publish_filtered(uri_obj, vec![], None, None).await;
                }
            }
        }

        if cancel.is_cancelled() {
            return false;
        }

        self.enter_phase(&mut phase, progress, quiet, Phase::Vanilla, 0)
            .await;
        self.ensure_vanilla_index(progress, false, quiet).await;
        if cancel.is_cancelled() {
            return false;
        }

        tokio::task::block_in_place(|| self.merge_pending_vanilla_index());

        self.rebuild_modifier_keys();

        self.enter_phase(&mut phase, progress, quiet, Phase::Localisation, 0)
            .await;
        let loc_signature = tokio::task::block_in_place(|| self.compute_loc_signature(&root_path));
        let loc_unchanged = *self.state.last_loc_signature.lock() == Some(loc_signature);
        if quiet && loc_unchanged {
            tracing::info!("quiet scan: loc signature unchanged, skipping loc rebuild");
        } else {
            self.rebuild_and_publish_loc(&root_path).await;
        }
        if cancel.is_cancelled() {
            return false;
        }
        *self.state.last_loc_signature.lock() = Some(loc_signature);

        // (#259), so a script created, deleted or edited outside a watched
        let fresh_inline_scripts = if scan_files.is_empty() {
            None
        } else {
            Some(tokio::task::block_in_place(|| {
                self.rebuild_inline_scripts(&scan_files)
            }))
        };

        self.state
            .index_ready
            .store(true, std::sync::atomic::Ordering::Relaxed);

        self.enter_phase(
            &mut phase,
            progress,
            quiet,
            Phase::Validate,
            scan_files.len(),
        )
        .await;
        let mut total_errors = 0usize;
        let mut total_warnings = 0usize;
        let mut total_infos = 0usize;
        let mut total_hints = 0usize;
        let mut files_with_errors = 0usize;
        let total_files = scan_files.len();
        let scan_game = self.state.config.read().game();
        let (scan_ruleset, scan_registry, modifier_keys_snap): (
            Option<Arc<RuleSet>>,
            _,
            Arc<HashSet<String>>,
        ) = {
            let rules = self.state.rules.read();
            (
                rules.ruleset.clone(),
                rules.scope_registry.clone(),
                rules.modifier_keys.clone(),
            )
        };

        let (scope_checks, var_checks) = {
            let cfg = self.state.config.read();
            (cfg.scope_checks, cfg.var_checks)
        };
        let track_uses = scan_ruleset
            .as_ref()
            .is_some_and(|rs| needs_use_tracking(rs, scan_game));
        // them in the other order here would be an ABBA deadlock). Both scan
        let open_doc_asts: Vec<(String, Arc<cwtools_parser::ast::ParsedFile>)> = if track_uses {
            let docs = self.state.documents.lock();
            docs.iter()
                .filter_map(|(u, d)| d.ast.clone().map(|ast| (u.clone(), ast)))
                .collect()
        } else {
            Vec::new()
        };
        type ValidationOutcome = (
            String,
            Vec<Diagnostic>,
            Option<UsedInstances>,
            Option<u64>,
            InlineIgnoreMap,
        );
        let type_index_snap = Arc::clone(&self.state.info_service.read().type_index);
        let loc_index_snap = self.state.loc_index.read().clone();
        // invariant bug surfaces instead of silently dropping diagnostics.
        assert_eq!(scan_files.len(), parsed_files.len());
        assert_eq!(scan_files.len(), source_hashes.len());
        assert_eq!(scan_files.len(), inline_ignores.len());
        let registry = scan_registry.as_ref();
        let prepared = scan_ruleset.as_ref().map(|ruleset| {
            make_prepared(
                ruleset,
                &self.state.string_table,
                scan_game,
                &type_index_snap,
                &modifier_keys_snap,
                loc_index_snap.as_deref(),
                None,
                fresh_inline_scripts.as_ref(),
                registry,
                scope_checks,
                var_checks,
            )
        });
        let Some(mut results): Option<Vec<ValidationOutcome>> = chunked_par_filter_map(
            &scan_files,
            &parsed_files,
            &source_hashes,
            &inline_ignores,
            PASS2_CHUNK_SIZE,
            &cancel,
            ChunkMapper {
                map: |file: &ScannedFile,
                      parsed_opt: &Option<cwtools_parser::ast::ParsedFile>,
                      source_hash: &Option<u64>,
                      inline_ignored: &InlineIgnoreMap| {
                    #[cfg(test)]
                    self.hold_pass2(crate::state::Pass2HoldPoint::Mid);
                    if cancel.is_cancelled() {
                        return None;
                    }
                    phase.tick();
                    let parsed = parsed_opt.as_ref()?;
                    if open_uris.contains(&file.uri) {
                        return None;
                    }
                    let no_lines = DocLines::none();
                    let (diagnostics, used) = match &prepared {
                        Some(prepared) => validate_parsed_with_indexes(
                            &file.uri, parsed, prepared, &no_lines, track_uses,
                        ),
                        None => (parse_errors_to_diagnostics(&parsed.errors, &no_lines), None),
                    };
                    Some((
                        file.uri.clone(),
                        diagnostics,
                        used,
                        *source_hash,
                        inline_ignored.clone(),
                    ))
                },
                before_chunk: || {
                    #[cfg(test)]
                    self.hold_pass2(crate::state::Pass2HoldPoint::Before);
                },
                after_chunk: || {
                    #[cfg(test)]
                    self.hold_pass2(crate::state::Pass2HoldPoint::After);
                },
            },
        )
        .await
        else {
            return false;
        };
        if track_uses
            && !cancel.is_cancelled()
            && let Some(prepared) = &prepared
        {
            let unrecorded: Vec<(String, Arc<cwtools_parser::ast::ParsedFile>)> = {
                let store = self.state.type_uses.read();
                open_doc_asts
                    .iter()
                    .filter(|(u, _)| !store.contains_key(u))
                    .cloned()
                    .collect()
            };
            let open_uses: Vec<(String, UsedInstances)> = unrecorded
                .par_iter()
                .map(|(u, ast)| {
                    let (_, used) = validate_prepared_tracking_uses(ast, u, prepared);
                    (u.clone(), used)
                })
                .collect();
            let merged = {
                let mut store = self.state.type_uses.write();
                store.retain(|uri, _| open_uris.contains(uri));
                for (uri, _, used, _, _) in &mut results {
                    store.insert(uri.clone(), used.take().unwrap_or_default());
                }
                for (uri, used) in open_uses {
                    store.insert(uri, used);
                }
                let mut merged = UsedInstances::default();
                for uses in store.values() {
                    merged.merge_from(uses);
                }
                merged
            };
            self.state
                .type_uses_revision
                .fetch_add(1, Ordering::Release);
            let no_lines = DocLines::none();
            for (uri, diagnostics, _, _, _) in &mut results {
                let file: cwtools_validation::FilePath = uri.as_str().into();
                for err in check_unused_instances(
                    prepared.ruleset,
                    scan_game,
                    &type_index_snap.instances_in_file(uri),
                    &merged,
                    &file,
                ) {
                    diagnostics.push(validation_error_to_diagnostic(&err, &no_lines));
                }
            }
        }
        if let Some(inline_scripts) = fresh_inline_scripts {
            *self.state.inline_scripts.write() = inline_scripts;
        }
        let results: Vec<(String, Vec<Diagnostic>, Option<u64>, InlineIgnoreMap)> = results
            .into_iter()
            .map(|(uri, diagnostics, _, source_hash, inline_ignored)| {
                (uri, diagnostics, source_hash, inline_ignored)
            })
            .collect();
        if cancel.is_cancelled() {
            return false;
        }
        let publish_total = results.len();

        let workspace_wide = {
            let cfg = self.state.config.read();
            cfg.workspace_wide_diagnostics
        };
        let mut closed_budget_remaining = if workspace_wide {
            WORKSPACE_DIAGNOSTICS_BUDGET
        } else {
            0
        };
        let mut published_this_scan = std::collections::HashSet::with_capacity(publish_total);

        self.enter_phase(&mut phase, progress, quiet, Phase::Publish, publish_total)
            .await;
        for (i, (uri, mut diagnostics, source_hash, inline_ignored)) in
            results.into_iter().enumerate()
        {
            phase.tick();
            crate::validate::drop_inline_suppressed(&mut diagnostics, &inline_ignored);

            let mut file_has_error = false;
            for d in &diagnostics {
                match d.severity {
                    Some(DiagnosticSeverity::ERROR) => {
                        total_errors += 1;
                        file_has_error = true;
                    }
                    Some(DiagnosticSeverity::WARNING) => total_warnings += 1,
                    Some(DiagnosticSeverity::INFORMATION) => total_infos += 1,
                    Some(DiagnosticSeverity::HINT) => total_hints += 1,
                    _ => {}
                }
            }
            if file_has_error {
                files_with_errors += 1;
            }

            let is_open = open_uris.contains(&uri);
            if !is_open {
                let previously_published =
                    { self.state.published_workspace_uris.lock().contains(&uri) };
                let publish_diagnostics = closed_budget_remaining > 0;
                if publish_diagnostics {
                    closed_budget_remaining -= 1;
                    if let Ok(uri_obj) = Url::parse(&uri) {
                        self.publish_filtered(uri_obj, diagnostics, None, source_hash)
                            .await;
                    }
                    self.state
                        .published_workspace_uris
                        .lock()
                        .insert(uri.clone());
                    published_this_scan.insert(uri);
                } else if previously_published && let Ok(uri_obj) = Url::parse(&uri) {
                    self.publish_filtered(uri_obj, Vec::new(), None, source_hash)
                        .await;
                }
            }

            if publish_throttle.should_wait_after(i) {
                publish_throttle.wait().await;
                tokio::task::yield_now().await;
                if cancel.is_cancelled() {
                    return false;
                }
            }
        }

        let mut clear_budget_remaining = WORKSPACE_DIAGNOSTICS_CLEAR_BUDGET;
        let stale_uris: Vec<String> = {
            let set = self.state.published_workspace_uris.lock();
            set.iter()
                .filter(|u| !published_this_scan.contains(u.as_str()))
                .cloned()
                .collect()
        };
        let held_back_clears = stale_uris.len().saturating_sub(clear_budget_remaining);
        for uri in stale_uris {
            if clear_budget_remaining == 0 {
                break;
            }
            clear_budget_remaining -= 1;
            self.state.published_workspace_uris.lock().remove(&uri);
            if let Ok(uri_obj) = Url::parse(&uri) {
                self.publish_filtered(uri_obj, Vec::new(), None, None).await;
            }
        }

        *self.state.last_scan_summary.lock() = Some(ScanSummary {
            total_files,
            validated_files: publish_total,
            files_with_errors,
            total_errors,
            total_warnings,
            total_infos,
            total_hints,
        });

        if workspace_wide
            && closed_budget_remaining == 0
            && publish_total > WORKSPACE_DIAGNOSTICS_BUDGET
        {
            let held_back = publish_total.saturating_sub(WORKSPACE_DIAGNOSTICS_BUDGET);
            tracing::info!(
                held_back,
                clear_held_back = held_back_clears,
                budget = WORKSPACE_DIAGNOSTICS_BUDGET,
                "workspace diagnostics budget exhausted; held back closed-file notifications"
            );
            self.client
                .log_message(
                    MessageType::INFO,
                    format!(
                        "Workspace diagnostics budget reached: {} files published, {} held back ({} stale clears deferred).",
                        WORKSPACE_DIAGNOSTICS_BUDGET, held_back, held_back_clears
                    ),
                )
                .await;
        }

        drop(parsed_files);

        self.end_phase(&mut phase).await;
        self.client
            .log_message(
                MessageType::INFO,
                format!(
                    "Workspace validation complete: {} errors across {} files",
                    total_errors, total_files
                ),
            )
            .await;

        let ws_prefix = self.state.config.read().workspace_prefix.clone();
        let file_list: Vec<serde_json::Value> = scan_files
            .iter()
            .map(|file| {
                let logical_path = logical_path_from_uri(&file.uri, &ws_prefix);
                let scope = logical_path
                    .split('/')
                    .next()
                    .unwrap_or("unknown")
                    .to_string();
                serde_json::json!({
                    "scope": scope,
                    "uri": file.uri.clone(),
                    "logicalpath": logical_path
                })
            })
            .collect();
        self.send_update_file_list(file_list).await;

        if !scan_files.is_empty() {
            *self.state.last_scan_fingerprint.lock() = Some((scan_fingerprint, scan_generation));
        }

        if cwtools_profiling::profile_enabled() {
            let st = self.state.string_table.stats();
            let info_summary = self.state.info_service.read().profile_summary();
            let vanilla = self
                .state
                .vanilla_index
                .lock()
                .as_ref()
                .map(|m| m.values().map(|v| v.len()).sum::<usize>())
                .unwrap_or(0);
            let loc_keys = self
                .state
                .loc_index
                .read()
                .as_deref()
                .map(|i| i.union().len())
                .unwrap_or(0);
            tracing::info!(target: "cwtools::profile", "{}", info_summary);
            tracing::info!(target: "cwtools::profile",
                "string_table {} MiB ({} entries) | vanilla_index {} instances | loc union {} keys",
                st.total_bytes() / (1024 * 1024), st.entries, vanilla, loc_keys);
        }
        cwtools_profiling::log_rss("workspace_scan_done");
        cwtools_profiling::trim_memory();
        cwtools_profiling::log_rss("after_trim");

        self.revalidate_all_open_docs(crate::ValidateTrigger::Reindex)
            .await;
        // and ask the client to re-request visible files (#184).
        self.invalidate_all_semantic_tokens();
        self.request_semantic_refresh().await;
        self.request_code_lens_refresh().await;
        true
    }

    /// a fresh registry (#259). Re-reads rather than reuses pass 1's ASTs —
    fn rebuild_inline_scripts(
        &self,
        scan_files: &[ScannedFile],
    ) -> cwtools_validation::InlineScripts {
        let ws_prefix = self.state.config.read().workspace_prefix.clone();
        let mut registry = cwtools_validation::InlineScripts::default();
        for file in scan_files {
            let logical_path = logical_path_from_uri(&file.uri, &ws_prefix);
            if !cwtools_validation::InlineScripts::is_script_path(&logical_path) {
                continue;
            }
            let open_text = self
                .state
                .documents
                .lock()
                .get(&file.uri)
                .map(|doc| doc.text.to_string());
            let text = match open_text {
                Some(text) => text,
                None => match cwtools_file_manager::file_manager::read_text_capped(
                    &file.path,
                    crate::access::MAX_URI_READ_BYTES,
                ) {
                    Ok((text, _)) => text,
                    Err(error) => {
                        tracing::warn!(
                            path = %file.path.display(),
                            error = %error,
                            "scan: skipping unreadable inline script"
                        );
                        continue;
                    }
                },
            };
            let parsed = parse_string_without_comments(&text, &self.state.string_table);
            registry.insert(&logical_path, parsed);
        }
        registry
    }

    #[cfg(test)]
    fn hold_pass2(&self, point: crate::state::Pass2HoldPoint) {
        let gate = self.state.pass2_gate.lock().clone();
        if let Some(gate) = gate {
            gate.hold(point);
        }
    }

    pub(crate) async fn revalidate_all_open_docs(&self, trigger: crate::ValidateTrigger) {
        let Ok(_validation_permit) = self.state.validation_permits.acquire().await else {
            return;
        };
        let open_docs: Vec<OpenDocSnapshot> = {
            let docs = self.state.documents.lock();
            docs.iter()
                .map(|(uri, doc)| {
                    let current_ast = match &doc.ast {
                        Some(ast) if doc.ast_version == Some(doc.version) => Some(ast.clone()),
                        _ => None,
                    };
                    (uri.clone(), doc.text.clone(), doc.version, current_ast)
                })
                .collect()
        };
        let (game, encoding) = {
            let cfg = self.state.config.read();
            (cfg.game(), cfg.position_encoding.clone())
        };
        let (ruleset_snap, registry_snap, modifier_keys_snap) = {
            let rules = self.state.rules.read();
            (
                rules.ruleset.clone(),
                rules.scope_registry.clone(),
                rules.modifier_keys.clone(),
            )
        };
        for (uri, text, version, current_ast) in open_docs {
            if self.is_ignored_uri(&uri) {
                self.clear_ignored_file_state(&uri);
                self.update_doc_tokens(&uri, None);
                let still_current = {
                    let docs = self.state.documents.lock();
                    docs.get(&uri)
                        .map(|d| d.version == version)
                        .unwrap_or(false)
                };
                if !still_current {
                    continue;
                }
                if let Ok(uri_obj) = Url::parse(&uri) {
                    self.publish_filtered(
                        uri_obj,
                        Vec::new(),
                        Some(version),
                        Some(cwtools_cache::workspace::content_hash(&text)),
                    )
                    .await;
                }
                continue;
            }
            let diagnostics = match current_ast {
                Some(ast) => {
                    let lines = DocLines::new(&text, encoding.clone());
                    let diags = match ruleset_snap.as_ref() {
                        Some(ruleset) => self.validate_parsed_prebuilt(
                            &uri,
                            &ast,
                            &modifier_keys_snap,
                            ruleset,
                            game,
                            registry_snap.as_ref(),
                            &lines,
                        ),
                        None => parse_errors_to_diagnostics(&ast.errors, &lines),
                    };
                    tracing::info!(
                        target: "cwtools::profile",
                        "[validate] ({}) {} diagnostics (prebuilt, no reparse)",
                        trigger.as_str(),
                        diags.len()
                    );
                    diags
                }
                None => {
                    self.parse_and_validate(&uri, &text, trigger, Some(version))
                        .await
                        .0
                }
            };
            let still_current = {
                let docs = self.state.documents.lock();
                docs.get(&uri)
                    .map(|d| d.version == version)
                    .unwrap_or(false)
            };
            if !still_current {
                continue;
            }
            if let Ok(uri_obj) = Url::parse(&uri) {
                self.publish_filtered(
                    uri_obj,
                    diagnostics,
                    Some(version),
                    Some(cwtools_cache::workspace::content_hash(&text)),
                )
                .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    use tower_lsp::lsp_types::Url;

    use cwtools_rules::rules_types::{PathOptions, RuleSet, TypeDefinition};
    use cwtools_validation::references::UsedInstances;

    use crate::command_progress::CommandProgress;
    use crate::state::{DocumentState, Pass2Gate, Pass2HoldPoint};

    const SENTINEL_URI: &str = "sentinel://uses";
    const SENTINEL_FP: (u64, u64) = (1, 1);
    const SENTINEL_REV: u64 = 7;

    fn test_backend() -> Backend {
        test_backend_with_socket().0
    }

    fn test_backend_with_socket() -> (Backend, tower_lsp::ClientSocket) {
        let state = Arc::new(DocumentState::new());
        let captured = Arc::new(parking_lot::Mutex::new(None));
        let slot = captured.clone();
        let server_state = state.clone();
        let (_service, socket) = tower_lsp::LspService::new(move |client| {
            *slot.lock() = Some(client.clone());
            Backend {
                client,
                state: server_state.clone(),
            }
        });
        let client = captured.lock().take().unwrap();
        (Backend { client, state }, socket)
    }

    fn clone_backend(backend: &Backend) -> Backend {
        Backend {
            client: backend.client.clone(),
            state: backend.state.clone(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn workspace_scan_applies_injected_publish_throttle_at_batch_boundary() {
        assert_eq!(
            DEFAULT_WORKSPACE_PUBLISH_THROTTLE.interval,
            std::time::Duration::ZERO,
            "normal unit tests must not wait for the production throttle"
        );
        assert_eq!(
            PRODUCTION_WORKSPACE_PUBLISH_THROTTLE.interval,
            std::time::Duration::from_millis(1)
        );
        assert!(!PRODUCTION_WORKSPACE_PUBLISH_THROTTLE.should_wait_after(48));
        assert!(PRODUCTION_WORKSPACE_PUBLISH_THROTTLE.should_wait_after(49));
        assert!(!PRODUCTION_WORKSPACE_PUBLISH_THROTTLE.should_wait_after(50));

        let (backend, _tmp) = setup_error_workspace(WORKSPACE_PUBLISH_BATCH_SIZE + 1);
        let wait_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let throttle = WorkspacePublishThrottle::with_wait_count(
            PRODUCTION_WORKSPACE_PUBLISH_THROTTLE.interval,
            wait_count.clone(),
        );
        let progress =
            CommandProgress::for_tests(backend.state.clone(), Arc::new(AtomicBool::new(false)));
        assert!(
            backend
                .validate_entire_workspace_inner(false, Some(&progress), throttle)
                .await
        );
        assert_eq!(
            backend
                .state
                .last_scan_summary
                .lock()
                .as_ref()
                .expect("completed scan must record a summary")
                .validated_files,
            WORKSPACE_PUBLISH_BATCH_SIZE + 1,
            "the scan must validate the deterministic file count"
        );
        assert_eq!(
            wait_count.load(Ordering::Relaxed),
            1,
            "a 51-file scan must throttle once after its 50th publish entry"
        );
    }

    fn setup_workspace(gate: Option<Arc<Pass2Gate>>) -> (Backend, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let things = tmp.path().join("common/things");
        std::fs::create_dir_all(&things).unwrap();
        for (i, name) in ["a.txt", "b.txt", "c.txt"].iter().enumerate() {
            std::fs::write(things.join(name), format!("thing_{i} = {{ }}\n")).unwrap();
        }
        let ws_uri = Url::from_file_path(tmp.path()).unwrap();
        let backend = test_backend();
        {
            let mut cfg = backend.state.config.write();
            cfg.workspace_uri = Some(ws_uri.as_str().into());
            cfg.workspace_prefix = Some(crate::paths::workspace_prefix_of(ws_uri.as_str()));
        }
        let mut ruleset = RuleSet::new();
        ruleset.types.push(TypeDefinition {
            name: "thing".to_string(),
            name_field: None,
            path_options: PathOptions {
                paths: vec!["common/things".to_string()],
                ..Default::default()
            },
            subtypes: Vec::new(),
            type_key_filter: None,
            skip_root_key: Vec::new(),
            starts_with: None,
            type_per_file: false,
            key_prefix: None,
            warning_only: false,
            unique: false,
            should_be_referenced: true,
            localisation: Vec::new(),
            graph_related_types: Vec::new(),
            modifiers: Vec::new(),
        });
        ruleset.reindex();
        backend.state.rules.write().ruleset = Some(Arc::new(ruleset));
        let mut uses = UsedInstances::default();
        uses.mark("sentinel_type", "sentinel_instance");
        backend
            .state
            .type_uses
            .write()
            .insert(SENTINEL_URI.to_string(), uses);
        backend
            .state
            .type_uses_revision
            .store(SENTINEL_REV, Ordering::Release);
        *backend.state.last_scan_fingerprint.lock() = Some(SENTINEL_FP);
        if let Some(gate) = gate {
            *backend.state.pass2_gate.lock() = Some(gate);
        }
        (backend, tmp)
    }

    fn assert_stores_unchanged(backend: &Backend) {
        assert!(
            backend.state.type_uses.read().contains_key(SENTINEL_URI),
            "cancelled pass 2 must not merge type_uses"
        );
        assert_eq!(
            backend.state.type_uses_revision.load(Ordering::Acquire),
            SENTINEL_REV,
            "type_uses_revision must be unchanged on cancel"
        );
        assert_eq!(
            *backend.state.last_scan_fingerprint.lock(),
            Some(SENTINEL_FP),
            "last_scan_fingerprint must be unchanged on cancel"
        );
    }

    async fn wait_arrived(gate: &Pass2Gate, point: Pass2HoldPoint) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if gate.has_arrived() {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "pass 2 never reached {point:?}"
            );
            tokio::task::yield_now().await;
        }
    }

    async fn run_cancelled_at(point: Pass2HoldPoint) {
        let gate = Pass2Gate::new(point);
        let cancel = Arc::new(AtomicBool::new(false));
        let (backend, _tmp) = setup_workspace(Some(gate.clone()));
        let scan_backend = clone_backend(&backend);
        let scan_cancel = cancel.clone();
        let handle = tokio::spawn(async move {
            let progress = CommandProgress::for_tests(scan_backend.state.clone(), scan_cancel);
            scan_backend
                .validate_entire_workspace_tracked(false, Some(&progress))
                .await
        });
        wait_arrived(&gate, point).await;
        cancel.store(true, Ordering::Relaxed);
        gate.release();
        let outcome = handle.await.expect("scan panicked");
        assert_eq!(outcome, ScanOutcome::Cancelled);
        assert_stores_unchanged(&backend);
    }

    async fn assert_pass2_allows_write(
        lock_name: &str,
        write: impl FnOnce(&DocumentState) + Send + 'static,
    ) -> Backend {
        let gate = Pass2Gate::new(Pass2HoldPoint::Before);
        let (backend, _tmp) = setup_workspace(Some(gate.clone()));
        let scan_backend = clone_backend(&backend);
        let handle = tokio::spawn(async move {
            let progress = CommandProgress::for_tests(
                scan_backend.state.clone(),
                Arc::new(AtomicBool::new(false)),
            );
            scan_backend
                .validate_entire_workspace_inner(
                    false,
                    Some(&progress),
                    DEFAULT_WORKSPACE_PUBLISH_THROTTLE,
                )
                .await
        });
        wait_arrived(&gate, Pass2HoldPoint::Before).await;

        let state = backend.state.clone();
        let mut writer = tokio::task::spawn_blocking(move || write(&state));
        let acquired = tokio::time::timeout(std::time::Duration::from_secs(5), &mut writer).await;
        let acquired_before_release = acquired.is_ok();
        gate.release();
        let writer_result = match acquired {
            Ok(result) => result,
            Err(_) => writer.await,
        };
        let scan_result = match handle.await {
            Ok(result) => result,
            Err(error) => panic!("scan panicked: {error}"),
        };

        if let Err(error) = writer_result {
            panic!("writer panicked: {error}");
        }
        assert!(scan_result, "scan must complete");
        assert!(
            acquired_before_release,
            "pass 2 blocked concurrent {lock_name}.write()"
        );
        backend
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_pass2_snapshot_preserves_concurrent_info_service_mutation() {
        let backend = assert_pass2_allows_write("info_service", |state| {
            let mut info = state.info_service.write();
            Arc::make_mut(&mut info.type_index)
                .file_index
                .insert("concurrent.txt");
        })
        .await;

        assert!(
            backend
                .state
                .info_service
                .read()
                .type_index
                .file_index
                .contains("concurrent.txt"),
            "pass 2 must not discard the live copy-on-write mutation"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_pass2_snapshot_allows_loc_index_write() {
        assert_pass2_allows_write("loc_index", |state| {
            drop(state.loc_index.write());
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_pass2_completed_scan_merges_type_uses() {
        let (backend, _tmp) = setup_workspace(None);
        let progress =
            CommandProgress::for_tests(backend.state.clone(), Arc::new(AtomicBool::new(false)));
        let outcome = backend
            .validate_entire_workspace_tracked(false, Some(&progress))
            .await;
        assert_eq!(outcome, ScanOutcome::Ran);
        assert!(
            !backend.state.type_uses.read().contains_key(SENTINEL_URI),
            "a finished pass must merge type_uses and drop the sentinel"
        );
        assert!(
            backend.state.type_uses_revision.load(Ordering::Acquire) > SENTINEL_REV,
            "type_uses_revision must bump on a finished pass"
        );
        assert_ne!(
            *backend.state.last_scan_fingerprint.lock(),
            Some(SENTINEL_FP),
            "last_scan_fingerprint must update on a finished pass"
        );
        assert!(
            backend.state.last_scan_fingerprint.lock().is_some(),
            "finished pass must record a fingerprint"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_pass2_cancel_before_first_chunk_skips_merge() {
        run_cancelled_at(Pass2HoldPoint::Before).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_pass2_cancel_mid_chunk_skips_merge() {
        run_cancelled_at(Pass2HoldPoint::Mid).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_pass2_cancel_between_chunks_skips_merge() {
        run_cancelled_at(Pass2HoldPoint::After).await;
    }

    type Row = (usize, usize, u64, u32);
    type Inputs = (Vec<usize>, Vec<Option<usize>>, Vec<Option<u64>>, Vec<u32>);

    fn inputs(n: usize) -> Inputs {
        (
            (0..n).collect(),
            (0..n).map(|i| (i % 7 != 3).then_some(i * 2)).collect(),
            (0..n).map(|i| Some(i as u64 * 3)).collect(),
            (0..n).map(|i| i as u32 * 7).collect(),
        )
    }

    fn take(a: &usize, b: &Option<usize>, c: &Option<u64>, d: &u32) -> Option<Row> {
        Some((*a, (*b)?, (*c)?, *d))
    }

    fn sequential(a: &[usize], b: &[Option<usize>], c: &[Option<u64>], d: &[u32]) -> Vec<Row> {
        a.iter()
            .zip(b)
            .zip(c)
            .zip(d)
            .filter_map(|(((a, b), c), d)| take(a, b, c, d))
            .collect()
    }

    async fn walk(n: usize, chunk: usize) -> Option<Vec<Row>> {
        let (a, b, c, d) = inputs(n);
        chunked_par_filter_map(
            &a,
            &b,
            &c,
            &d,
            chunk,
            &CancelFlag::inert(),
            ChunkMapper {
                map: take,
                before_chunk: || {},
                after_chunk: || {},
            },
        )
        .await
    }

    const SIZES: [usize; 7] = [0, 1, 255, 256, 257, 300, 512];

    #[tokio::test]
    async fn chunked_walk_matches_unchunked_at_every_boundary() {
        for n in SIZES {
            let (a, b, c, d) = inputs(n);
            let expected = sequential(&a, &b, &c, &d);
            assert_eq!(
                walk(n, PASS2_CHUNK_SIZE).await,
                Some(expected.clone()),
                "chunked walk of {n} elements"
            );
            assert_eq!(
                walk(n, usize::MAX).await,
                Some(expected),
                "single-chunk walk of {n} elements"
            );
        }
    }

    #[tokio::test]
    async fn chunked_walk_is_independent_of_chunk_size() {
        let unchunked = walk(300, usize::MAX).await;
        for chunk in [1, 2, 7, 255, 256, 257, 299, 300, 301] {
            assert_eq!(
                walk(300, chunk).await,
                unchunked,
                "300 elements in chunks of {chunk}"
            );
        }
    }

    #[tokio::test]
    async fn cancelled_walk_returns_none_not_a_partial_result() {
        let (a, b, c, d) = inputs(300);
        let cancelled = CancelFlag::cancelled_for_tests();
        let out = chunked_par_filter_map(
            &a,
            &b,
            &c,
            &d,
            PASS2_CHUNK_SIZE,
            &cancelled,
            ChunkMapper {
                map: take,
                before_chunk: || {},
                after_chunk: || {},
            },
        )
        .await;
        assert_eq!(out, None);
    }

    fn setup_error_workspace(n: usize) -> (Backend, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let things = tmp.path().join("common/things");
        std::fs::create_dir_all(&things).unwrap();
        for i in 0..n {
            std::fs::write(things.join(format!("{i}.txt")), "thing = {\n").unwrap();
        }
        let ws_uri = Url::from_file_path(tmp.path()).unwrap();
        let backend = test_backend();
        {
            let mut cfg = backend.state.config.write();
            cfg.workspace_uri = Some(ws_uri.as_str().into());
            cfg.workspace_prefix = Some(crate::paths::workspace_prefix_of(ws_uri.as_str()));
        }
        (backend, tmp)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_scan_summary_counts_errors_and_files() {
        let (backend, _tmp) = setup_error_workspace(3);
        let progress =
            CommandProgress::for_tests(backend.state.clone(), Arc::new(AtomicBool::new(false)));
        let outcome = backend
            .validate_entire_workspace_tracked(false, Some(&progress))
            .await;
        assert_eq!(outcome, ScanOutcome::Ran);
        let summary_guard = backend.state.last_scan_summary.lock();
        let summary = summary_guard
            .as_ref()
            .expect("a completed scan must store a summary");
        assert_eq!(
            summary.total_files, 3,
            "summary must count all workspace files"
        );
        assert_eq!(
            summary.validated_files, 3,
            "validated files must equal the result set"
        );
        assert_eq!(
            summary.files_with_errors, 3,
            "every malformed file must carry an error"
        );
        assert!(
            summary.total_errors > 0,
            "summary must record positive error count"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_workspace_diagnostics_disabled_clears_previous() {
        let (backend, _tmp) = setup_error_workspace(2);
        let progress =
            CommandProgress::for_tests(backend.state.clone(), Arc::new(AtomicBool::new(false)));
        let outcome = backend
            .validate_entire_workspace_tracked(false, Some(&progress))
            .await;
        assert_eq!(outcome, ScanOutcome::Ran);
        assert_eq!(
            backend.state.published_workspace_uris.lock().len(),
            2,
            "closed files should be published when workspace-wide is on"
        );

        backend.state.config.write().workspace_wide_diagnostics = false;
        let progress2 =
            CommandProgress::for_tests(backend.state.clone(), Arc::new(AtomicBool::new(false)));
        let outcome2 = backend
            .validate_entire_workspace_tracked(false, Some(&progress2))
            .await;
        assert_eq!(outcome2, ScanOutcome::Ran);
        assert!(
            backend.state.published_workspace_uris.lock().is_empty(),
            "disabled workspace-wide diagnostics must clear closed-file publishes"
        );
        let summary_guard = backend.state.last_scan_summary.lock();
        let summary = summary_guard
            .as_ref()
            .expect("summary is still captured when publishing is disabled");
        assert!(
            summary.total_errors > 0,
            "summary must still count errors even when they are not published"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_workspace_diagnostics_budget_caps_closed_files() {
        let (backend, _tmp) = setup_error_workspace(4);
        let progress =
            CommandProgress::for_tests(backend.state.clone(), Arc::new(AtomicBool::new(false)));
        let outcome = backend
            .validate_entire_workspace_tracked(false, Some(&progress))
            .await;
        assert_eq!(outcome, ScanOutcome::Ran);
        let published = backend.state.published_workspace_uris.lock();
        assert_eq!(
            published.len(),
            WORKSPACE_DIAGNOSTICS_BUDGET,
            "only the budgeted number of closed files should be published"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_missing_workspace_root_logs_error_to_client() {
        use futures_util::stream::StreamExt;

        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("no_such_workspace");
        let missing_uri = Url::from_file_path(&missing).unwrap();
        let (backend, socket) = test_backend_with_socket();
        {
            let mut cfg = backend.state.config.write();
            cfg.workspace_uri = Some(missing_uri.as_str().into());
            cfg.workspace_prefix = Some(crate::paths::workspace_prefix_of(missing_uri.as_str()));
        }

        let expected_root = missing.to_string_lossy().to_string();
        let expected_root_for_collector = expected_root.clone();
        let (found_tx, found_rx) = tokio::sync::oneshot::channel();
        let collector = tokio::spawn(async move {
            let mut socket = socket;
            let mut found_tx = Some(found_tx);
            while let Some(req) = socket.next().await {
                if req.method() != "window/logMessage" {
                    continue;
                }
                let Some(params) = req.params() else {
                    continue;
                };
                let Some(msg_type) = params["type"].as_i64() else {
                    continue;
                };
                let Some(msg) = params["message"].as_str() else {
                    continue;
                };
                if msg_type == 1
                    && msg.starts_with("error: discovery failed for")
                    && msg.contains(&expected_root_for_collector)
                    && let Some(tx) = found_tx.take()
                {
                    let _ = tx.send(msg.to_string());
                }
            }
        });

        let scan = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            backend.validate_entire_workspace(false),
        )
        .await
        .expect("missing-root workspace validation timed out");
        assert!(scan);
        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), found_rx)
            .await
            .expect("missing-root log message timed out")
            .expect("missing-root log collector stopped");
        assert!(
            msg.starts_with("error: discovery failed for"),
            "message must match CLI parity prefix, got: {msg}"
        );
        assert!(
            msg.contains(&expected_root),
            "message must name the missing root, got: {msg}"
        );
        collector.abort();
        let _ = collector.await;
    }

    /// #221: a scan with no client token behind it — the startup scan — used to
    #[tokio::test(flavor = "multi_thread")]
    async fn a_tokenless_phase_is_still_sampled() {
        let backend = test_backend();
        let ticker = start_phase(&backend, None, false, Phase::Validate, 4);
        ticker.tick();
        let summary = ticker
            .stop()
            .expect("a phase with no command token behind it must still be sampled");
        assert!(
            summary.contains(Phase::Validate.label()),
            "the phase summary must name its phase, got {summary}"
        );
    }

    /// #435: `enter_phase`'s `quiet` short-circuit used to drop phase
    #[tokio::test(flavor = "multi_thread")]
    async fn a_quiet_phase_with_a_token_still_reports_against_it() {
        let backend = test_backend();
        let progress = CommandProgress::begin(
            &backend,
            Some(ProgressToken::String("test/435".to_string())),
            "test",
            false,
        )
        .await;
        let mut ticker = PhaseTicker::inert();
        backend
            .enter_phase(&mut ticker, Some(&progress), true, Phase::Validate, 4)
            .await;
        assert!(
            progress.reports_sent() >= 1,
            "the phase boundary must reach the command token"
        );
        ticker.tick();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while progress.reports_sent() < 2 && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            progress.reports_sent() >= 2,
            "a sampler tick must reach the command token"
        );
        let summary = ticker
            .stop()
            .expect("a quiet phase carrying a command token must still be sampled");
        assert!(
            summary.contains(Phase::Validate.label()),
            "the phase summary must name its phase, got {summary}"
        );
        assert!(
            !backend.state.loading_bar_active.load(Ordering::SeqCst),
            "a quiet pass must never touch the server's own loadingBar indicator"
        );
        progress.finish(None).await;
    }

    /// silent, unchanged from before #435.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_quiet_phase_with_no_token_stays_inert() {
        let backend = test_backend();
        let mut ticker = PhaseTicker::inert();
        backend
            .enter_phase(&mut ticker, None, true, Phase::Validate, 4)
            .await;
        ticker.tick();
        assert!(
            ticker.stop().is_none(),
            "a quiet pass with no command token must get an inert ticker"
        );
        assert!(!backend.state.loading_bar_active.load(Ordering::SeqCst));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_running_phase_reports_a_heartbeat() {
        use futures_util::stream::StreamExt;

        let (backend, socket) = test_backend_with_socket();
        let (found_tx, found_rx) = tokio::sync::oneshot::channel();
        let collector = tokio::spawn(async move {
            let mut socket = socket;
            let mut found_tx = Some(found_tx);
            while let Some(req) = socket.next().await {
                if req.method() != "window/logMessage" {
                    continue;
                }
                let Some(params) = req.params() else { continue };
                let Some(msg) = params["message"].as_str() else {
                    continue;
                };
                if msg.starts_with("Scan phase still running:")
                    && let Some(tx) = found_tx.take()
                {
                    let _ = tx.send(msg.to_string());
                }
            }
        });

        let ticker = start_phase(&backend, None, false, Phase::Validate, 4);
        ticker.tick();
        let msg = tokio::time::timeout(std::time::Duration::from_secs(20), found_rx)
            .await
            .expect("a phase past the heartbeat interval must report one")
            .expect("heartbeat collector stopped");
        assert!(
            msg.contains(Phase::Validate.label()),
            "the heartbeat must name its phase, got {msg}"
        );
        assert!(
            msg.contains("1/4"),
            "the heartbeat must say how far the phase got, got {msg}"
        );
        ticker.stop();
        collector.abort();
        let _ = collector.await;
    }

    /// the shortest, so without the assert a length-invariant bug upstream would
    #[tokio::test]
    #[should_panic]
    async fn mismatched_lengths_panic_rather_than_truncate() {
        let (a, b, c, d) = inputs(300);
        let _ = chunked_par_filter_map(
            &a[..299],
            &b,
            &c,
            &d,
            PASS2_CHUNK_SIZE,
            &CancelFlag::inert(),
            ChunkMapper {
                map: take,
                before_chunk: || {},
                after_chunk: || {},
            },
        )
        .await;
    }
}
