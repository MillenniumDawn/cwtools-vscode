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

#[derive(Debug, Clone, Default)]
pub(crate) struct ScanSummary {
    pub total_files: usize,
    pub validated_files: usize,
    pub files_with_errors: usize,
    pub total_errors: usize,
    pub total_warnings: usize,
    pub total_infos: usize,
    pub total_hints: usize,
}

const WATCHED_DEBOUNCE_MS: u64 = 500;
const WATCHED_BULK_CAP: usize = 200;

/// (#155): fires at most once per server process, so the e2e suite can
static WATCHED_BATCH_PANIC_ONCE: AtomicBool = AtomicBool::new(true);

pub(crate) fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

type OpenDocSnapshot = (
    String,
    Arc<str>,
    i32,
    Option<Arc<cwtools_parser::ast::ParsedFile>>,
);

struct ScannedFile {
    path: std::path::PathBuf,
    uri: String,
}

/// `workspace/executeCommand` (#204). `tower-lsp` answers `$/cancelRequest` by
pub(crate) struct ScanGuard {
    client: Client,
    state: Arc<DocumentState>,
    owns_scan: bool,
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

    pub(crate) fn for_command(backend: &Backend) -> Self {
        Self {
            client: backend.client.clone(),
            state: backend.state.clone(),
            owns_scan: false,
            quiet: false,
            finished: false,
        }
    }

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

const SCAN_PROGRESS_TOKEN: &str = "cwtools/scan";

impl Backend {
    pub(crate) async fn send_loading_bar(&self, enable: bool, value: &str) {
        self.send_loading_bar_pct(None, enable, value, None).await;
    }

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
        self.emit_loading_bar(
            owner.and_then(CommandProgress::token).cloned(),
            enable,
            value,
            percentage,
            true,
        )
        .await;
    }

    pub(crate) async fn report_loading_bar_pct(
        &self,
        token: Option<&ProgressToken>,
        value: &str,
        percentage: u32,
    ) {
        if !self.state.loading_bar_active.load(Ordering::SeqCst) {
            return;
        }
        self.emit_loading_bar(token.cloned(), true, value, Some(percentage), false)
            .await;
    }

    async fn emit_loading_bar(
        &self,
        command_token: Option<ProgressToken>,
        enable: bool,
        value: &str,
        percentage: Option<u32>,
        open_stream: bool,
    ) {
        let payload = match percentage {
            Some(pct) => {
                serde_json::json!({ "enable": enable, "value": value, "percentage": pct })
            }
            None => serde_json::json!({ "enable": enable, "value": value }),
        };
        self.client.send_notification::<LoadingBar>(payload).await;
        self.send_work_done_progress(command_token, enable, value, percentage, open_stream)
            .await;
    }

