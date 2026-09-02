use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Mutex, OnceLock};

use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::format::FmtSpan;

const PROFILE_BUFFER_CAP: usize = 4 * 1024 * 1024;

fn profile_buffer() -> &'static Mutex<VecDeque<u8>> {
    static BUFFER: OnceLock<Mutex<VecDeque<u8>>> = OnceLock::new();
    BUFFER.get_or_init(|| Mutex::new(VecDeque::new()))
}

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

pub fn export_profiling_log() -> String {
    let Ok(ring) = profile_buffer().lock() else {
        return String::new();
    };
    String::from_utf8_lossy(&ring.iter().copied().collect::<Vec<u8>>()).into_owned()
}

pub fn profile_enabled() -> bool {
    matches!(
        std::env::var("CWTOOLS_PROFILE").ok().as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

pub fn init_tracing() {
    let rust_log = std::env::var("RUST_LOG").ok();
    let profile = profile_enabled();
    if rust_log.is_none() && !profile {
        return;
    }

    let filter = match &rust_log {
        Some(_) => tracing_subscriber::EnvFilter::from_default_env(),
        None => tracing_subscriber::EnvFilter::new("info"),
    };

    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_span_events(FmtSpan::CLOSE);

    let _ = if profile {
        builder
            .with_ansi(false)
            .with_writer(BufferWriter)
            .try_init()
    } else {
        builder.with_writer(std::io::stderr).try_init()
    };
}

pub fn current_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

pub fn format_mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

pub fn trim_memory() {
    #[cfg(target_os = "linux")]
    // SAFETY: malloc_trim takes no ownership and is safe to call at any time
    unsafe {
        libc::malloc_trim(0);
    }
}

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
        let ring = Mutex::new(VecDeque::<u8>::new());
        let push = |buf: &[u8]| {
            let mut r = ring.lock().unwrap();
            r.extend(buf.iter().copied());
            if r.len() > PROFILE_BUFFER_CAP {
                let overflow = r.len() - PROFILE_BUFFER_CAP;
                r.drain(..overflow);
            }
        };

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
        if let Some(bytes) = current_rss_bytes() {
            assert!(bytes > 0);
        }
    }
}
