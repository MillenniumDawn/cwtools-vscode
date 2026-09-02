use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tower_lsp::Client;
use tower_lsp::lsp_types::*;

use crate::{Backend, DocumentState};

const SAMPLE_INTERVAL_MS: u64 = 200;

/// period down makes "the percentage moves inside a phase" — the whole of #221
fn sample_interval() -> std::time::Duration {
    parse_sample_interval(std::env::var("CWTOOLS_SAMPLE_INTERVAL_MS").ok().as_deref())
}

fn parse_sample_interval(raw: Option<&str>) -> std::time::Duration {
    let ms = raw
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .unwrap_or(SAMPLE_INTERVAL_MS);
    std::time::Duration::from_millis(ms)
}

#[cfg(not(test))]
const HEARTBEAT_INTERVAL_SECS: u64 = 30;
#[cfg(test)]
const HEARTBEAT_INTERVAL_SECS: u64 = 1;

#[derive(Clone, Default)]
pub(crate) struct CancelFlag(Option<Arc<AtomicBool>>);

impl CancelFlag {
    pub(crate) fn inert() -> Self {
        Self(None)
    }

    #[cfg(test)]
    pub(crate) fn cancelled_for_tests() -> Self {
        Self(Some(Arc::new(AtomicBool::new(true))))
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.as_ref().is_some_and(|f| f.load(Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanOutcome {
    Ran,
    Busy,
    Cancelled,
}

pub(crate) fn token_key(token: &ProgressToken) -> String {
    match token {
        ProgressToken::Number(n) => format!("n:{n}"),
        ProgressToken::String(s) => format!("s:{s}"),
    }
}

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

pub(crate) fn phase_percentage(phase: Phase, done: usize, total: usize) -> u32 {
    let (start, end) = phase.span();
    if total == 0 {
        return start;
    }
    let ratio = (done.min(total) as f64) / (total as f64);
    start + ((end - start) as f64 * ratio).round() as u32
}

#[derive(Clone)]
struct ProgressSink {
    client: Client,
    token: ProgressToken,
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
    pub(crate) fn inert() -> Self {
        Self { live: None }
    }

    pub(crate) fn tick(&self) {
        if let Some(live) = self.live.as_ref() {
            live.done.fetch_add(1, Ordering::Relaxed);
        }
    }

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
        self.halt();
    }
}

/// command starts is handed the one that started it (#228). Nothing about the
pub(crate) struct CommandProgress {
    state: Arc<DocumentState>,
    sink: Option<ProgressSink>,
    key: Option<String>,
    cancel: CancelFlag,
    ended: bool,
}

impl CommandProgress {
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

    #[cfg(test)]
    pub(crate) fn reports_sent(&self) -> usize {
        self.sink
            .as_ref()
            .map_or(0, |sink| sink.reports.load(Ordering::Relaxed))
    }

    pub(crate) fn cancel_flag(&self) -> CancelFlag {
        self.cancel.clone()
    }

    pub(crate) fn token(&self) -> Option<&ProgressToken> {
        self.sink.as_ref().map(|sink| &sink.token)
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub(crate) async fn report_phase(&self, phase: Phase) {
        if let Some(sink) = self.sink.as_ref() {
            sink.report(phase.label(), Some(phase_percentage(phase, 0, 1)))
                .await;
        }
    }

    pub(crate) async fn finish(mut self, message: Option<String>) {
        self.ended = true;
        self.deregister();
        if let Some(sink) = self.sink.take() {
            sink.send(WorkDoneProgress::End(WorkDoneProgressEnd { message }))
                .await;
        }
    }

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

pub(crate) fn cancel_flag_of(progress: Option<&CommandProgress>) -> CancelFlag {
    progress.map_or_else(CancelFlag::inert, CommandProgress::cancel_flag)
}

impl Backend {
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
            assert_eq!(phase_percentage(phase, 500, 100), end);
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
