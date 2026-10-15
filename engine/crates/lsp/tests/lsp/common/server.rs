use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use cwtools_test_support::scratch_home_env;
use serde_json::{Map, Value, json};
use tempfile::TempDir;

use super::scan::scan_release_path;
use super::wire::{
    jsonrpc_notification, jsonrpc_request, path_uri, read_frame_from, shutdown_request,
    write_frame_to,
};

/// How long one read waits for the frame it needs before the test fails. A
/// scan on a loaded runner takes seconds; a hung server never answers.
const READ_DEADLINE: Duration = Duration::from_secs(60);

/// How long a graceful stop waits for the server to exit before killing it.
const STOP_BOUND: Duration = Duration::from_secs(10);

const SHUTDOWN_ID: i64 = 2_147_483_647;
const SEEN_FRAMES: usize = 12;
const STDERR_TAIL_BYTES: usize = 16 * 1024;

/// A `cwtools-server` child driven over stdio. It owns the workspace, rules and
/// base-game directories, keeps the server's stderr, bounds every read, and is
/// killed on drop, so a hung or crashed server fails one test with the server's
/// own output in the message instead of hanging the binary.
pub struct TestServer {
    child: Child,
    stdin: Option<ChildStdin>,
    frames: Receiver<Result<Value, String>>,
    stderr: Arc<StderrLog>,
    workspace: TempDir,
    rules: Option<TempDir>,
    vanilla: Option<TempDir>,
    init_params: Value,
    init: Value,
    deadline: Duration,
    next_id: i64,
    seen: VecDeque<String>,
    ended: Option<String>,
    stopped: bool,
    /// The server's stderr is already in the failure message.
    reported: bool,
}

#[derive(Default)]
struct StderrLog {
    text: Mutex<Vec<u8>>,
    done: AtomicBool,
}

enum Wait {
    Timeout,
    Closed(String),
}

/// What [`TestServer::shutdown`] observed.
pub struct Shutdown {
    /// The server answered `shutdown` with a success reply and a null result.
    pub responded: bool,
    /// The child's own exit status, `None` when it had to be killed.
    pub status: Option<ExitStatus>,
}

impl TestServer {
    pub fn builder() -> ServerBuilder {
        ServerBuilder::default()
    }

    pub fn workspace(&self) -> &Path {
        self.workspace.path()
    }

    /// The directory named by the `rulesCache` init option.
    pub fn rules_dir(&self) -> &Path {
        self.rules
            .as_ref()
            .expect("internal error: this server was built without rules")
            .path()
    }

    /// The directory named by the `vanilla` init option.
    pub fn vanilla_dir(&self) -> &Path {
        self.vanilla
            .as_ref()
            .expect("internal error: this server was built without a vanilla root")
            .path()
    }

    /// Where the server signals that a scan guard released, when the builder
    /// asked for [`ServerBuilder::scan_release_marker`].
    pub fn scan_release_path(&self) -> PathBuf {
        scan_release_path(self.workspace())
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.workspace().join(rel)
    }

    pub fn uri(&self, rel: &str) -> String {
        path_uri(self.path(rel))
    }

    /// Write `content` under the workspace, creating parent directories.
    pub fn write_file(&self, rel: &str, content: impl AsRef<[u8]>) -> PathBuf {
        let path = self.path(rel);
        write_creating_dirs(&path, content.as_ref());
        path
    }

    /// The full response to the `initialize` request.
    pub fn init_response(&self) -> &Value {
        &self.init
    }

