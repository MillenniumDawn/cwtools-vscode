//! Progress and cancellation for the long `workspace/executeCommand` handlers.
//!
//! The bulk commands (`clearAllCaches`, `reindexWorkspace`, `cacheVanilla`,
//! `reloadrulesconfig`) run for tens of seconds on a large mod. A client that
//! wants a real progress bar and a working Cancel button passes a
//! `workDoneToken` with the request; the server then reports its phases and a
//! percentage against *that* token, so the client's own notification is the
//! one and only indicator for the operation.
//!
//! Cancellation rides `window/workDoneProgress/cancel` — a notification — and
//! deliberately not `$/cancelRequest`. `tower-lsp` answers a request cancel by
//! dropping the handler future, and a dropped future only stops at an `await`;
//! the expensive scan phases are `block_in_place` sections tens of seconds
//! long, so a request cancel cannot interrupt one. A notification is delivered
//! and handled while the scan is still running, which is what lets
//! [`CancelFlag`] be polled per file from inside the rayon passes.
//!
//! `tower-lsp` 0.20 has no `work_done_progress_cancel` on its `LanguageServer`
//! trait (there is a TODO in its `lib.rs` saying as much), so the notification
//! is registered with `LspService::build().custom_method(...)` in `main.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tower_lsp::Client;
use tower_lsp::lsp_types::*;

use crate::{Backend, DocumentState};

/// How often the phase sampler turns its counter into a `$/progress` report.
/// Fast enough that a bar over a 30s scan visibly moves, slow enough that a
/// 12k-file parse doesn't spend its time writing to stdout.
const SAMPLE_INTERVAL_MS: u64 = 200;

/// A shared "the user pressed Cancel" flag, polled by the scan.
///
/// `None` for work with no client token behind it — the startup scan and the
/// periodic background pass, which the user never asked for and cannot cancel.
/// Nothing can set it, so every check is a predictable `false` and the hot
/// loops pay one null check rather than an atomic load.
#[derive(Clone, Default)]
pub(crate) struct CancelFlag(Option<Arc<AtomicBool>>);

impl CancelFlag {
    /// The flag for work nobody can cancel.
    pub(crate) fn inert() -> Self {
        Self(None)
    }

    /// An already-latched flag, for tests that need to drive a cancel path
    /// without standing up a `CommandProgress` and a client token.
    #[cfg(test)]
    pub(crate) fn cancelled_for_tests() -> Self {
        Self(Some(Arc::new(AtomicBool::new(true))))
    }

    /// `Relaxed` throughout: this is a one-way false→true latch used to stop
    /// doing more work, never to publish data. The worst a stale read costs is
    /// one more file parsed before the next check sees it.
    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.as_ref().is_some_and(|f| f.load(Ordering::Relaxed))
    }
}

/// What a scan attempt actually did, for callers that report back to the user.
///
/// `Busy` is the CAS loser (another scan holds the workspace) and is retried by
/// several callers; `Cancelled` must never be retried — the user asked for it
/// to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanOutcome {
    Ran,
    Busy,
    Cancelled,
}

/// Canonical map key for a `workDoneToken`.
///
/// The protocol allows a number or a string and a client may legitimately use
/// both, so the discriminant is part of the key: token `1` and token `"1"` are
/// different tokens and must not collide.
pub(crate) fn token_key(token: &ProgressToken) -> String {
    match token {
        ProgressToken::Number(n) => format!("n:{n}"),
        ProgressToken::String(s) => format!("s:{s}"),
    }
}

/// The scan's phases and the slice of the overall percentage each one owns.
///
/// Hand-weighted from a cold MD scan (~7.4k workspace files): parsing and
/// validating dominate, the loc rebuild is the biggest single transient, and
/// discovery is noise. They only have to be monotonic and roughly
/// proportional — a bar that jumps from 40% to 70% reads as broken even when
/// the phase boundary is honest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Discover,
    Parse,
    Vanilla,
    Localisation,
    Validate,
    Publish,
}

impl Phase {
    /// `(start, end)` of this phase's span of the 0-100 bar.
    fn span(self) -> (u32, u32) {
        match self {
            Phase::Discover => (0, 3),
            Phase::Parse => (3, 40),
            Phase::Vanilla => (40, 55),
            Phase::Localisation => (55, 70),
            Phase::Validate => (70, 92),
            Phase::Publish => (92, 100),
        }
    }

