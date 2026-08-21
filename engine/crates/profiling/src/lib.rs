//! Shared profiling + logging helpers for the CLI and LSP binaries.
//!
//! Two knobs, both off by default so a normal run stays quiet:
//!
//! - `RUST_LOG`: standard env-filter (e.g. `RUST_LOG=cwtools_validation=debug`).
//! - `CWTOOLS_PROFILE`: turn on the profiling report. Span timings at `info`
//!   plus RSS samples at phase boundaries (see [`log_rss`]).
//!
//! `RUST_LOG`-only output goes to **stderr** so it never corrupts the LSP's
//! stdout JSON-RPC channel. When `CWTOOLS_PROFILE` is on, output is routed to a
//! bounded in-memory buffer instead: the VS Code client doesn't drain the
//! server's stderr, so a stderr write inside a request handler would block once
//! the OS pipe fills. [`export_profiling_log`] hands the buffer to the client.
//! Instrument a hot path with `#[tracing::instrument(skip_all)]` to have it
//! timed; see PROFILING.md.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Mutex, OnceLock};

use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::format::FmtSpan;

/// Cap the profiling buffer so a long session can't grow it without bound.
/// A few MB is plenty for span timings; oldest bytes are dropped when full.
const PROFILE_BUFFER_CAP: usize = 4 * 1024 * 1024;

/// Process-global ring of recent profiling output, filled only when
/// `CWTOOLS_PROFILE` is on. Oldest bytes are evicted past [`PROFILE_BUFFER_CAP`].
fn profile_buffer() -> &'static Mutex<VecDeque<u8>> {
    static BUFFER: OnceLock<Mutex<VecDeque<u8>>> = OnceLock::new();
    BUFFER.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// `MakeWriter` that appends formatted tracing output into [`profile_buffer`],
/// dropping the oldest bytes once the cap is reached.
#[derive(Clone, Copy)]
struct BufferWriter;

impl Write for BufferWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Ok(mut ring) = profile_buffer().lock() {
            ring.extend(buf.iter().copied());
            if ring.len() > PROFILE_BUFFER_CAP {
                let overflow = ring.len() - PROFILE_BUFFER_CAP;
                ring.drain(..overflow);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for BufferWriter {
    type Writer = BufferWriter;

    fn make_writer(&'a self) -> Self::Writer {
        *self
    }
}

/// Snapshot the in-memory profiling buffer as a UTF-8 string. Returns an empty
/// string when profiling is off or nothing has been logged yet. Non-draining:
/// repeated exports during one session each return the full retained window.
pub fn export_profiling_log() -> String {
    let Ok(ring) = profile_buffer().lock() else {
        return String::new();
    };
    String::from_utf8_lossy(&ring.iter().copied().collect::<Vec<u8>>()).into_owned()
}

/// True when `CWTOOLS_PROFILE` is set to a truthy value (`1`, `true`, `yes`, `on`).
pub fn profile_enabled() -> bool {
    matches!(
        std::env::var("CWTOOLS_PROFILE").ok().as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

/// Install the global tracing subscriber when `RUST_LOG` or `CWTOOLS_PROFILE`
/// is set. Idempotent and safe to call from every binary's `main` — a second
/// call (or a competing subscriber) is ignored.
///
/// When `CWTOOLS_PROFILE` is on, output goes to the in-memory buffer (see the
/// module docs); the `RUST_LOG`-only case still writes to stderr.
pub fn init_tracing() {
    let rust_log = std::env::var("RUST_LOG").ok();
    let profile = profile_enabled();
    if rust_log.is_none() && !profile {
        return;
    }

    let filter = match &rust_log {
        Some(_) => tracing_subscriber::EnvFilter::from_default_env(),
        // CWTOOLS_PROFILE on its own implies "show me the timings".
        None => tracing_subscriber::EnvFilter::new("info"),
    };

    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        // Emit span-close events so instrumented hot paths report busy/idle
        // time — that's the profiling signal.
        .with_span_events(FmtSpan::CLOSE);

    // Profiling on: buffer-only, because the LSP client never drains stderr
    // and a blocked stderr write would stall a request handler. Otherwise
    // (RUST_LOG only) keep the plain stderr behavior.
    let _ = if profile {
        builder
            .with_ansi(false)
            .with_writer(BufferWriter)
            .try_init()
    } else {
        builder.with_writer(std::io::stderr).try_init()
    };
}

/// Current resident set size (physical memory) of this process, in bytes.
/// Linux-only (reads `/proc/self/status`); returns `None` elsewhere or on error.
pub fn current_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            // Format: "VmRSS:\t   123456 kB"
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

/// Format a byte count as a human-readable MiB string, e.g. `1462.3 MiB`.
pub fn format_mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

/// Return freed heap back to the OS (glibc `malloc_trim`). After a big transient
/// (the workspace scan parses the whole base game + ~2M loc entries, then drops
/// them) glibc holds the freed pages, so RSS stays at the peak. Calling this once
/// the transients are gone drops RSS toward the real working set. No-op off glibc.
pub fn trim_memory() {
    #[cfg(target_os = "linux")]
    // SAFETY: malloc_trim takes no ownership and is safe to call at any time;
    // it only releases free heap. The return value (1 = memory freed) is ignored.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// When profiling is enabled, log the current RSS at a named phase boundary.
/// Cheap no-op otherwise. Use this around the expensive phases (workspace
/// scan, loc rebuild) to see where memory is spent and whether it is released.
pub fn log_rss(phase: &str) {
    if !profile_enabled() {
        return;
    }
    match current_rss_bytes() {
        Some(bytes) => {
            tracing::info!(target: "cwtools::profile", phase, rss = %format_mib(bytes), "rss sample")
        }
        None => {
            tracing::info!(target: "cwtools::profile", phase, "rss sample (unavailable on this platform)")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_mib_rounds() {
        assert_eq!(format_mib(1024 * 1024), "1.0 MiB");
        assert_eq!(format_mib(1536 * 1024), "1.5 MiB");
    }

    #[test]
    fn buffer_writer_is_bounded_and_readable() {
        // Isolated buffer so the test doesn't fight the process-global one.
        let ring = Mutex::new(VecDeque::<u8>::new());
        let push = |buf: &[u8]| {
            let mut r = ring.lock().unwrap();
            r.extend(buf.iter().copied());
            if r.len() > PROFILE_BUFFER_CAP {
                let overflow = r.len() - PROFILE_BUFFER_CAP;
                r.drain(..overflow);
            }
        };

        // Write more than the cap; growth must stop at the cap and keep the tail.
        let chunk = vec![b'x'; 1024];
        let writes = (PROFILE_BUFFER_CAP / chunk.len()) + 16;
        for _ in 0..writes {
            push(&chunk);
        }
        push(b"TAIL");

        let r = ring.lock().unwrap();
        assert!(r.len() <= PROFILE_BUFFER_CAP);
        let contents: Vec<u8> = r.iter().copied().collect();
        assert!(contents.ends_with(b"TAIL"));
    }

    #[test]
    fn buffer_writer_appends_into_global_buffer() {
        use std::io::Write;
        let mut w = BufferWriter;
        w.write_all(b"profile-line\n").unwrap();
        assert!(export_profiling_log().contains("profile-line"));
    }

    #[test]
    fn rss_is_positive_on_linux() {
        // On Linux this process has a non-zero RSS; elsewhere None is fine.
        if let Some(bytes) = current_rss_bytes() {
            assert!(bytes > 0);
        }
    }
}