    /// The server's stderr so far.
    pub fn stderr_text(&self) -> String {
        let text = self
            .stderr
            .text
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&text).into_owned()
    }

    /// Send `initialize` built from the builder and wait for its response.
    pub fn initialize(&mut self) -> &Value {
        let params = self.init_params.clone();
        self.initialize_with(params)
    }

    /// Send `initialize` built from the builder without reading anything back,
    /// for a test that watches what the server logs while it handles it.
    pub fn send_initialize(&mut self) {
        let params = self.init_params.clone();
        self.send_request(1, "initialize", params);
    }

    /// [`TestServer::initialize`] with the members of the object `extra` set
    /// over the `initialize` params the builder made.
    pub fn initialize_merging(&mut self, extra: Value) -> &Value {
        let mut params = self.init_params.clone();
        let Value::Object(extra) = extra else {
            panic!("internal error: initialize_merging takes a JSON object, got {extra}");
        };
        for (key, value) in extra {
            params[key] = value;
        }
        self.initialize_with(params)
    }

    pub fn initialize_with(&mut self, params: Value) -> &Value {
        self.send_request(1, "initialize", params);
        let response = self.read_response_for_id(1);
        if let Some(error) = response.get("error") {
            self.fail(&format!("initialize failed: {error}"));
        }
        self.init = response;
        &self.init
    }

    pub fn initialized(&mut self) {
        self.notify("initialized", json!({}));
    }

    pub fn send(&mut self, body: &str) {
        let written = match self.stdin.as_ref() {
            Some(mut stdin) => write_frame_to(&mut stdin, body),
            None => self.fail("cannot send: stdin is already closed"),
        };
        if let Err(error) = written {
            self.fail(&format!("cannot send to the server: {error}"));
        }
    }

    /// Write bytes to stdin as they are, with no framing.
    pub fn send_raw(&mut self, bytes: &[u8]) {
        let written = match self.stdin.as_ref() {
            Some(mut stdin) => stdin.write_all(bytes).and_then(|()| stdin.flush()),
            None => self.fail("cannot send: stdin is already closed"),
        };
        if let Err(error) = written {
            self.fail(&format!("cannot send to the server: {error}"));
        }
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(&jsonrpc_notification(method, params));
    }

    /// Send a request without waiting for the reply.
    pub fn send_request(&mut self, id: i64, method: &str, params: Value) {
        self.send(&jsonrpc_request(id, method, params));
    }

    /// Send a request and return its whole response, `error` included.
    pub fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send_request(id, method, params);
        self.read_response_for_id(id)
    }

    /// [`TestServer::call`] under an id the test chooses, for the tests that
    /// assert the id they sent comes back.
    pub fn call_with_id(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send_request(id, method, params);
        self.read_response_for_id(id)
    }

    /// Send a request and return its `result`. An `error` response fails the
    /// test, so an assertion about what is absent cannot pass on an error.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let response = self.call(method, params);
        self.result_of(method, response)
    }

    /// The `result` of a response, failing the test when it is an `error`.
    pub fn result_of(&mut self, what: &str, response: Value) -> Value {
        if let Some(error) = response.get("error") {
            self.fail(&format!("{what} failed: {error}"));
        }
        response.get("result").cloned().unwrap_or(Value::Null)
    }

    /// Send a `textDocument/didOpen` for `rel` with `text`.
    pub fn open(&mut self, rel: &str, text: &str) {
        let uri = self.uri(rel);
        self.open_uri(&uri, text);
    }

    pub fn open_uri(&mut self, uri: &str, text: &str) {
        self.open_uri_as(uri, "hoi4", text);
    }

    /// [`TestServer::open`] under another `languageId`.
    pub fn open_as(&mut self, rel: &str, language_id: &str, text: &str) {
        let uri = self.uri(rel);
        self.open_uri_as(&uri, language_id, text);
    }

    pub fn open_uri_as(&mut self, uri: &str, language_id: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": text,
                }
            }),
        );
    }

    /// Send a full-sync `textDocument/didChange` that replaces the text of `rel`.
    pub fn change(&mut self, rel: &str, version: i32, text: &str) {
        let uri = self.uri(rel);
        self.change_uri(&uri, version, text);
    }

    pub fn change_uri(&mut self, uri: &str, version: i32, text: &str) {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }],
            }),
        );
    }

    pub fn save_uri(&mut self, uri: &str) {
        self.notify(
            "textDocument/didSave",
            json!({ "textDocument": { "uri": uri } }),
        );
    }

    pub fn close(&mut self, rel: &str) {
        let uri = self.uri(rel);
        self.close_uri(&uri);
    }

    pub fn close_uri(&mut self, uri: &str) {
        self.notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri } }),
        );
    }

    /// Open `rel` and wait for its first diagnostics. A did_open publishes only
    /// after the file's index write lands, so this is the signal that its
    /// exports (value_set members, enum values, type instances) are queryable.
    pub fn open_and_wait(&mut self, rel: &str, text: &str) {
        self.open(rel, text);
        self.wait_for_diagnostics(rel);
    }

    /// The next frame, whatever it is.
    pub fn read_frame(&mut self) -> Value {
        let until = self.horizon();
        self.expect_frame(until, "the next frame")
    }

    /// The next frame if one arrives within `within`. `None` means the server
    /// stayed quiet, which is a legitimate outcome only for a test that asserts
    /// silence; a server that exited is reported as a failure.
    pub fn try_read_frame(&mut self, within: Duration) -> Option<Value> {
        match self.next_frame(Instant::now() + within) {
            Ok(frame) => Some(frame),
            Err(Wait::Timeout) => None,
            Err(Wait::Closed(_)) if self.stopped => None,
            Err(Wait::Closed(reason)) => {
                self.fail(&format!("the server closed its stdout: {reason}"))
            }
        }
    }

    /// Read frames, dropping the ones `accept` rejects, until one is accepted.
    pub fn read_until(&mut self, what: &str, accept: impl FnMut(&Value) -> bool) -> Value {
        self.read_until_within(self.deadline, what, accept)
    }

    /// [`TestServer::read_until`] with its own wait instead of the server's.
    pub fn read_until_within(
        &mut self,
        budget: Duration,
        what: &str,
        mut accept: impl FnMut(&Value) -> bool,
    ) -> Value {
        let until = Instant::now() + budget;
        loop {
            let frame = self.expect_frame(until, what);
            if accept(&frame) {
                return frame;
            }
        }
    }

    /// The response to request `id`, skipping every other frame.
    pub fn read_response_for_id(&mut self, id: i64) -> Value {
        self.read_until(&format!("the response to request {id}"), |frame| {
            is_response(frame) && frame["id"] == id
        })
    }

    /// Drain frames until the `publishDiagnostics` for the URI ending in
    /// `rel_path`, and return it. The server canonicalizes the URI, so the
    /// match is by path suffix.
    pub fn wait_for_diagnostics(&mut self, rel_path: &str) -> Value {
        self.read_until(&format!("publishDiagnostics for {rel_path}"), |frame| {
            frame["method"] == "textDocument/publishDiagnostics"
                && frame["params"]["uri"]
                    .as_str()
                    .is_some_and(|uri| uri.ends_with(rel_path))
        })
    }

    /// Read the server's next request to the client and answer it with a null
    /// result.
    pub fn answer_server_request(&mut self) -> Value {
        let request = self.read_until("a request from the server", |frame| {
            frame.get("method").is_some() && frame.get("id").is_some()
        });
        self.reply(&request);
        request
    }

    /// Read until the response to request `id`, answering every
    /// `workspace/applyEdit` request on the way as applied. Returns the response
    /// and the edit of the last `applyEdit` seen, if any, so the caller can
    /// assert on its contents. `fixAllWorkspace` and `formatWorkspace` are the
    /// server's requests to the client, so the harness acts as an LSP client.
    pub fn read_response_answering_apply_edit(&mut self, id: i64) -> (Value, Option<Value>) {
        let until = self.horizon();
        let what = format!("the response to request {id}");
        let mut edit = None;
        loop {
            let frame = self.expect_frame(until, &what);
            if frame["method"] == "workspace/applyEdit" {
                edit = Some(frame["params"]["edit"].clone());
                let reply =
                    json!({ "jsonrpc": "2.0", "id": frame["id"], "result": { "applied": true } });
                self.send(&reply.to_string());
            } else if is_response(&frame) && frame["id"] == id {
                return (frame, edit);
            }
        }
    }

    /// Answer `request`, a request from the server, with a null result.
    pub fn reply(&mut self, request: &Value) {
        assert!(request.get("method").is_some(), "not a request: {request}");
        assert!(request.get("id").is_some(), "request has no id: {request}");
        let body = json!({ "jsonrpc": "2.0", "id": request["id"], "result": null });
        self.send(&body.to_string());
    }

    /// Collect frames until none arrives for `quiet`, or `budget` elapses. Use
    /// it only where zero frames is a legitimate outcome (a no-op guard that
    /// must not revalidate); anything asserting on what arrived wants
    /// [`TestServer::drain_after_first`].
    pub fn drain_until_quiet(&mut self, quiet: Duration, budget: Duration) -> Vec<Value> {
        let start = Instant::now();
        let mut frames = Vec::new();
        while start.elapsed() <= budget {
            let Some(frame) = self.try_read_frame(quiet) else {
                break;
            };
            frames.push(frame);
        }
        frames
    }

    /// [`TestServer::drain_until_quiet`], but the wait for the first frame is
    /// the whole budget. `quiet` bounds the gap between frames; spending it
    /// before anything has arrived turns "how fast did the host answer" into
    /// "did the server do the work", which reads green on a dev machine and red
    /// on a loaded runner that needs seconds to publish.
    pub fn drain_after_first(&mut self, quiet: Duration, budget: Duration) -> Vec<Value> {
        let Some(first) = self.try_read_frame(budget) else {
            return Vec::new();
        };
        let mut frames = vec![first];
        frames.extend(self.drain_until_quiet(quiet, budget));
        frames
    }

    /// Stop the server the way an editor does: `shutdown`, `exit`, then close
    /// stdin. A graceful exit lets spawned coverage runtimes flush their
    /// profiles. Kills the server if it has not exited within a bound.
    pub fn stop(&mut self) -> Option<ExitStatus> {
        if self.stopped {
            return None;
        }
        if self.stdin.is_some() {
            let _ = self.try_send(&shutdown_request(SHUTDOWN_ID));
            let _ = self.try_send(&jsonrpc_notification("exit", json!({})));
            drop(self.stdin.take());
        }
        self.stopped = true;
        self.reap(STOP_BOUND)
    }

    /// Stop the server and report whether it answered `shutdown` properly
    /// before `exit` was sent, and how it exited.
    pub fn shutdown(&mut self) -> Shutdown {
        const BOUND: Duration = Duration::from_secs(2);
        if self.stopped {
            return Shutdown {
                responded: false,
                status: None,
            };
        }
        let _ = self.try_send(&shutdown_request(SHUTDOWN_ID));
        let until = Instant::now() + BOUND;
        let mut responded = false;
        while let Ok(frame) = self.next_frame(until) {
            if is_response(&frame) && frame["id"] == SHUTDOWN_ID {
                responded =
                    frame.get("error").is_none() && frame.get("result").is_some_and(Value::is_null);
                break;
            }
        }
        if responded {
            let _ = self.try_send(&jsonrpc_notification("exit", json!({})));
        }
        drop(self.stdin.take());
        self.stopped = true;
        let status = self.reap(BOUND);
        Shutdown { responded, status }
    }

    /// Frames still in flight after the server exited, up to the end of its
    /// stdout.
    pub fn read_to_end(&mut self) -> Vec<Value> {
        let until = self.horizon();
        let mut frames = Vec::new();
        loop {
            match self.next_frame(until) {
                Ok(frame) => frames.push(frame),
                Err(Wait::Closed(_)) => return frames,
                Err(Wait::Timeout) => self.fail("the server's stdout did not close"),
            }
        }
    }

    /// Whether the child has exited, without reaping it.
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    fn try_send(&mut self, body: &str) -> std::io::Result<()> {
        match self.stdin.as_ref() {
            Some(mut stdin) => write_frame_to(&mut stdin, body),
            None => Ok(()),
        }
    }

    fn reap(&mut self, bound: Duration) -> Option<ExitStatus> {
        let until = Instant::now() + bound;
        while Instant::now() < until {
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        None
    }

    fn horizon(&self) -> Instant {
        Instant::now() + self.deadline
    }

    fn expect_frame(&mut self, until: Instant, what: &str) -> Value {
        match self.next_frame(until) {
            Ok(frame) => frame,
            Err(Wait::Timeout) => self.fail(&format!("timed out waiting for {what}")),
            Err(Wait::Closed(reason)) => self.fail(&format!(
                "the server closed its stdout while waiting for {what}: {reason}"
            )),
        }
    }

    fn next_frame(&mut self, until: Instant) -> Result<Value, Wait> {
        if let Some(reason) = &self.ended {
            return Err(Wait::Closed(reason.clone()));
        }
        let remaining = until.saturating_duration_since(Instant::now());
        match self.frames.recv_timeout(remaining) {
            Ok(Ok(frame)) => {
                self.note(&frame);
                Ok(frame)
            }
            Ok(Err(reason)) => {
                self.ended = Some(reason.clone());
                Err(Wait::Closed(reason))
            }
            Err(RecvTimeoutError::Timeout) => Err(Wait::Timeout),
            Err(RecvTimeoutError::Disconnected) => {
                let reason = String::from("the stdout reader stopped");
                self.ended = Some(reason.clone());
                Err(Wait::Closed(reason))
            }
        }
    }

    fn note(&mut self, frame: &Value) {
        let summary = match frame["method"].as_str() {
            Some(method) => method.to_string(),
            None => format!("response {}", frame["id"]),
        };
        if self.seen.len() == SEEN_FRAMES {
            self.seen.pop_front();
        }
        self.seen.push_back(summary);
    }

    fn stderr_tail(&self) -> String {
        let text = self.stderr_text();
        if text.is_empty() {
            return String::from("(empty)");
        }
        let mut start = text.len().saturating_sub(STDERR_TAIL_BYTES);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        text[start..].to_string()
    }

    /// Fail the test with the server's state and stderr in the message.
    pub fn fail(&mut self, message: &str) -> ! {
        let state = match self.child.try_wait() {
            Ok(Some(status)) => {
                let until = Instant::now() + Duration::from_secs(1);
                while !self.stderr.done.load(Ordering::SeqCst) && Instant::now() < until {
                    thread::sleep(Duration::from_millis(10));
                }
                format!("exited with {status}")
            }
            _ => String::from("still running"),
        };
        let seen = self.seen.iter().cloned().collect::<Vec<_>>().join(", ");
        let stderr = self.stderr_tail();
        self.reported = true;
        panic!("{message}\nserver {state}; last frames: [{seen}]\n--- server stderr ---\n{stderr}");
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if thread::panicking() {
            if !self.reported {
                eprintln!("--- server stderr ---\n{}", self.stderr_tail());
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        } else {
            self.stop();
        }
    }
}

fn is_response(frame: &Value) -> bool {
    frame.get("id").is_some() && frame.get("method").is_none()
}

fn write_creating_dirs(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("cannot create the parent directory");
    }
    std::fs::write(path, content).expect("cannot write the test file");
}

