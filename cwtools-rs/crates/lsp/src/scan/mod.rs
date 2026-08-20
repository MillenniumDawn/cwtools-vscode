use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tower_lsp::Client;
use tower_lsp::lsp_types::*;

use cwtools_validation::build_modifier_keys;

use crate::command_progress::CommandProgress;
use crate::{Backend, DocumentState, LoadingBar, UpdateFileList};

mod loc;
mod reindex;
mod vanilla;
mod watched;
mod workspace;

pub(crate) use vanilla::{VanillaLoc, VanillaLocKey};

/// Aggregate counts captured from a completed workspace validation pass,
/// returned by the `validateWorkspace` execute command and stored so later
/// callers can read the last result without re-running the scan.
#[derive(Debug, Clone, Default)]
pub(crate) struct ScanSummary {
    /// Total workspace files discovered on disk this pass.
    pub total_files: usize,
    /// Files that were validated (closed files in the scan's result set).
    pub validated_files: usize,
    pub files_with_errors: usize,
    pub total_errors: usize,
    pub total_warnings: usize,
    pub total_infos: usize,
    pub total_hints: usize,
}

/// Trailing window for coalescing `didChangeWatchedFiles` create/modify events.
/// Fixed (not a sliding reset) so a continuous churn stream still drains.
const WATCHED_DEBOUNCE_MS: u64 = 500;
/// Above this many distinct files in one window, validate the whole workspace
/// once (a rules re-clone / git checkout) instead of per file.
const WATCHED_BULK_CAP: usize = 200;

/// Test-only one-shot panic switch for `CWTOOLS_WATCHED_BATCH_PANIC_ONCE`
/// (#155): fires at most once per server process, so the e2e suite can
/// exercise a background task's panic recovery without leaving the injected
/// panic armed for every later pass.
static WATCHED_BATCH_PANIC_ONCE: AtomicBool = AtomicBool::new(true);

/// True when `name` is set to a truthy value (`1`, `true`, `yes`, `on`) — same
/// convention as `cwtools_profiling::profile_enabled`, so `VAR=0` or an empty
/// value (a shell habit for "unset") doesn't accidentally arm a test hook.
pub(crate) fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

/// One open document captured for a post-scan / config-change revalidation: its
/// uri, current text, version, and — when the cached AST still matches the
/// version — that AST (a current AST routes the doc through the no-reparse
/// prebuilt path; `None` falls back to a full re-parse).
type OpenDocSnapshot = (
    String,
    Arc<str>,
    i32,
    Option<Arc<cwtools_parser::ast::ParsedFile>>,
);

/// One discovered workspace file, with its URI built once for every scan pass.
struct ScannedFile {
    path: std::path::PathBuf,
    uri: String,
}

/// RAII guard for the loading indicator and, for a full scan, the
/// `scan_in_progress` flag. The guard lives inside the awaiting future, so a
/// panic unwinding through it and a dropped future both still run `Drop`: a
/// scan can't wedge every later scan out forever, and the bar can't be left
/// spinning.
///
/// The dropped-future case is a client cancelling a long
/// `workspace/executeCommand` (#204). `tower-lsp` answers `$/cancelRequest` by
/// dropping the handler, so the work never reaches the bar-off its normal exit
/// sends. Cancellation stays best-effort otherwise: indexing already done is
/// kept, not rolled back.
///
/// [`ScanGuard::finish`] is the normal exit and does the same work inline.
pub(crate) struct ScanGuard {
    client: Client,
    state: Arc<DocumentState>,
    /// Whether this guard also holds `scan_in_progress`. `cacheVanilla` drives
    /// the bar without taking the scan flag, and must not release someone
    /// else's.
    owns_scan: bool,
    /// A quiet scan sends no progress at all, so there is nothing to close.
    quiet: bool,
    finished: bool,
}

impl ScanGuard {
    fn for_scan(backend: &Backend, quiet: bool) -> Self {
        Self {
            client: backend.client.clone(),
            state: backend.state.clone(),
            owns_scan: true,
            quiet,
            finished: false,
        }
    }

    /// For a command that drives the bar outside the scan guard.
    pub(crate) fn for_command(backend: &Backend) -> Self {
        Self {
            client: backend.client.clone(),
            state: backend.state.clone(),
            owns_scan: false,
            quiet: false,
            finished: false,
        }
    }

    /// Close the indicator, then release the scan flag — in that order, so the
    /// next scan's `begin` can't be overtaken by this one's `end`.
    pub(crate) async fn finish(mut self) {
        self.finished = true;
        if !self.quiet {
            Backend {
                client: self.client.clone(),
                state: self.state.clone(),
            }
            .send_loading_bar(false, "")
            .await;
        }
        if self.owns_scan {
            self.state.scan_in_progress.store(false, Ordering::SeqCst);
        }
    }
}

impl Drop for ScanGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        // `Drop` can't await, so the close goes out on its own task, which then
        // releases the scan flag to keep `finish`'s ordering. `try_current`
        // because a guard dropped as the runtime tears down has no executor to
        // spawn on — release the flag and let the process exit.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) if !self.quiet => {
                let backend = Backend {
                    client: self.client.clone(),
                    state: self.state.clone(),
                };
                let owns_scan = self.owns_scan;
                handle.spawn(async move {
                    backend.send_loading_bar(false, "").await;
                    if owns_scan {
                        backend
                            .state
                            .scan_in_progress
                            .store(false, Ordering::SeqCst);
                    }
                });
            }
            _ if self.owns_scan => self.state.scan_in_progress.store(false, Ordering::SeqCst),
            _ => {}
        }
    }
}

