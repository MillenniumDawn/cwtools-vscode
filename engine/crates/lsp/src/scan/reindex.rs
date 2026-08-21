use std::sync::atomic::{AtomicBool, Ordering};

use crate::Backend;

use super::{env_flag, spawn_logging_panics};

/// Test-only one-shot panic switch for `CWTOOLS_REINDEX_PANIC_ONCE` (#155):
/// fires at most once per server process so the e2e suite can exercise a
/// background task's panic recovery without leaving the injected panic armed
/// for every later pass.
static REINDEX_PANIC_ONCE: AtomicBool = AtomicBool::new(true);

impl Backend {
    /// Record that the user just interacted with the editor (an edit or a
    /// completion request), resetting the idle clock the background reindex
    /// loop watches.
    pub(crate) fn mark_activity(&self) {
        let now_ms = self.state.start.elapsed().as_millis() as u64;
        self.state.last_activity_ms.store(now_ms, Ordering::Relaxed);
    }

    /// Whether a quiet background pass may run right now: the initial scan
    /// has finished, no scan (foreground or background) is already running,
    /// and the user has been idle for at least `idle_ms`.
    pub(crate) fn should_run_background_pass(&self, idle_ms: u64) -> bool {
        if !self.state.index_ready.load(Ordering::Relaxed) {
            return false;
        }
        if self.state.scan_in_progress.load(Ordering::SeqCst) {
            return false;
        }
        let now_ms = self.state.start.elapsed().as_millis() as u64;
        let last_activity_ms = self.state.last_activity_ms.load(Ordering::Relaxed);
        is_idle(now_ms, last_activity_ms, idle_ms)
    }

    /// The configured background-reindex cadence in seconds. `0` means off.
    /// `CWTOOLS_REINDEX_INTERVAL_SECS` overrides the config value entirely
    /// when set (including to re-enable a disabled config), so tests don't
    /// have to wait out a real 30-minute default.
    fn effective_reindex_interval_secs(&self) -> u64 {
        if let Ok(v) = std::env::var("CWTOOLS_REINDEX_INTERVAL_SECS") {
            return v.parse().unwrap_or(0);
        }
        self.state
            .config
            .read()
            .background_reindex_interval_minutes
            .saturating_mul(60)
    }

    /// How long the user must be idle before a background pass runs, in
    /// milliseconds. The `CWTOOLS_REINDEX_IDLE_SECS` test override wins over
    /// the configured `backgroundReindexIdleSeconds`, which wins over the 15s
    /// default (`Config::new`). Re-read on every not-idle tick, so a live
    /// config change applies without waiting out the old window.
    fn reindex_idle_ms(&self) -> u64 {
        let config_secs = self.state.config.read().background_reindex_idle_seconds;
        let env_val = std::env::var("CWTOOLS_REINDEX_IDLE_SECS").ok();
        resolve_reindex_idle_ms(env_val.as_deref(), config_secs)
    }

    /// Periodic quiet re-scan so a long-running session doesn't accumulate
    /// stale index state (deleted-file entries missed by the watcher, a
    /// settings change that only takes effect on the next scan, …). Spawned
    /// once from `initialized` alongside the startup scan; runs for the life
    /// of the server.
    ///
    /// Each cycle re-reads the effective interval so toggling the setting (or
    /// the env override, in tests) live takes effect without a restart: 0
    /// means disabled, and the loop just polls every 60s waiting for it to
    /// become positive. Once enabled, it sleeps out the interval, then waits
    /// for the user to go idle — re-reading the idle window and the enabled
    /// flag on each tick (bounded to 15s, see `reindex_wait_tick_ms`), so
    /// lowering or disabling either setting is noticed promptly even when
    /// the window is hours — before running a quiet
    /// `validate_entire_workspace`. Never unwraps: a malformed env var
    /// degrades to "disabled"/"default", it doesn't panic the loop. Each
    /// pass runs through `run_reindex_pass`, so a panic inside one pass is
    /// logged and this loop's own task survives to try again next interval
    /// (#155) instead of silently ending periodic reindexing for the rest of
    /// the session.
    pub(crate) async fn background_reindex_loop(&self) {
        loop {
            let interval_secs = self.effective_reindex_interval_secs();
            if interval_secs == 0 {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                continue;
            }
            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;

            loop {
                if self.effective_reindex_interval_secs() == 0 {
                    // Disabled while we were waiting for the interval or for
                    // the user to go idle; the outer loop will pick that up
                    // and fall back to polling.
                    break;
                }
                let idle_ms = self.reindex_idle_ms();
                if self.should_run_background_pass(idle_ms) {
                    self.run_reindex_pass().await;
                    break;
                }
                // Not idle yet — slip forward and check again rather than
                // skipping the whole interval.
                tokio::time::sleep(std::time::Duration::from_millis(reindex_wait_tick_ms(
                    idle_ms,
                )))
                .await;
            }
        }
    }

    /// Run one background reindex pass on its own task via
    /// `spawn_logging_panics`, so a panic inside `validate_entire_workspace`
    /// is logged instead of silently killing `background_reindex_loop`'s task
    /// (#155) — `scan_in_progress` already self-heals through `ScanGuard`
    /// regardless, but nothing previously stopped the panic from also ending
    /// every reindex pass after it.
    async fn run_reindex_pass(&self) {
        let client = self.client.clone();
        let state = self.state.clone();
        spawn_logging_panics("background reindex pass", async move {
            // Test-only panic injection (#155): CWTOOLS_REINDEX_PANIC_ONCE
            // panics the first pass after the server starts, then clears
            // itself, so the e2e suite can exercise the recovery path above.
            if env_flag("CWTOOLS_REINDEX_PANIC_ONCE")
                && REINDEX_PANIC_ONCE.swap(false, Ordering::SeqCst)
            {
                panic!("CWTOOLS_REINDEX_PANIC_ONCE: injected panic for #155 test coverage");
            }
            Backend { client, state }
                .validate_entire_workspace(true)
                .await;
        })
        .await;
    }
}