#[derive(Default)]
pub struct ServerBuilder {
    rules: Vec<(String, String)>,
    empty_rules: bool,
    files: Vec<(String, Vec<u8>)>,
    vanilla: Option<Vec<(String, Vec<u8>)>>,
    init_opts: Map<String, Value>,
    capabilities: Option<Value>,
    no_root: bool,
    envs: Vec<(OsString, Option<OsString>)>,
    current_dir: Option<PathBuf>,
    scan_release: bool,
    deadline: Option<Duration>,
}

impl ServerBuilder {
    /// Rules text written to `test_rules.cwt` in the rules directory.
    pub fn rules(self, text: &str) -> Self {
        self.rules_file("test_rules.cwt", text)
    }

    /// Give the server a rules directory with nothing in it.
    pub fn empty_rules(mut self) -> Self {
        self.empty_rules = true;
        self
    }

    /// Rules text written to `name` in the rules directory. A later call for
    /// the same name replaces the earlier text.
    pub fn rules_file(mut self, name: &str, text: &str) -> Self {
        let text = text.to_string();
        match self.rules.iter_mut().find(|(existing, _)| existing == name) {
            Some((_, current)) => *current = text,
            None => self.rules.push((name.to_string(), text)),
        }
        self
    }

    /// A file written under the workspace before the server starts.
    pub fn file(mut self, rel: &str, content: impl Into<Vec<u8>>) -> Self {
        self.files.push((rel.to_string(), content.into()));
        self
    }