    /// The user-facing phase text. Also the `loadingBar` status-bar string, so
    /// the two channels can't drift.
    pub(crate) fn label(self) -> &'static str {
        cwtools_i18n::t(match self {
            Phase::Discover => cwtools_i18n::Key::ProgressDiscover,
            Phase::Parse => cwtools_i18n::Key::ProgressParse,
            Phase::Vanilla => cwtools_i18n::Key::ProgressVanilla,
            Phase::Localisation => cwtools_i18n::Key::ProgressLocalisation,
            Phase::Validate => cwtools_i18n::Key::ProgressValidate,
            Phase::Publish => cwtools_i18n::Key::ProgressPublish,
        })
    }
}

/// Overall bar position for `done` of `total` items within `phase`.
///
/// A `total` of 0 sits at the phase start rather than dividing by zero, and
/// `done` past `total` (a miscount) clamps to the phase end instead of
/// overrunning into the next phase's span.
pub(crate) fn phase_percentage(phase: Phase, done: usize, total: usize) -> u32 {
    let (start, end) = phase.span();
    if total == 0 {
        return start;
    }
    let ratio = (done.min(total) as f64) / (total as f64);
    start + ((end - start) as f64 * ratio).round() as u32
}

/// Everything needed to emit `$/progress` from a detached task: the client, the
/// token to report against, and the phase weighting.
#[derive(Clone)]
struct ProgressSink {
    client: Client,
    token: ProgressToken,
}

impl ProgressSink {
    async fn send(&self, value: WorkDoneProgress) {
        self.client
            .send_notification::<tower_lsp::lsp_types::notification::Progress>(ProgressParams {
                token: self.token.clone(),
                value: ProgressParamsValue::WorkDone(value),
            })
            .await;
    }

    /// `cancellable` is left unset: the protocol says a report without it keeps
    /// whatever `begin` established, so the one place that decides stays the
    /// one place that says it.
    async fn report(&self, message: &str, percentage: Option<u32>) {
        self.send(WorkDoneProgress::Report(WorkDoneProgressReport {
            cancellable: None,
            message: Some(message.to_string()),
            percentage,
        }))
        .await;
    }
}

/// Turns a counter the parallel passes bump into a moving percentage.
///
/// The rayon sections can't report for themselves: they hold `parking_lot`
/// guards, run under `block_in_place`, and have no async context to send a
/// notification from. Restructuring them into awaitable chunks would either
/// break pass 2's single consistent index snapshot or add a `block_in_place`
/// round trip per chunk, so instead they bump an atomic and this task samples
/// it on a timer.
///
/// Inert (no task spawned, [`PhaseTicker::tick`] a no-op) when the command
/// carried no token — the background pass must not pay for progress nobody
/// asked for.
pub(crate) struct PhaseTicker {
    done: Option<Arc<AtomicUsize>>,
    sampler: Option<tokio::task::JoinHandle<()>>,
}

impl PhaseTicker {
    /// A ticker for work with no progress stream behind it.
    pub(crate) fn inert() -> Self {
        Self {
            done: None,
            sampler: None,
        }
    }

