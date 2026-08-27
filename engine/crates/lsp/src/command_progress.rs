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

/// The sampler's period, overridable by `CWTOOLS_SAMPLE_INTERVAL_MS`.
///
/// The e2e suite has no other way to see the bar move: at 200ms a fixture small
/// enough to run fast finishes every phase before the first sample, and one big
/// enough to outlast it turns a timing margin into the assertion. Turning the
/// period down makes "the percentage moves inside a phase" — the whole of #221
/// — a deterministic check instead of a race. Zero is ignored rather than
/// panicking `tokio::time::interval`; unset, which is every real run, is 200ms.
fn sample_interval() -> std::time::Duration {
    parse_sample_interval(std::env::var("CWTOOLS_SAMPLE_INTERVAL_MS").ok().as_deref())
}

/// The env parse, split out so the fallbacks are testable without mutating the
/// process environment out from under every other test in the binary.
fn parse_sample_interval(raw: Option<&str>) -> std::time::Duration {
    let ms = raw
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .unwrap_or(SAMPLE_INTERVAL_MS);
    std::time::Duration::from_millis(ms)
}

/// How long a single phase may run before it says so in the output channel,
/// then again every interval after that. Long enough that a healthy scan is
/// silent (every phase of a warm Millennium Dawn scan is under this), short
/// enough that a user watching a frozen bar gets an answer while they watch.
/// 1s in tests so a heartbeat assertion doesn't sit for half a minute.
#[cfg(not(test))]
const HEARTBEAT_INTERVAL_SECS: u64 = 30;
#[cfg(test)]
const HEARTBEAT_INTERVAL_SECS: u64 = 1;

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
    /// `send_notification` no-ops before the handshake, so a unit test can't
    /// observe `$/progress` on the wire; this counts reports instead.
    #[cfg(test)]
    reports: Arc<AtomicUsize>,
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
        #[cfg(test)]
        self.reports.fetch_add(1, Ordering::Relaxed);
        self.send(WorkDoneProgress::Report(WorkDoneProgressReport {
            cancellable: None,
            message: Some(message.to_string()),
            percentage,
        }))
        .await;
    }
}

/// Turns a counter the parallel passes bump into a moving percentage, and says
/// so in the output channel when a phase overruns.
///
/// The rayon sections can't report for themselves: they hold `parking_lot`
/// guards, run under `block_in_place`, and have no async context to send a
/// notification from. Restructuring them into awaitable chunks would either
/// break pass 2's single consistent index snapshot or add a `block_in_place`
/// round trip per chunk, so instead they bump an atomic and this task samples
/// it on a timer.
///
/// Inert (no task spawned, [`PhaseTicker::tick`] a no-op) only for a quiet
/// pass with no command token behind it, which must not reach the user at
/// all. Every visible scan gets one,
/// with or without a client token behind it: the startup scan is exactly the
/// one that used to sit on a phase boundary percentage for its whole longest
/// phase and read as a hang (#221).
pub(crate) struct PhaseTicker {
    live: Option<LivePhase>,
}

struct LivePhase {
    phase: Phase,
    started: std::time::Instant,
    done: Arc<AtomicUsize>,
    sampler: tokio::task::JoinHandle<()>,
}

impl PhaseTicker {
    /// A ticker for work with no progress stream behind it.
    pub(crate) fn inert() -> Self {
        Self { live: None }
    }