/// Whether at least `idle_ms` have passed since `last_activity_ms`, both
/// measured in milliseconds on the same monotonic clock (`DocumentState::start`).
/// Saturating so a `last_activity_ms` briefly ahead of `now_ms` (there is no
/// such clock, but defend anyway) reads as "not idle" instead of wrapping.
pub(crate) fn is_idle(now_ms: u64, last_activity_ms: u64, idle_ms: u64) -> bool {
    now_ms.saturating_sub(last_activity_ms) >= idle_ms
}

/// Env > config precedence for the reindex idle window, split out from
/// `Backend::reindex_idle_ms` so it's unit-testable without a `Backend`.
/// A malformed env value degrades to the config value, it doesn't panic.
pub(crate) fn resolve_reindex_idle_secs(env_val: Option<&str>, config_secs: u64) -> u64 {
    env_val
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(config_secs)
}

/// The resolved idle window in milliseconds. Saturating so an absurd
/// configured value (u64::MAX seconds) pins at u64::MAX ms instead of
/// wrapping into a tiny window.
pub(crate) fn resolve_reindex_idle_ms(env_val: Option<&str>, config_secs: u64) -> u64 {
    resolve_reindex_idle_secs(env_val, config_secs).saturating_mul(1000)
}

/// Sleep tick for the not-idle wait in `background_reindex_loop`. Capped at
/// 15s so a lowered or disabled setting is noticed promptly even when the
/// idle window is hours; floored at 50ms so `idle_ms` = 0 (the e2e override)
/// doesn't busy-spin. The idleness comparison still uses the full window.
pub(crate) fn reindex_wait_tick_ms(idle_ms: u64) -> u64 {
    idle_ms.clamp(50, 15_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_idle_below_threshold_is_false() {
        // 4999ms since last activity, 5000ms required — not idle yet.
        assert!(!is_idle(5_000, 1, 5_000));
    }

    #[test]
    fn test_is_idle_at_exact_threshold_is_true() {
        // Exactly `idle_ms` elapsed counts as idle (>=, not >).
        assert!(is_idle(6_000, 1_000, 5_000));
    }

    #[test]
    fn test_is_idle_past_threshold_is_true() {
        assert!(is_idle(10_000, 1_000, 5_000));
    }

    #[test]
    fn test_is_idle_zero_threshold_is_always_true() {
        // idle_ms = 0 means "never wait" — used by the e2e test to trigger
        // the background pass immediately once the interval elapses.
        assert!(is_idle(0, 0, 0));
        assert!(is_idle(12_345, 12_345, 0));
    }

    #[test]
    fn test_is_idle_last_activity_ahead_of_now_is_false() {
        // Should never happen (last_activity_ms is derived from the same
        // monotonic clock as now_ms), but the saturating subtraction must
        // not wrap into "always idle" if it somehow does.
        assert!(!is_idle(100, 200, 1));
        // ...unless idle_ms is 0, where "no wait required" still holds.
        assert!(is_idle(100, 200, 0));
    }

    #[test]
    fn test_reindex_idle_env_wins_over_config() {
        assert_eq!(resolve_reindex_idle_secs(Some("3"), 40), 3);
        // Including re-tightening a config-widened window down to zero.
        assert_eq!(resolve_reindex_idle_secs(Some("0"), 40), 0);
    }

    #[test]
    fn test_reindex_idle_config_wins_over_default() {
        // No env override → the configured value, whatever the built-in
        // default is.
        assert_eq!(resolve_reindex_idle_secs(None, 40), 40);
    }

    #[test]
    fn test_reindex_idle_malformed_env_degrades_to_config() {
        assert_eq!(resolve_reindex_idle_secs(Some("junk"), 40), 40);
        assert_eq!(resolve_reindex_idle_secs(Some(""), 40), 40);
    }

    #[test]
    fn test_reindex_idle_default_is_15_seconds() {
        // An untouched Config carries the documented 15s default.
        assert_eq!(
            crate::state::Config::new().background_reindex_idle_seconds,
            15
        );
    }

    #[test]
    fn test_reindex_idle_ms_saturates_instead_of_wrapping() {
        // A u64::MAX-ish window must pin at u64::MAX ms, not wrap into a
        // near-zero window that would let a background pass fire mid-typing.
        assert_eq!(resolve_reindex_idle_ms(None, u64::MAX), u64::MAX);
        assert_eq!(resolve_reindex_idle_ms(None, u64::MAX / 999), u64::MAX);
        // And the non-saturating path still converts normally.
        assert_eq!(resolve_reindex_idle_ms(None, 15), 15_000);
        assert_eq!(resolve_reindex_idle_ms(Some("3"), 40), 3_000);
    }

    #[test]
    fn test_reindex_wait_tick_is_bounded() {
        // Small windows tick at the window itself...
        assert_eq!(reindex_wait_tick_ms(5_000), 5_000);
        // ...zero (the e2e override) floors at 50ms instead of busy-spinning...
        assert_eq!(reindex_wait_tick_ms(0), 50);
        // ...and huge windows cap at 15s so a live settings change is
        // noticed promptly, not after the old window elapses.
        assert_eq!(reindex_wait_tick_ms(3_600_000), 15_000);
        assert_eq!(reindex_wait_tick_ms(u64::MAX), 15_000);
    }
}