    async fn send_work_done_progress(
        &self,
        command_token: Option<ProgressToken>,
        enable: bool,
        value: &str,
        percentage: Option<u32>,
        open_stream: bool,
    ) {
        use tower_lsp::lsp_types::request::WorkDoneProgressCreate;
        if let Some(token) = command_token
            && enable
        {
            self.client
                .send_notification::<tower_lsp::lsp_types::notification::Progress>(ProgressParams {
                    token,
                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::Report(
                        WorkDoneProgressReport {
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
                if !open_stream {
                    return;
                }
                let client = self.client.clone();
                let create_token = token.clone();
                let created = detached_client_request(async move {
                    client
                        .send_request::<WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
                            token: create_token,
                        })
                        .await
                })
                .await;
                if !matches!(created, Some(Ok(()))) {
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
        self.state
            .scan_progress_active
            .store(enable, Ordering::SeqCst);
    }

    async fn send_update_file_list(&self, file_list: Vec<serde_json::Value>) {
        let payload = serde_json::json!({ "fileList": file_list });
        self.client
            .send_notification::<UpdateFileList>(payload)
            .await;
    }

    pub(crate) fn rebuild_modifier_keys(&self) {
        // Lock order: rules -> info_service. One `rules` write guard holds the
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

/// tower-lsp 0.20 keeps a pending slot per server→client request and `expect`s
/// the oneshot receiver to still be alive when the reply lands
/// (`service/client/pending.rs`). Dropping the future that awaits the reply
/// leaves the slot behind, so the client's answer panics the whole process
/// (#675) — and `spawn_debounced_validate` aborts the previous validation on
/// every keystroke. Running the request on its own task moves the receiver out
/// of the abortable future: dropping a `JoinHandle` detaches, it does not
/// cancel, so the reply still finds a live receiver. Awaiting the handle keeps
/// the caller's ordering and return value; `None` means the task panicked.
pub(crate) async fn detached_client_request<T, F>(fut: F) -> Option<T>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    tokio::spawn(fut).await.ok()
}

/// it (#155). `context` names the task in the log line. Returns whether `fut`
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

/// that parallel load can blow through (#198). Unset, which is every real run,
pub(crate) async fn hold_scan_for_tests() {
    hold_for_tests("CWTOOLS_SCAN_HOLD_MS", "CWTOOLS_SCAN_HOLD_FILE").await;
}

/// phase's ticker demonstrably alive — #434, proving a stray sampler tick
pub(crate) async fn hold_parse_for_tests() {
    hold_for_tests("CWTOOLS_PARSE_HOLD_MS", "CWTOOLS_PARSE_HOLD_FILE").await;
}

async fn hold_for_tests(ms_var: &str, file_var: &str) {
    if let Some(ms) = std::env::var(ms_var)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }
    let Ok(gate) = std::env::var(file_var) else {
        return;
    };
    let gate = std::path::PathBuf::from(gate);
    while tokio::fs::try_exists(&gate).await.unwrap_or(false) {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

pub(crate) fn stat_signature_for(files: &[std::path::PathBuf]) -> u64 {
    let mut sorted: Vec<&std::path::Path> = files.iter().map(|p| p.as_path()).collect();
    sorted.sort_unstable();
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

/// the decision is independently unit-testable (#155 fix-round-2): at the
pub(crate) fn watched_batch_slot_is_ours(state: &DocumentState) -> bool {
    state
        .watched_debounce
        .lock()
        .as_ref()
        .is_some_and(|h| !h.is_finished())
}

pub(crate) fn resolve_watched_deletes(
    changes: &HashSet<String>,
    deletes: impl Iterator<Item = String>,
) -> Vec<String> {
    deletes.filter(|uri| !changes.contains(uri)).collect()
}

pub(crate) fn watched_batch_over_cap(changes: usize, deletes: usize) -> bool {
    changes.saturating_add(deletes) > WATCHED_BULK_CAP
}

pub(crate) fn quiet_pass_can_skip(
    quiet: bool,
    files_empty: bool,
    current: (u64, u64),
    stored: Option<(u64, u64)>,
) -> bool {
    quiet && !files_empty && stored == Some(current)
}

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

    #[test]
    fn test_stat_signature_stable_for_unchanged_files() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.yml");
        let b = tmp.path().join("b.yml");
        std::fs::write(&a, "l_english:\n key:0 \"value\"\n").unwrap();
        std::fs::write(&b, "l_english:\n other:0 \"value\"\n").unwrap();

        let sig1 = stat_signature_for(&[a.clone(), b.clone()]);
        let sig2 = stat_signature_for(&[b, a]);
        assert_eq!(sig1, sig2, "signature must not depend on discovery order");
    }

    #[test]
    fn test_stat_signature_changes_when_a_file_is_touched() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.yml");
        std::fs::write(&a, "l_english:\n key:0 \"value\"\n").unwrap();

        let before = stat_signature_for(std::slice::from_ref(&a));
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

    #[test]
    fn test_quiet_pass_skips_on_matching_fingerprint() {
        assert!(
            quiet_pass_can_skip(true, false, (7, 1), Some((7, 1))),
            "a quiet pass with an unchanged fingerprint + generation must skip"
        );
    }

    #[test]
    fn test_quiet_pass_runs_when_file_fingerprint_differs() {
        assert!(
            !quiet_pass_can_skip(true, false, (8, 1), Some((7, 1))),
            "a changed file fingerprint must run the pass"
        );
    }

    #[test]
    fn test_quiet_pass_runs_when_generation_differs() {
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
        assert!(
            !quiet_pass_can_skip(false, false, (7, 1), Some((7, 1))),
            "a foreground pass must always run"
        );
    }

    #[test]
    fn test_quiet_pass_does_not_skip_empty_walk() {
        assert!(
            !quiet_pass_can_skip(true, true, (7, 1), Some((7, 1))),
            "an empty walk must not short-circuit"
        );
    }

    fn filetime_set(path: &std::path::Path, time: std::time::SystemTime) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(time).unwrap();
    }

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
        assert!(!watched_batch_over_cap(WATCHED_BULK_CAP, 0));
        assert!(watched_batch_over_cap(WATCHED_BULK_CAP, 1));
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
        assert!(
            backend
                .state
                .scan_in_progress
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok(),
            "flag should be free again once the guard is dropped"
        );
    }

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
        let rs = ruleset_with_type("foo", "common/foos", None);

        let root = vanilla_root();
        let foos = root.join("common").join("foos");
        std::fs::create_dir_all(&foos).unwrap();
        std::fs::write(foos.join("good.txt"), "foo_one = { }\n").unwrap();
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
        // workspace, so the check was silently dead in the LSP (#283). Both
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
        for game in ["hoi4", "stellaris", "eu4", "ck3", "vic3", "eu5"] {
            let _ = discover_vanilla_dir(game);
        }
        assert!(discover_vanilla_dir("nexus_games").is_none());
    }
}