/// Token for the scan's `$/progress` stream. A single fixed token is safe
/// because full scans are serialized by `scan_in_progress` and
/// `scan_progress_active` gates the `begin`/`end` pairing — reusing a token
/// while its progress is live is a protocol violation.
const SCAN_PROGRESS_TOKEN: &str = "cwtools/scan";

impl Backend {
    /// Report background indexing/validation progress.
    ///
    /// Two channels, deliberately: the custom `loadingBar` notification
    /// (`{ "enable": bool, "value": string }`) that drives the bundled VS Code
    /// extension's status bar, and the standard `$/progress` stream every other
    /// client understands. `cwtools-server` is a standalone binary, so in
    /// Neovim / Helix / Zed the custom notification is dropped on the floor and
    /// the initial index looks like a hang.
    ///
    /// Both go out from the one place so the phase strings can't drift apart.
    ///
    /// Closing what was never opened is dropped rather than sent: several
    /// callers clear the bar defensively (a cancelled scan's [`ScanGuard`], a
    /// `cacheVanilla` that hit a fresh cache and indexed nothing), and a client
    /// should see one close per open, not one per caller that thought about it.
    pub(crate) async fn send_loading_bar(&self, enable: bool, value: &str) {
        self.send_loading_bar_pct(None, enable, value, None).await;
    }

    /// [`send_loading_bar`] with a known position on the 0-100 bar, reported
    /// against `owner`'s stream when a command drove this scan.
    ///
    /// `owner` is the command whose `workspace/executeCommand` started the
    /// work, not whichever command started last: two commands overlapping (a
    /// `cacheVanilla` sent while a `reindexWorkspace` is still scanning) each
    /// keep their own `$/progress` stream. `None` is the startup scan and the
    /// periodic background pass, which report over the server's own stream.
    ///
    /// The percentage rides the `loadingBar` payload too (as an optional
    /// `percentage` field) so the extension's status bar can show it; a client
    /// on an older build just ignores the extra key.
    ///
    /// [`send_loading_bar`]: Backend::send_loading_bar
    pub(crate) async fn send_loading_bar_pct(
        &self,
        owner: Option<&CommandProgress>,
        enable: bool,
        value: &str,
        percentage: Option<u32>,
    ) {
        let was_active = self.state.loading_bar_active.swap(enable, Ordering::SeqCst);
        if !enable && !was_active {
            return;
        }
        let payload = match percentage {
            Some(pct) => {
                serde_json::json!({ "enable": enable, "value": value, "percentage": pct })
            }
            None => serde_json::json!({ "enable": enable, "value": value }),
        };
        self.client.send_notification::<LoadingBar>(payload).await;
        self.send_work_done_progress(owner, enable, value, percentage)
            .await;
    }