    /// Count one item finished. Called from inside the rayon closures, so it
    /// must stay a single relaxed atomic add.
    pub(crate) fn tick(&self) {
        if let Some(done) = self.done.as_ref() {
            done.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Stop sampling. Not `async`: the sampler owns nothing that needs
    /// draining, and its last report is already on the wire or superseded by
    /// the caller's own end-of-phase report.
    pub(crate) fn stop(mut self) {
        if let Some(handle) = self.sampler.take() {
            handle.abort();
        }
    }
}

impl Drop for PhaseTicker {
    fn drop(&mut self) {
        // A ticker dropped without `stop` (an early return on cancel) must not
        // leave a task reporting progress for a phase that is over.
        if let Some(handle) = self.sampler.take() {
            handle.abort();
        }
    }
}

/// Owns one command's `$/progress` stream for the life of the command.
///
/// Created per `workspace/executeCommand`. With no `workDoneToken` on the
/// request every method is a no-op and the server falls back to its own
/// `loadingBar` + `cwtools/scan` reporting, so an older client (or a plain
/// `:LspExecuteCommand` from an editor that doesn't pass tokens) behaves
/// exactly as it did before.
///
/// Two commands in flight at once hold one of these each, and the work a
/// command starts is handed the one that started it (#228). Nothing about the
/// stream is global: a second command beginning cannot divert the first's
/// phases onto its own token, and a second command ending cannot leave the
/// first reporting to the server's stream.
pub(crate) struct CommandProgress {
    state: Arc<DocumentState>,
    /// `None` when the request carried no token — every method is then a no-op
    /// and the server falls back to its own indicator. The sink carries its own
    /// `Client` clone so the sampler task can outlive a borrow of the backend.
    sink: Option<ProgressSink>,
    key: Option<String>,
    cancel: CancelFlag,
    ended: bool,
}

impl CommandProgress {
    /// Register the token and send `begin`.
    ///
    /// No `window/workDoneProgress/create` round trip: that request exists so a
    /// *server*-initiated token can be registered with the client, and this
    /// token came from the client in the first place.
    /// `cancellable` is what the bar advertises, and must reflect whether this
    /// command actually polls its flag. A command whose body never checks —
    /// `genlocall`, `fixAllWorkspace`, the base-game index — passes `false`
    /// rather than showing a Cancel button that does nothing.
    pub(crate) async fn begin(
        backend: &Backend,
        token: Option<ProgressToken>,
        title: &str,
        cancellable: bool,
    ) -> Self {
        let Some(token) = token else {
            return Self {
                state: backend.state.clone(),
                sink: None,
                key: None,
                cancel: CancelFlag::inert(),
                ended: false,
            };
        };
        let key = token_key(&token);
        let flag = Arc::new(AtomicBool::new(false));
        backend
            .state
            .command_cancels
            .lock()
            .insert(key.clone(), flag.clone());
        let sink = ProgressSink {
            client: backend.client.clone(),
            token,
        };
        sink.send(WorkDoneProgress::Begin(WorkDoneProgressBegin {
            title: title.to_string(),
            cancellable: Some(cancellable),
            message: None,
            percentage: Some(0),
        }))
        .await;
        Self {
            state: backend.state.clone(),
            sink: Some(sink),
            key: Some(key),
            cancel: CancelFlag(Some(flag)),
            ended: false,
        }
    }

    /// `ended` so Drop does not send a progress end.
    #[cfg(test)]
    pub(crate) fn for_tests(state: Arc<DocumentState>, flag: Arc<AtomicBool>) -> Self {
        Self {
            state,
            sink: None,
            key: None,
            cancel: CancelFlag(Some(flag)),
            ended: true,
        }
    }

    /// The flag to hand to the scan.
    pub(crate) fn cancel_flag(&self) -> CancelFlag {
        self.cancel.clone()
    }

    /// The token this command's phase reports go to, or `None` when the request
    /// carried none. The scan reports its boundaries against the token of the
    /// command that started it, so two commands running at once keep their
    /// streams apart.
    pub(crate) fn token(&self) -> Option<&ProgressToken> {
        self.sink.as_ref().map(|sink| &sink.token)
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Report a phase with no item count behind it — the boundary markers.
    pub(crate) async fn report_phase(&self, phase: Phase) {
        if let Some(sink) = self.sink.as_ref() {
            sink.report(phase.label(), Some(phase_percentage(phase, 0, 1)))
                .await;
        }
    }

    /// Start sampling a counter for a parallel phase. The returned ticker is
    /// bumped once per item by the worker closures.
    pub(crate) fn start_phase(&self, phase: Phase, total: usize) -> PhaseTicker {
        let Some(sink) = self.sink.clone() else {
            return PhaseTicker {
                done: None,
                sampler: None,
            };
        };
        let done = Arc::new(AtomicUsize::new(0));
        let counter = done.clone();
        let sampler = tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_millis(SAMPLE_INTERVAL_MS));
            // The first tick of a tokio interval fires immediately; the phase
            // has done nothing yet, so let the caller's own boundary report
            // stand and start sampling one interval in.
            interval.tick().await;
            loop {
                interval.tick().await;
                let seen = counter.load(Ordering::Relaxed);
                sink.report(phase.label(), Some(phase_percentage(phase, seen, total)))
                    .await;
            }
        });
        PhaseTicker {
            done: Some(done),
            sampler: Some(sampler),
        }
    }

    /// Close the stream and deregister the token.
    pub(crate) async fn finish(mut self, message: Option<String>) {
        self.ended = true;
        self.deregister();
        if let Some(sink) = self.sink.take() {
            sink.send(WorkDoneProgress::End(WorkDoneProgressEnd { message }))
                .await;
        }
    }

    /// Drop the token from the cancel registry.
    fn deregister(&self) {
        if let Some(key) = self.key.as_ref() {
            self.state.command_cancels.lock().remove(key);
        }
    }
}