    /// Count one item finished. Called from inside the rayon closures, so it
    /// must stay a single relaxed atomic add.
    pub(crate) fn tick(&self) {
        if let Some(live) = self.live.as_ref() {
            live.done.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Stop sampling and describe the phase that just ended, for the output
    /// channel. Not `async`: the sampler owns nothing that needs draining, and
    /// its last report is already on the wire or superseded by the caller's own
    /// end-of-phase report.
    pub(crate) fn stop(mut self) -> Option<String> {
        self.halt()
    }

    fn halt(&mut self) -> Option<String> {
        let live = self.live.take()?;
        live.sampler.abort();
        Some(format!(
            "Scan phase finished: {} ({:.1}s)",
            live.phase.label(),
            live.started.elapsed().as_secs_f64()
        ))
    }
}

impl Drop for PhaseTicker {
    fn drop(&mut self) {
        // A ticker dropped without `stop` (an early return on cancel) must not
        // leave a task reporting progress for a phase that is over.
        self.halt();
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
            #[cfg(test)]
            reports: Arc::new(AtomicUsize::new(0)),
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

    /// How many reports have gone to this command's token.
    #[cfg(test)]
    pub(crate) fn reports_sent(&self) -> usize {
        self.sink
            .as_ref()
            .map_or(0, |sink| sink.reports.load(Ordering::Relaxed))
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

/// Start sampling a phase. The returned ticker is bumped once per item by the
/// worker closures, and its task turns that counter into bar updates plus a
/// heartbeat line whenever the phase runs long.
///
/// `progress` is the command that started the scan, or `None` for the startup
/// and background passes — it decides which `$/progress` stream the updates go
/// to, not whether they happen. A `total` of 0 is a phase with no per-item
/// counter (base-game indexing, the loc rebuild): the bar holds at the boundary
/// value the caller reported and only the heartbeat runs.
///
/// `quiet` decides how a sample is delivered, not whether it is: a quiet pass
/// reports straight to `progress`'s own stream (bypassing the server's
/// `loadingBar`/`$/progress` indicator entirely) when it carries a token, and
/// says nothing otherwise. A non-quiet pass always goes through
/// [`Backend::report_loading_bar_pct`], which drives the server indicator and,
/// when `progress` carries a token, that stream too. The overrun heartbeat is
/// output-channel logging, not the indicator, so it is not gated on `quiet`:
/// any pass that samples also heartbeats.
///
/// [`Backend::report_loading_bar_pct`]: crate::Backend::report_loading_bar_pct
pub(crate) fn start_phase(
    backend: &Backend,
    progress: Option<&CommandProgress>,
    quiet: bool,
    phase: Phase,
    total: usize,
) -> PhaseTicker {
    let done = Arc::new(AtomicUsize::new(0));
    let counter = done.clone();
    let token = progress.and_then(CommandProgress::token).cloned();
    let sink = progress.and_then(|p| p.sink.clone());
    let backend = Backend {
        client: backend.client.clone(),
        state: backend.state.clone(),
    };
    let sampler = tokio::spawn(async move {
        let mut interval = tokio::time::interval(sample_interval());
        // The first tick of a tokio interval fires immediately; the phase has
        // done nothing yet, so let the caller's own boundary report stand and
        // start sampling one interval in.
        interval.tick().await;
        let started = std::time::Instant::now();
        let heartbeat = std::time::Duration::from_secs(HEARTBEAT_INTERVAL_SECS);
        let mut next_heartbeat = heartbeat;
        loop {
            interval.tick().await;
            let seen = counter.load(Ordering::Relaxed);
            if total > 0 {
                let percentage = phase_percentage(phase, seen, total);
                match (quiet, sink.as_ref()) {
                    (true, Some(sink)) => sink.report(phase.label(), Some(percentage)).await,
                    (true, None) => {}
                    (false, _) => {
                        backend
                            .report_loading_bar_pct(token.as_ref(), phase.label(), percentage)
                            .await;
                    }
                }
            }
            let elapsed = started.elapsed();
            if elapsed >= next_heartbeat {
                next_heartbeat += heartbeat;
                backend
                    .client
                    .log_message(
                        MessageType::INFO,
                        heartbeat_message(phase, seen, total, elapsed),
                    )
                    .await;
            }
        }
    });
    PhaseTicker {
        live: Some(LivePhase {
            phase,
            started: std::time::Instant::now(),
            done,
            sampler,
        }),
    }
}

/// The "this is still running" line. A phase with a counter says how far it
/// got, so a stalled one is distinguishable from a slow one at a glance.
fn heartbeat_message(
    phase: Phase,
    seen: usize,
    total: usize,
    elapsed: std::time::Duration,
) -> String {
    let secs = elapsed.as_secs_f64();
    if total > 0 {
        format!(
            "Scan phase still running: {} ({secs:.0}s, {seen}/{total} files)",
            phase.label()
        )
    } else {
        format!("Scan phase still running: {} ({secs:.0}s)", phase.label())
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

    #[test]
    fn test_sample_interval_falls_back_to_the_default() {
        let default = std::time::Duration::from_millis(SAMPLE_INTERVAL_MS);
        assert_eq!(parse_sample_interval(None), default, "unset");
        assert_eq!(parse_sample_interval(Some("")), default, "empty");
        assert_eq!(parse_sample_interval(Some("soon")), default, "unparseable");
        // `tokio::time::interval` panics on a zero period, so a stray `=0` must
        // fall back rather than take the scan down with it.
        assert_eq!(parse_sample_interval(Some("0")), default, "zero");
    }

    #[test]
    fn test_sample_interval_honors_a_positive_override() {
        assert_eq!(
            parse_sample_interval(Some("5")),
            std::time::Duration::from_millis(5)
        );
    }

    #[test]
    fn test_heartbeat_says_how_far_a_counted_phase_got() {
        let msg = heartbeat_message(
            Phase::Validate,
            1200,
            7514,
            std::time::Duration::from_secs(90),
        );
        assert!(msg.contains(Phase::Validate.label()), "{msg}");
        assert!(msg.contains("90s"), "{msg}");
        assert!(
            msg.contains("1200/7514"),
            "a stalled phase must be distinguishable from a slow one: {msg}"
        );
    }

    #[test]
    fn test_heartbeat_omits_the_count_for_an_uncounted_phase() {
        let msg = heartbeat_message(Phase::Vanilla, 0, 0, std::time::Duration::from_secs(30));
        assert!(msg.contains(Phase::Vanilla.label()), "{msg}");
        assert!(!msg.contains('/'), "no counter, no ratio: {msg}");
    }

    #[tokio::test]
    async fn test_inert_ticker_is_a_no_op() {
        let ticker = PhaseTicker::inert();
        ticker.tick();
        assert!(
            ticker.stop().is_none(),
            "a quiet pass's ticker has no phase to report the duration of"
        );
    }
}