    /// The `$/progress` half of [`send_loading_bar`]. The first `enable` creates
    /// the token and begins; later ones report a new phase; `enable = false`
    /// ends it. Silent unless the client advertised `window.workDoneProgress` —
    /// a server-initiated progress needs `window/workDoneProgress/create`, and a
    /// client that didn't advertise support isn't required to answer it.
    async fn send_work_done_progress(
        &self,
        owner: Option<&CommandProgress>,
        enable: bool,
        value: &str,
        percentage: Option<u32>,
    ) {
        use tower_lsp::lsp_types::request::WorkDoneProgressCreate;
        // A command that passed its own `workDoneToken` owns this scan: its
        // phases report against that token and its `end` is sent by
        // `CommandProgress`, so opening the server's stream on top would show
        // two bars for one operation.
        let command_token = owner.and_then(CommandProgress::token).cloned();
        if let Some(token) = command_token
            && enable
        {
            // Phase updates only. `begin`/`end` belong to `CommandProgress`,
            // which outlives any single scan the command triggers.
            //
            // A close deliberately falls through to the server's stream
            // instead: the startup scan may still have `cwtools/scan` open when
            // the user hits Re-index, and swallowing its `end` here would leave
            // the client spinning on it forever. Below, an unopened stream is a
            // no-op, so falling through costs a command-owned scan nothing.
            self.client
                .send_notification::<tower_lsp::lsp_types::notification::Progress>(ProgressParams {
                    token,
                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::Report(
                        WorkDoneProgressReport {
                            // Unset: the command's `begin` already said whether
                            // it can be cancelled, and a report that omits this
                            // keeps that answer.
                            cancellable: None,
                            message: Some(value.to_string()),
                            percentage,
                        },
                    )),
                })
                .await;
            return;
        }
        if !self.state.client_work_done_progress.load(Ordering::Relaxed) {
            return;
        }
        let token = ProgressToken::String(SCAN_PROGRESS_TOKEN.to_string());
        let was_active = self.state.scan_progress_active.load(Ordering::SeqCst);
        let progress = match (enable, was_active) {
            (false, false) => return, // nothing live to end
            (false, true) => WorkDoneProgress::End(WorkDoneProgressEnd { message: None }),
            (true, true) => WorkDoneProgress::Report(WorkDoneProgressReport {
                cancellable: Some(false),
                message: Some(value.to_string()),
                percentage,
            }),
            (true, false) => {
                // The client may refuse the token; leave the stream closed so a
                // later phase creates it again instead of reporting against a
                // token that was never registered.
                if self
                    .client
                    .send_request::<WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
                        token: token.clone(),
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                WorkDoneProgress::Begin(WorkDoneProgressBegin {
                    title: "CWTools".to_string(),
                    cancellable: Some(false),
                    message: Some(value.to_string()),
                    percentage,
                })
            }
        };
        self.client
            .send_notification::<tower_lsp::lsp_types::notification::Progress>(ProgressParams {
                token,
                value: ProgressParamsValue::WorkDone(progress),
            })
            .await;
        // Only now is the stream open (or closed). A future dropped partway
        // through the create round-trip — a cancelled command — must not leave
        // an `end` owing on a token the client never saw begin.
        self.state
            .scan_progress_active
            .store(enable, Ordering::SeqCst);
    }

    /// Send the `updateFileList` server→client notification so the VS Code
    /// extension file explorer populates.
    /// Payload: `{ "fileList": [{ "scope": string, "uri": string, "logicalpath": string }] }`.
    async fn send_update_file_list(&self, file_list: Vec<serde_json::Value>) {
        let payload = serde_json::json!({ "fileList": file_list });
        self.client
            .send_notification::<UpdateFileList>(payload)
            .await;
    }

    /// Rebuild the cached modifier-key set and the expanded modifier→scopes map
    /// from the current ruleset and type index.
    pub(crate) fn rebuild_modifier_keys(&self) {
        // Lock order: rules -> info_service. One `rules` write guard holds the
        // ruleset we read from and the modifier data we write into.
        let mut rules = self.state.rules.write();
        let (keys, scopes) = match rules.ruleset.as_ref() {
            Some(rs) => {
                let info_guard = self.state.info_service.read();
                (
                    build_modifier_keys(rs, &info_guard.type_index),
                    crate::completion::expanded_modifier_scopes(rs, &info_guard.type_index),
                )
            }
            None => Default::default(),
        };
        rules.modifier_keys = Arc::new(keys);
        rules.modifier_scopes = Arc::new(scopes);
        drop(rules);
        self.bump_info_revision();
    }
}

/// Spawn `fut` on its own task and await it, turning a panic into a
/// `tracing::error!` instead of letting it vanish with a dropped `JoinHandle`
/// — the same pattern the startup scan's watcher in `initialized` uses, split
/// out so `run_reindex_pass`, `arm_watched_batch`'s debounce window, and
/// `initialized`'s own wrap of the whole `background_reindex_loop` task share
/// it (#155). `context` names the task in the log line. Returns whether `fut`
/// completed without panicking, so a caller holding state the task also
/// touched (the watched-batch queues) can react.
pub(crate) async fn spawn_logging_panics<F>(context: &str, fut: F) -> bool
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    match tokio::spawn(fut).await {
        Ok(()) => true,
        Err(e) => {
            tracing::error!("{context} panicked: {e}");
            false
        }
    }
}