    /// Give the server an (empty) base-game root.
    pub fn vanilla(mut self) -> Self {
        self.vanilla.get_or_insert_with(Vec::new);
        self
    }

    /// A file written under the base-game root before the server starts.
    pub fn vanilla_file(mut self, rel: &str, content: impl Into<Vec<u8>>) -> Self {
        self.vanilla
            .get_or_insert_with(Vec::new)
            .push((rel.to_string(), content.into()));
        self
    }

    /// Merge `opts`, an object, over the initialization options.
    pub fn init_opts(mut self, opts: Value) -> Self {
        let Value::Object(map) = opts else {
            panic!("internal error: init_opts takes a JSON object, got {opts}");
        };
        self.init_opts.extend(map);
        self
    }

    /// The client capabilities, `{}` unless set.
    pub fn capabilities(mut self, capabilities: Value) -> Self {
        self.capabilities = Some(capabilities);
        self
    }

    /// Initialize without a `rootUri`.
    pub fn no_root(mut self) -> Self {
        self.no_root = true;
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.envs.push((key.into(), Some(value.into())));
        self
    }

    pub fn env_remove(mut self, key: impl Into<OsString>) -> Self {
        self.envs.push((key.into(), None));
        self
    }

    /// The directory the server runs in.
    pub fn current_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(dir.into());
        self
    }

    /// Have the server signal each scan guard release through a marker file
    /// named by [`TestServer::scan_release_path`], kept outside the workspace
    /// so the server never writes into the tree its own scans walk.
    pub fn scan_release_marker(mut self) -> Self {
        self.scan_release = true;
        self
    }

    /// Replace the 60 second wait applied to every read.
    pub fn deadline(mut self, deadline: Duration) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Start the server and run the `initialize` request. The `initialized`
    /// notification is left to the test: it starts the workspace scan, and the
    /// tests that open documents by hand want no scan racing them.
    pub fn spawn(self) -> TestServer {
        let mut server = self.spawn_uninitialized();
        server.initialize();
        server
    }

    /// [`ServerBuilder::spawn`], then `initialized` and the startup scan run to
    /// its end.
    pub fn spawn_scanned(self) -> TestServer {
        let mut server = self.spawn();
        server.initialized();
        server.wait_for_scan_done();
        server
    }

    /// Start the server and send nothing, for a test that drives the
    /// handshake, or breaks it, itself.
    pub fn spawn_uninitialized(self) -> TestServer {
        let workspace = tempfile::tempdir().expect("cannot create the workspace directory");
        for (rel, content) in &self.files {
            write_creating_dirs(&workspace.path().join(rel), content);
        }
        let rules = (self.empty_rules || !self.rules.is_empty()).then(|| {
            let dir = tempfile::tempdir().expect("cannot create the rules directory");
            for (name, text) in &self.rules {
                write_creating_dirs(&dir.path().join(name), text.as_bytes());
            }
            dir
        });
        let vanilla = self.vanilla.as_ref().map(|files| {
            let dir = tempfile::tempdir().expect("cannot create the base-game directory");
            for (rel, content) in files {
                write_creating_dirs(&dir.path().join(rel), content);
            }
            dir
        });

        let mut opts = Map::new();
        if let Some(rules) = &rules {
            opts.insert("language".into(), json!("hoi4"));
            opts.insert("rulesCache".into(), json!(rules.path().to_string_lossy()));
        }
        if let Some(vanilla) = &vanilla {
            opts.insert("vanilla".into(), json!(vanilla.path().to_string_lossy()));
        }
        opts.extend(self.init_opts);
        let mut params = json!({
            "processId": std::process::id(),
            "capabilities": self.capabilities.unwrap_or_else(|| json!({})),
        });
        if !self.no_root {
            params["rootUri"] = json!(path_uri(workspace.path()));
        }
        if !opts.is_empty() {
            params["initializationOptions"] = Value::Object(opts);
        }

        let mut command = server_command();
        for (key, value) in &self.envs {
            match value {
                Some(value) => command.env(key, value),
                None => command.env_remove(key),
            };
        }
        if let Some(dir) = &self.current_dir {
            command.current_dir(dir);
        }
        if self.scan_release {
            command.env(
                "CWTOOLS_SCAN_RELEASE_FILE",
                scan_release_path(workspace.path()),
            );
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("cannot spawn cwtools-server");
        let stdin = child.stdin.take();
        let (tx, frames) = mpsc::channel();
        pump_frames(child.stdout.take().expect("piped stdout"), tx);
        let stderr = Arc::new(StderrLog::default());
        capture_stderr(
            child.stderr.take().expect("piped stderr"),
            Arc::clone(&stderr),
        );

        TestServer {
            child,
            stdin,
            frames,
            stderr,
            workspace,
            rules,
            vanilla,
            init_params: params,
            init: Value::Null,
            deadline: self.deadline.unwrap_or(READ_DEADLINE),
            next_id: 2,
            seen: VecDeque::new(),
            ended: None,
            stopped: false,
            reported: false,
        }
    }
}

/// The one place the server binary is resolved. `RUST_LOG` is emptied so the
/// default run logs nothing; a test that wants discovery sets its own HOME
/// afterwards, and the later `env` wins.
fn server_command() -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin("cwtools-server"));
    command.env("RUST_LOG", "");
    command.envs(scratch_home_env());
    command
}

/// Forward every frame on stdout onto a channel, then the reason it stopped.
fn pump_frames(stdout: ChildStdout, tx: Sender<Result<Value, String>>) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_frame_from(&mut reader) {
                Ok(frame) => {
                    if tx.send(Ok(frame)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(error.to_string()));
                    return;
                }
            }
        }
    });
}

fn capture_stderr(mut pipe: ChildStderr, log: Arc<StderrLog>) {
    thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(read) = pipe.read(&mut chunk) {
            if read == 0 {
                break;
            }
            let mut text = log.text.lock().unwrap_or_else(PoisonError::into_inner);
            text.extend_from_slice(&chunk[..read]);
        }
        log.done.store(true, Ordering::SeqCst);
    });
}