impl Drop for CommandProgress {
    fn drop(&mut self) {
        if self.ended {
            return;
        }
        // The handler future was dropped — a `$/cancelRequest`, or a panic
        // unwinding out of the command. The client is still showing a progress
        // notification it will never get an `end` for, so send one. `Drop`
        // can't await, so it goes out on its own task; a runtime already torn
        // down leaves nothing to send it on and nothing to leak either.
        self.deregister();
        let Some(sink) = self.sink.take() else { return };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                sink.send(WorkDoneProgress::End(WorkDoneProgressEnd {
                    message: Some(
                        cwtools_i18n::t(cwtools_i18n::Key::ProgressCancelled).to_string(),
                    ),
                }))
                .await;
            });
        }
    }
}

/// [`CommandProgress::start_phase`] for a scan that may have no command behind
/// it, so the scan body doesn't branch on `Option` at every phase.
pub(crate) fn start_phase(
    progress: Option<&CommandProgress>,
    phase: Phase,
    total: usize,
) -> PhaseTicker {
    match progress {
        Some(progress) => progress.start_phase(phase, total),
        None => PhaseTicker::inert(),
    }
}

/// The cancel flag behind an optional command, inert when there is none.
pub(crate) fn cancel_flag_of(progress: Option<&CommandProgress>) -> CancelFlag {
    progress.map_or_else(CancelFlag::inert, CommandProgress::cancel_flag)
}

impl Backend {
    /// `window/workDoneProgress/cancel` (C→S). Latches the cancel flag for the
    /// in-flight command that owns `params.token`.
    ///
    /// Unknown tokens are ignored, not an error: a cancel that races the
    /// command's own completion is normal, and the protocol has no failure
    /// response for a notification anyway.
    pub(crate) async fn on_work_done_progress_cancel(&self, params: WorkDoneProgressCancelParams) {
        let key = token_key(&params.token);
        let flag = self.state.command_cancels.lock().get(&key).cloned();
        match flag {
            Some(flag) => {
                flag.store(true, Ordering::Relaxed);
                tracing::info!(token = %key, "command cancelled by client");
            }
            None => {
                tracing::debug!(token = %key, "cancel for an unknown or finished progress token");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_key_separates_number_and_string_tokens() {
        assert_ne!(
            token_key(&ProgressToken::Number(1)),
            token_key(&ProgressToken::String("1".to_string())),
            "a numeric token and the same digits as a string are different tokens"
        );
    }

    #[test]
    fn test_inert_flag_is_never_cancelled() {
        assert!(!CancelFlag::inert().is_cancelled());
        assert!(!CancelFlag::default().is_cancelled());
    }

    #[test]
    fn test_cancel_flag_latches() {
        let inner = Arc::new(AtomicBool::new(false));
        let flag = CancelFlag(Some(inner.clone()));
        assert!(!flag.is_cancelled());
        inner.store(true, Ordering::Relaxed);
        assert!(flag.is_cancelled(), "a clone observes the latch");
    }

    #[test]
    fn test_phase_percentage_stays_inside_its_span() {
        for phase in [
            Phase::Discover,
            Phase::Parse,
            Phase::Vanilla,
            Phase::Localisation,
            Phase::Validate,
            Phase::Publish,
        ] {
            let (start, end) = phase.span();
            assert_eq!(phase_percentage(phase, 0, 100), start);
            assert_eq!(phase_percentage(phase, 100, 100), end);
            // A miscount past the total clamps rather than bleeding into the
            // next phase's span.
            assert_eq!(phase_percentage(phase, 500, 100), end);
            // No division by zero for a phase with nothing in it.
            assert_eq!(phase_percentage(phase, 0, 0), start);
        }
    }

    #[test]
    fn test_phase_spans_are_contiguous_and_cover_the_bar() {
        let phases = [
            Phase::Discover,
            Phase::Parse,
            Phase::Vanilla,
            Phase::Localisation,
            Phase::Validate,
            Phase::Publish,
        ];
        assert_eq!(phases[0].span().0, 0, "the bar starts at 0");
        assert_eq!(phases[phases.len() - 1].span().1, 100, "and ends at 100");
        for pair in phases.windows(2) {
            assert_eq!(
                pair[0].span().1,
                pair[1].span().0,
                "phases must not leave a gap or overlap: {:?} -> {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn test_phase_percentage_is_monotonic_within_a_phase() {
        let mut last = 0;
        for done in 0..=1000 {
            let pct = phase_percentage(Phase::Parse, done, 1000);
            assert!(pct >= last, "percentage went backwards at {done}");
            last = pct;
        }
    }

    #[tokio::test]
    async fn test_inert_ticker_is_a_no_op() {
        let ticker = PhaseTicker {
            done: None,
            sampler: None,
        };
        ticker.tick();
        ticker.stop();
    }
}