/// Hold an in-flight scan open when a test asks for it, from just after the
/// loading bar so the scan-started signal is already out. `CWTOOLS_SCAN_HOLD_MS`
/// holds for a fixed span, which is enough when a test only needs the scan busy
/// while it sends something. `CWTOOLS_SCAN_HOLD_FILE` names a path and holds for
/// as long as it exists, so a test that also cares *when* the hold ends starts
/// and ends it on a signal it owns rather than betting on a wall-clock window
/// that parallel load can blow through (#198). Unset, which is every real run,
/// both are no-ops.
pub(crate) async fn hold_scan_for_tests() {
    if let Some(ms) = std::env::var("CWTOOLS_SCAN_HOLD_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }
    let Ok(gate) = std::env::var("CWTOOLS_SCAN_HOLD_FILE") else {
        return;
    };
    let gate = std::path::PathBuf::from(gate);
    while tokio::fs::try_exists(&gate).await.unwrap_or(false) {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// Fold a stat-only signature (path, size, mtime) over `files` into one hash,
/// in a deterministic (sorted-path) order so the result doesn't depend on
/// directory-walk order. Shared by the loc-rebuild skip and the whole-pass
/// short-circuit; split out from `Backend` so it's unit-testable without a
/// live `tower_lsp::Client`.
pub(crate) fn stat_signature_for(files: &[std::path::PathBuf]) -> u64 {
    // Sort by reference — the caller still owns `files`.
    let mut sorted: Vec<&std::path::Path> = files.iter().map(|p| p.as_path()).collect();
    sorted.sort_unstable();
    // Limitation: a same-length edit in the same second on a coarse-mtime fs (FAT/NFS) false-negatives the skip; acceptable, we don't content-hash.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for path in sorted {
        path.to_string_lossy().hash(&mut hasher);
        if let Ok(meta) = std::fs::metadata(path) {
            meta.len().hash(&mut hasher);
            if let Ok(modified) = meta.modified()
                && let Ok(since_epoch) = modified.duration_since(std::time::UNIX_EPOCH)
            {
                since_epoch.as_nanos().hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Whether `state.watched_debounce` currently holds a live (unfinished)
/// handle. Split out of `spawn_watched_batch_window`'s panic-recovery path so
/// the decision is independently unit-testable (#155 fix-round-2): at the
/// point the recovery path calls this, the slot can only hold either THIS
/// window's own handle — still unfinished, since we're suspended mid-recovery,
/// not returned — or nothing/a finished one if some later stage already
/// cleared or re-armed it (unreachable today; `arm_watched_batch`'s own gate
/// no-ops against a live handle, so nothing else can install one while ours
/// is running). `true` means "safe to overwrite with a retry"; `false` means
/// "something else already moved on, don't spawn a retry at all".
pub(crate) fn watched_batch_slot_is_ours(state: &DocumentState) -> bool {
    state
        .watched_debounce
        .lock()
        .as_ref()
        .is_some_and(|h| !h.is_finished())
}

/// Drop from `deletes` any URI that also arrived as a CHANGED/CREATED this
/// window: a delete coincident with a re-create (an atomic save's
/// remove+rewrite) is a change, not a delete of the index entry.
pub(crate) fn resolve_watched_deletes(
    changes: &HashSet<String>,
    deletes: impl Iterator<Item = String>,
) -> Vec<String> {
    deletes.filter(|uri| !changes.contains(uri)).collect()
}

/// Whether a coalesced watched batch (changes + deletes together) exceeds the
/// per-file cap and should collapse into one workspace rescan instead.
/// Saturating so an absurd count can't wrap.
pub(crate) fn watched_batch_over_cap(changes: usize, deletes: usize) -> bool {
    changes.saturating_add(deletes) > WATCHED_BULK_CAP
}

/// Whether a QUIET background pass can short-circuit the whole reindex +
/// revalidate: true only for a quiet pass with a non-empty walk (an empty
/// walk is a transiently-unreadable root, not "everything deleted", so it
/// must still run) whose fingerprint matches the last stored one. A
/// foreground pass always returns false.
pub(crate) fn quiet_pass_can_skip(
    quiet: bool,
    files_empty: bool,
    current: (u64, u64),
    stored: Option<(u64, u64)>,
) -> bool {
    quiet && !files_empty && stored == Some(current)
}

/// Stat-only signature (file size, mtime-nanos) for a single watched file —
/// the per-file analogue of `stat_signature_for`. `None` when the file can't
/// be stat'd, so the caller can't prove it's unchanged and revalidates.
pub(crate) fn watched_stat_sig(path: &std::path::Path) -> Option<(u64, u128)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some((meta.len(), mtime))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::vanilla::index_vanilla_dir;
    use super::*;
    use crate::paths::discover_vanilla_dir;
    use cwtools_rules::rules_types::{PathOptions, RuleSet, TypeDefinition};
    use cwtools_string_table::string_table::StringTable;

    #[test]
    fn test_discover_vanilla_dir_unknown_game_is_none() {
        assert!(discover_vanilla_dir("not_a_real_game").is_none());
        assert!(discover_vanilla_dir("").is_none());
    }

    // ── spawn_logging_panics (#155 shared background-task wrapper) ──────────

    #[tokio::test]
    async fn test_spawn_logging_panics_survives_a_panicking_task() {
        // The whole point of the wrapper: a panicking task body must not
        // propagate to (or panic) the caller awaiting it.
        let ok = spawn_logging_panics("test task", async {
            panic!("boom");
        })
        .await;
        assert!(!ok, "a panicking task must report false, not propagate");
    }

    #[tokio::test]
    async fn test_spawn_logging_panics_reports_true_on_success() {
        let ok = spawn_logging_panics("test task", async {}).await;
        assert!(ok, "a task that returns normally must report true");
    }

    // ── watched_batch_slot_is_ours (#155 fix-round-2 MINOR-4 regression) ────

    #[tokio::test]
    async fn test_watched_batch_slot_is_ours_while_handle_unfinished() {
        // Pins the fix: the first attempt at this fix inverted the check, so
        // the normal case (the slot holding this window's own still-running
        // handle, exactly like at the real panic-observation point) read as
        // "not ours" and skipped installing the retry. A live handle in the
        // slot must read as ours.
        let state = DocumentState::new();
        let handle = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });
        *state.watched_debounce.lock() = Some(handle);
        assert!(
            watched_batch_slot_is_ours(&state),
            "a live handle in the slot must read as ours"
        );
    }

    #[tokio::test]
    async fn test_watched_batch_slot_is_not_ours_once_finished() {
        let state = DocumentState::new();
        let handle = tokio::spawn(async {});
        // `JoinHandle::await` would consume it; poll `is_finished()` instead
        // so the (now-finished) handle can still be stored in the slot.
        while !handle.is_finished() {
            tokio::task::yield_now().await;
        }
        *state.watched_debounce.lock() = Some(handle);
        assert!(
            !watched_batch_slot_is_ours(&state),
            "a finished handle must not read as ours"
        );
    }

    #[test]
    fn test_watched_batch_slot_is_not_ours_when_empty() {
        let state = DocumentState::new();
        assert!(
            !watched_batch_slot_is_ours(&state),
            "an empty slot must not read as ours"
        );
    }

    // ── stat_signature_for (quiet-scan loc-rebuild + whole-pass skip) ───────

    #[test]
    fn test_stat_signature_stable_for_unchanged_files() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.yml");
        let b = tmp.path().join("b.yml");
        std::fs::write(&a, "l_english:\n key:0 \"value\"\n").unwrap();
        std::fs::write(&b, "l_english:\n other:0 \"value\"\n").unwrap();

        let sig1 = stat_signature_for(&[a.clone(), b.clone()]);
        // Same files, reversed discovery order — the signature sorts paths
        // first, so order of the input slice must not matter.
        let sig2 = stat_signature_for(&[b, a]);
        assert_eq!(sig1, sig2, "signature must not depend on discovery order");
    }

    #[test]
    fn test_stat_signature_changes_when_a_file_is_touched() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.yml");
        std::fs::write(&a, "l_english:\n key:0 \"value\"\n").unwrap();

        let before = stat_signature_for(std::slice::from_ref(&a));
        // Rewrite with different content (length changes) and bump mtime.
        std::fs::write(&a, "l_english:\n key:0 \"a different, longer value\"\n").unwrap();
        let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        filetime_set(&a, newer);

        let after = stat_signature_for(&[a]);
        assert_ne!(
            before, after,
            "touching a file's size/mtime should change the signature"
        );
    }

    #[test]
    fn test_stat_signature_changes_when_file_set_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.yml");
        std::fs::write(&a, "l_english:\n key:0 \"value\"\n").unwrap();
        let one_file = stat_signature_for(std::slice::from_ref(&a));

        let b = tmp.path().join("b.yml");
        std::fs::write(&b, "l_english:\n other:0 \"value\"\n").unwrap();
        let two_files = stat_signature_for(&[a, b]);

        assert_ne!(
            one_file, two_files,
            "adding a file to the set should change the signature"
        );
    }

    // ── quiet_pass_can_skip (whole-pass short-circuit) ──────────────────────

    #[test]
    fn test_quiet_pass_skips_on_matching_fingerprint() {
        assert!(
            quiet_pass_can_skip(true, false, (7, 1), Some((7, 1))),
            "a quiet pass with an unchanged fingerprint + generation must skip"
        );
    }

    #[test]
    fn test_quiet_pass_runs_when_file_fingerprint_differs() {
        // A changed/added/removed/touched file moves the content fingerprint.
        assert!(
            !quiet_pass_can_skip(true, false, (8, 1), Some((7, 1))),
            "a changed file fingerprint must run the pass"
        );
    }

    #[test]
    fn test_quiet_pass_runs_when_generation_differs() {
        // A rules/config change bumps the generation even if the file set is
        // byte-for-byte identical on disk.
        assert!(
            !quiet_pass_can_skip(true, false, (7, 2), Some((7, 1))),
            "a bumped settings generation must run the pass"
        );
    }

    #[test]
    fn test_quiet_pass_runs_on_first_pass_with_no_stored_fingerprint() {
        assert!(
            !quiet_pass_can_skip(true, false, (7, 1), None),
            "the first pass has nothing to compare against and must run"
        );
    }

    #[test]
    fn test_foreground_pass_never_skips() {
        // Even with a matching fingerprint, a user-invoked (non-quiet) scan runs
        // in full — reindexWorkspace / clearAllCaches / reloadrulesconfig.
        assert!(
            !quiet_pass_can_skip(false, false, (7, 1), Some((7, 1))),
            "a foreground pass must always run"
        );
    }

    #[test]
    fn test_quiet_pass_does_not_skip_empty_walk() {
        // A transiently-unreadable root yields an empty walk; short-circuiting
        // (or recording) a fingerprint for it would suppress the recovery pass.
        assert!(
            !quiet_pass_can_skip(true, true, (7, 1), Some((7, 1))),
            "an empty walk must not short-circuit"
        );
    }

    /// Set a file's mtime forward without depending on filesystem mtime
    /// resolution (some filesystems truncate to 1s), so the "touched" test
    /// above is deterministic. `std::fs::File::set_modified` is stable since
    /// Rust 1.75.
    fn filetime_set(path: &std::path::Path, time: std::time::SystemTime) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(time).unwrap();
    }

    // ── watched_stat_sig (stat-gate for watched CHANGED validation) ────────

    #[test]
    fn test_watched_stat_sig_stable_for_unchanged_file() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("a.txt");
        std::fs::write(&f, "foo = { }\n").unwrap();
        let s1 = watched_stat_sig(&f);
        let s2 = watched_stat_sig(&f);
        assert!(s1.is_some(), "an existing file must have a signature");
        assert_eq!(s1, s2, "unchanged file must produce a stable signature");
    }

    #[test]
    fn test_watched_stat_sig_changes_on_size() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("a.txt");
        std::fs::write(&f, "foo = { }\n").unwrap();
        let before = watched_stat_sig(&f);
        std::fs::write(&f, "foo = { }\nbar = { }\n").unwrap();
        let after = watched_stat_sig(&f);
        assert_ne!(before, after, "a size change must change the signature");
    }

    #[test]
    fn test_watched_stat_sig_changes_on_mtime() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("a.txt");
        std::fs::write(&f, "foo = { }\n").unwrap();
        let before = watched_stat_sig(&f);
        // Same length, bumped mtime — a same-size rewrite (common with
        // formatters / atomic saves) must still invalidate the skip.
        let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        filetime_set(&f, newer);
        let after = watched_stat_sig(&f);
        assert_ne!(before, after, "an mtime bump must change the signature");
    }

    #[test]
    fn test_watched_stat_sig_none_for_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("does_not_exist.txt");
        assert!(
            watched_stat_sig(&f).is_none(),
            "a missing file has no signature, so the caller can't skip it"
        );
    }

    // ── watched batch coalescing (delete + change in one window) ───────────

    #[test]
    fn test_resolve_watched_deletes_excludes_changed_uris() {
        let changes: HashSet<String> = ["a".to_string()].into_iter().collect();
        let deletes: HashSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();
        let out = resolve_watched_deletes(&changes, deletes.into_iter());
        assert_eq!(
            out,
            vec!["b".to_string()],
            "a URI both deleted and changed in one window is a change, not a delete"
        );
    }

    #[test]
    fn test_resolve_watched_deletes_passes_through_pure_deletes() {
        let changes: HashSet<String> = HashSet::new();
        let deletes: HashSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();
        let mut out = resolve_watched_deletes(&changes, deletes.into_iter());
        out.sort();
        assert_eq!(
            out,
            vec!["a".to_string(), "b".to_string()],
            "deletes with no coincident change pass through unchanged"
        );
    }

    #[test]
    fn test_watched_batch_over_cap_counts_deletes_and_changes() {
        // At the cap is not over it (matches the changes-only `> CAP` today).
        assert!(!watched_batch_over_cap(WATCHED_BULK_CAP, 0));
        assert!(watched_batch_over_cap(WATCHED_BULK_CAP, 1));
        // Deletes alone can trip the cap, and so can a delete+change mix that
        // neither side would trip on its own.
        assert!(watched_batch_over_cap(0, WATCHED_BULK_CAP + 1));
        assert!(watched_batch_over_cap(
            WATCHED_BULK_CAP / 2 + 1,
            WATCHED_BULK_CAP / 2 + 1
        ));
        assert!(!watched_batch_over_cap(
            WATCHED_BULK_CAP / 2,
            WATCHED_BULK_CAP / 2
        ));
    }

    // ── ScanGuard (B1 re-entrancy guard) ──────────────────────────────────

    /// A `Backend` over a real (never-initialized) `Client`, so guard tests can
    /// run the notification path — the client suppresses every message before
    /// the handshake, which is exactly what these tests want.
    fn test_backend() -> Backend {
        let state = Arc::new(DocumentState::new());
        let captured = Arc::new(parking_lot::Mutex::new(None));
        let slot = captured.clone();
        let server_state = state.clone();
        let (_service, _socket) = tower_lsp::LspService::new(move |client| {
            *slot.lock() = Some(client.clone());
            Backend {
                client,
                state: server_state.clone(),
            }
        });
        let client = captured.lock().take().unwrap();
        Backend { client, state }
    }

    /// Wait for `flag` to clear, or give up. The cancelled path releases from a
    /// spawned task, so the release is not observable on return from `drop`.
    async fn wait_for_clear(flag: &AtomicBool) -> bool {
        for _ in 0..200 {
            if !flag.load(Ordering::SeqCst) {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        false
    }

    #[tokio::test]
    async fn test_scan_guard_releases_flag_on_finish() {
        let backend = test_backend();
        assert!(
            backend
                .state
                .scan_in_progress
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        );
        let guard = ScanGuard::for_scan(&backend, false);
        assert!(backend.state.scan_in_progress.load(Ordering::SeqCst));
        guard.finish().await;
        assert!(
            !backend.state.scan_in_progress.load(Ordering::SeqCst),
            "finish must release the flag"
        );
    }

    /// #204: a cancelled `workspace/executeCommand` drops the scanning future
    /// without ever reaching `finish`.
    #[tokio::test]
    async fn test_scan_guard_releases_flag_when_dropped_unfinished() {
        let backend = test_backend();
        assert!(
            backend
                .state
                .scan_in_progress
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        );
        drop(ScanGuard::for_scan(&backend, false));
        assert!(
            wait_for_clear(&backend.state.scan_in_progress).await,
            "a dropped guard must release the flag"
        );
    }

    /// A quiet background pass opens no progress, so its guard releases inline
    /// rather than waiting on a task that has nothing to send.
    #[tokio::test]
    async fn test_quiet_scan_guard_releases_flag_inline_on_drop() {
        let backend = test_backend();
        assert!(
            backend
                .state
                .scan_in_progress
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        );
        drop(ScanGuard::for_scan(&backend, true));
        assert!(
            !backend.state.scan_in_progress.load(Ordering::SeqCst),
            "a quiet scan's guard must release the flag without a spawn"
        );
    }

    /// `cacheVanilla` drives the bar without holding the scan flag; its guard
    /// must leave a concurrent scan's flag alone.
    #[tokio::test]
    async fn test_command_guard_leaves_the_scan_flag_alone() {
        let backend = test_backend();
        backend.state.scan_in_progress.store(true, Ordering::SeqCst);
        drop(ScanGuard::for_command(&backend));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            backend.state.scan_in_progress.load(Ordering::SeqCst),
            "a command guard must not release a scan it never took"
        );
    }

    #[test]
    fn test_scan_guard_cas_rejects_second_entrant_while_held() {
        let flag = AtomicBool::new(false);
        assert!(
            flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok(),
            "first scan should win the CAS"
        );
        // A second scan racing in while the first is still running loses the CAS,
        // mirroring how `validate_entire_workspace` bails on a losing entrant.
        assert!(
            flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err(),
            "overlapping scan should lose the CAS while the first is in progress"
        );
    }

    #[tokio::test]
    async fn test_scan_guard_drop_then_reacquire_succeeds() {
        let backend = test_backend();
        assert!(
            backend
                .state
                .scan_in_progress
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        );
        drop(ScanGuard::for_scan(&backend, false));
        assert!(wait_for_clear(&backend.state.scan_in_progress).await);
        // Guard dropped (scan finished, cancelled, or panicked) — a later scan
        // can acquire.
        assert!(
            backend
                .state
                .scan_in_progress
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok(),
            "flag should be free again once the guard is dropped"
        );
    }

    /// Build a minimal `RuleSet` containing one type definition.
    fn ruleset_with_type(name: &str, path: &str, name_field: Option<&str>) -> RuleSet {
        let mut rs = RuleSet::new();
        rs.types.push(TypeDefinition {
            name: name.to_string(),
            name_field: name_field.map(|s| s.to_string()),
            path_options: PathOptions {
                paths: vec![path.to_string()],
                path_strict: false,
                path_file: None,
                path_extension: None,
                paths_lower: Vec::new(),
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
            should_be_referenced: false,
            localisation: Vec::new(),
            graph_related_types: Vec::new(),
            modifiers: Vec::new(),
        });
        rs.reindex();
        rs
    }

    fn vanilla_root() -> std::path::PathBuf {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.keep();
        path.join("vanilla")
    }

    #[test]
    fn test_index_vanilla_dir_collects_instances() {
        let rs = ruleset_with_type("foo", "common/foos", None);

        let root = vanilla_root();
        let foos = root.join("common").join("foos");
        std::fs::create_dir_all(&foos).unwrap();
        std::fs::write(foos.join("a.txt"), "foo_one = { }\nfoo_two = { }\n").unwrap();

        let table = StringTable::new();
        let (per_type, _aux) = index_vanilla_dir(&root, &rs, &table, None, "hoi4");

        let names: Vec<&str> = per_type
            .get("foo")
            .map(|v| v.iter().map(|(_, i)| i.name.as_str()).collect())
            .unwrap_or_default();
        assert!(names.contains(&"foo_one"), "got: {:?}", names);
        assert!(names.contains(&"foo_two"), "got: {:?}", names);
    }

    #[test]
    fn test_index_vanilla_dir_writes_parse_cache() {
        let rs = ruleset_with_type("foo", "common/foos", None);
        let root = vanilla_root();
        let foos = root.join("common").join("foos");
        std::fs::create_dir_all(&foos).unwrap();
        std::fs::write(foos.join("a.txt"), "foo_one = { }\n").unwrap();
        let cache = tempfile::tempdir().unwrap();
        let table = StringTable::new();

        let (first, _) = index_vanilla_dir(&root, &rs, &table, Some(cache.path()), "hoi4");
        assert!(first.get("foo").is_some_and(|entries| !entries.is_empty()));

        let namespace = std::fs::read_dir(cache.path().join("parse-cache"))
            .unwrap()
            .flatten()
            .next()
            .unwrap()
            .path();
        assert!(
            std::fs::read_dir(namespace)
                .unwrap()
                .flatten()
                .any(|entry| entry.path().extension().is_some_and(|ext| ext == "cwb"))
        );

        let (second, _) = index_vanilla_dir(&root, &rs, &table, Some(cache.path()), "hoi4");
        assert!(second.get("foo").is_some_and(|entries| !entries.is_empty()));
    }

    #[test]
    fn test_index_vanilla_dir_uses_name_field() {
        // type[foo] instances are identified by the `name =` leaf, not the node key.
        let rs = ruleset_with_type("foo", "common/foos", Some("name"));

        let root = vanilla_root();
        let foos = root.join("common").join("foos");
        std::fs::create_dir_all(&foos).unwrap();
        std::fs::write(
            foos.join("a.txt"),
            "foo_one = { name = real_name_a }\nfoo_two = { name = real_name_b }\n",
        )
        .unwrap();

        let table = StringTable::new();
        let (per_type, _aux) = index_vanilla_dir(&root, &rs, &table, None, "hoi4");

        let names: Vec<&str> = per_type
            .get("foo")
            .map(|v| v.iter().map(|(_, i)| i.name.as_str()).collect())
            .unwrap_or_default();
        assert!(
            names.contains(&"real_name_a"),
            "name_field instance not extracted: {:?}",
            names
        );
        assert!(
            names.contains(&"real_name_b"),
            "name_field instance not extracted: {:?}",
            names
        );
        assert!(
            !names.contains(&"foo_one"),
            "node key should not be used when name_field is set: {:?}",
            names
        );
    }

    #[test]
    fn test_index_vanilla_dir_no_matching_path_is_empty() {
        let rs = ruleset_with_type("foo", "common/foos", None);

        let root = vanilla_root();
        // No common/foos directory at all.
        std::fs::create_dir_all(root.join("other")).unwrap();

        let table = StringTable::new();
        let (per_type, _aux) = index_vanilla_dir(&root, &rs, &table, None, "hoi4");
        assert!(
            per_type.is_empty(),
            "no matching path should yield an empty index, got: {:?}",
            per_type
        );
    }

    #[test]
    fn test_index_vanilla_dir_skips_unparseable_files() {
        // A malformed file must not abort indexing; valid files in the same dir
        // are still collected.
        let rs = ruleset_with_type("foo", "common/foos", None);

        let root = vanilla_root();
        let foos = root.join("common").join("foos");
        std::fs::create_dir_all(&foos).unwrap();
        std::fs::write(foos.join("good.txt"), "foo_one = { }\n").unwrap();
        // Bare brace with no opening: a parse error.
        std::fs::write(foos.join("bad.txt"), "}\n").unwrap();

        let table = StringTable::new();
        let (per_type, _aux) = index_vanilla_dir(&root, &rs, &table, None, "hoi4");

        let entries = per_type.get("foo").cloned().unwrap_or_default();
        let names: Vec<&str> = entries.iter().map(|(_, i)| i.name.as_str()).collect();
        assert!(
            names.contains(&"foo_one"),
            "valid instance should still be collected despite a bad file: {:?}",
            names
        );
        // Each instance keeps its real source file (goto-into-vanilla).
        assert!(
            entries
                .iter()
                .any(|(uri, _)| uri.replace('\\', "/").ends_with("common/foos/good.txt")),
            "instance should carry its source path, got: {:?}",
            entries.iter().map(|(u, _)| u.as_ref()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_index_vanilla_dir_aux_contains_file_paths() {
        // The vanilla cache aux must record every file that was discovered so
        // the cached index can be validated against the install later.
        let rs = ruleset_with_type("foo", "common/foos", None);

        let root = vanilla_root();
        let foos = root.join("common").join("foos");
        std::fs::create_dir_all(&foos).unwrap();
        std::fs::write(foos.join("a.txt"), "foo_one = { }\n").unwrap();

        let table = StringTable::new();
        let (_per_type, aux) = index_vanilla_dir(&root, &rs, &table, None, "hoi4");
        let logical = aux
            .file_paths
            .iter()
            .map(|p| p.replace('\\', "/"))
            .find(|p| p.ends_with("common/foos/a.txt"));
        assert!(
            logical.is_some(),
            "aux should contain the logical file path, got: {:?}",
            aux.file_paths
        );
    }

    #[tokio::test]
    async fn test_vanilla_merge_populates_the_file_index() {
        // CW113 is gated on a non-empty file index, and the editor's stayed
        // empty: the cache load dropped `file_paths` and nothing walked the
        // workspace, so the check was silently dead in the LSP (#283). Both
        // halves have to be there — vanilla alone would flag every reference to
        // a file the mod ships itself.
        let backend = test_backend();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("mod");
        std::fs::create_dir_all(root.join("gfx")).unwrap();
        std::fs::write(root.join("gfx").join("mod_icon.dds"), b"").unwrap();
        backend.state.config.write().workspace_roots = vec![root];

        backend.stage_vanilla_payload(cwtools_info::vanilla_cache::VanillaCacheData {
            per_type: std::collections::HashMap::new(),
            aux: cwtools_info::vanilla_cache::VanillaCacheAux {
                file_paths: vec!["gfx/vanilla_icon.dds".to_string()],
                ..Default::default()
            },
        });
        backend.merge_pending_vanilla_index();

        let info = backend.state.info_service.read();
        assert!(
            info.type_index.file_index.contains("gfx/vanilla_icon.dds"),
            "cached base-game paths must reach the file index"
        );
        assert!(
            info.type_index.file_index.contains("gfx/mod_icon.dds"),
            "the workspace's own files must reach the file index"
        );
    }

    #[test]
    fn test_index_vanilla_dir_respects_path_strict() {
        // path_strict = yes must only match the exact declared path, not siblings.
        let mut rs = ruleset_with_type("foo", "common/foos", None);
        rs.types[0].path_options.path_strict = true;
        rs.reindex();

        let root = vanilla_root();
        let foos = root.join("common").join("foos");
        let sibling = root.join("common").join("bars");
        std::fs::create_dir_all(&foos).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(foos.join("a.txt"), "foo_one = { }\n").unwrap();
        std::fs::write(sibling.join("b.txt"), "foo_two = { }\n").unwrap();

        let table = StringTable::new();
        let (per_type, _aux) = index_vanilla_dir(&root, &rs, &table, None, "hoi4");

        let names: Vec<&str> = per_type
            .get("foo")
            .map(|v| v.iter().map(|(_, i)| i.name.as_str()).collect())
            .unwrap_or_default();
        assert!(names.contains(&"foo_one"), "got: {:?}", names);
        assert!(
            !names.contains(&"foo_two"),
            "path_strict must not match sibling path, got: {:?}",
            names
        );
    }

    #[test]
    fn test_discover_vanilla_dir_known_game_maps_folder() {
        // discover_vanilla_dir relies on real Steam installs, which won't exist
        // in CI. Verify the mapping indirectly by exercising each known game id
        // and checking that non-existent games return None deterministically.
        for game in ["hoi4", "stellaris", "eu4", "ck3", "vic3"] {
            let _ = discover_vanilla_dir(game);
        }
        assert!(discover_vanilla_dir("nexus_games").is_none());
    }
}
