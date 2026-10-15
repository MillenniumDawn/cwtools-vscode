use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::poll::poll_until;
use super::server::TestServer;

/// Where the server under test signals that its scan guard released. Kept out
/// of `workspace`, so the server never writes into the tree its own scans walk;
/// the workspace's unique tempdir name keeps concurrent tests apart.
pub fn scan_release_path(workspace: &Path) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("scan-release");
    std::fs::create_dir_all(&dir).expect("create scan release marker dir");
    dir.join(workspace.file_name().expect("workspace tempdir has a name"))
}

pub fn clear_scan_release(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => panic!("could not clear scan release marker: {error}"),
    }
}

pub fn wait_for_scan_release(path: &Path) {
    assert!(
        poll_until(
            Instant::now() + Duration::from_secs(30),
            Duration::from_millis(5),
            || path.exists().then_some(())
        )
        .is_some(),
        "the active scan did not signal that its guard was released"
    );
    std::fs::remove_file(path).expect("consume scan release marker");
}

/// A `loadingBar` with `enable=true`: a scan has started.
pub fn is_scan_started(frame: &Value) -> bool {
    frame["method"] == "loadingBar" && frame["params"]["enable"] == Value::Bool(true)
}

impl TestServer {
    /// Read frames until the `loadingBar` notification with `enable=false`
    /// arrives, i.e. the workspace scan finished (index_ready is now set).
    ///
    /// A server that died during startup fails here, where it happened, instead
    /// of being handed back as a ready one that the next wait blames (#198).
    pub fn wait_for_scan_done(&mut self) {
        self.read_until("the scan to finish (loadingBar enable=false)", |frame| {
            frame["method"] == "loadingBar" && frame["params"]["enable"] == Value::Bool(false)
        });
    }

    /// Wait for a scan-started signal, `loadingBar` with `enable=true`. `what`
    /// names the scan awaited, so a failure says which one never started.
    ///
    /// `CWTOOLS_SCAN_HOLD_FILE`, which holds while the named path exists, keeps
    /// the scan open *after* this signal fires (see `scan::hold_scan_for_tests`),
    /// so the wait for the started signal is a measure of server
    /// command-processing latency under load, not of the hold. Size `budget` to
    /// that latency (a generous fixed value), not to the hold magnitude: the
    /// hold cannot expire before the signal it follows, so a larger budget never
    /// lets the scan finish first (#212).
    pub fn wait_for_scan_started(&mut self, budget: Duration, what: &str) {
        self.read_until_within(budget, what, is_scan_started);
    }

    /// Send `reindexWorkspace` until it actually starts a scan, returning once
    /// the scan-started signal is in hand. The gate for every test that needs a
    /// scan running, and holding behind its file gate, before it acts.
    ///
    /// Waiting on the bar-on alone is not enough, because the command is allowed
    /// to do nothing: `Backend::validate_entire_workspace` returns `false` and
    /// sends no `loadingBar` at all when it loses the re-entrancy CAS, and the
    /// previous scan's bar-off goes out *before* that guard drops. A
    /// `reindexWorkspace` sent straight after the startup scan's bar-off can land
    /// in exactly that window, skip, and leave the test waiting for a signal that
    /// is never coming. No budget fixes that; the test has to notice the command
    /// answered without scanning and ask again (#198).
    ///
    /// A skipped command answers immediately, whereas a scan that runs sends its
    /// bar-on before the hold and answers only after, so "answered with no bar-on
    /// yet" identifies the skip without depending on the message text. The ids
    /// come from a private range so they cannot collide with a caller's own
    /// requests.
    pub fn reindex_until_scan_starts(&mut self) {
        let scan_release = self.scan_release_path();
        for attempt in 0..20 {
            clear_scan_release(&scan_release);
            let id = 7900 + attempt;
            self.send_request(
                id,
                "workspace/executeCommand",
                json!({ "command": "reindexWorkspace", "arguments": [] }),
            );
            let frame = self.read_until_within(
                Duration::from_secs(30),
                "the reindex scan to start, or the command to answer without starting one",
                |frame| is_scan_started(frame) || frame["id"] == id,
            );
            if is_scan_started(&frame) {
                return;
            }
            // The previous scan sends bar-off before dropping its guard. Retry
            // only after the server signals that the guard itself has released.
            wait_for_scan_release(&scan_release);
        }
        self.fail("reindexWorkspace answered without starting a scan 20 times running");
    }

    /// Wait for the response to request `id`, reporting whether any scan
    /// started before it arrived. Pairs with
    /// [`TestServer::wait_for_scan_started`] for the commands whose contract is
    /// about *which* side of a scan the answer lands on.
    pub fn wait_for_response_watching_scans(
        &mut self,
        id: i64,
        budget: Duration,
        what: &str,
    ) -> (Value, bool) {
        let mut saw_scan = false;
        let response = self.read_until_within(budget, what, |frame| {
            saw_scan |= is_scan_started(frame);
            frame.get("method").is_none() && frame["id"] == id
        });
        (response, saw_scan)
    }

    /// Assert no response to `id` arrives for `quiet`. A server that exited
    /// would satisfy that vacuously, so it fails rather than passes.
    pub fn assert_no_response_within(&mut self, id: i64, quiet: Duration, what: &str) {
        let until = Instant::now() + quiet;
        while let Some(frame) = self.try_read_frame(until.saturating_duration_since(Instant::now()))
        {
            assert!(frame["id"] != id, "{what}: {frame}");
        }
    }

    /// Collect frames until the scan the caller just triggered signals that its
    /// guard released, then until `quiet` passes with nothing new. The release
    /// is the end of the scan, so a silent scan phase cannot cut the drain
    /// short; the quiet window only has to cover frames still in flight on
    /// stdout.
    pub fn drain_through_scan_release(&mut self, quiet: Duration) -> Vec<Value> {
        wait_for_scan_release(&self.scan_release_path());
        self.drain_until_quiet(quiet, Duration::from_secs(8))
    }
}
