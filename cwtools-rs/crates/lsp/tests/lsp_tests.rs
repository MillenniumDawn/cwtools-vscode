use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

/// Build a URI the way the server does. `format!("file://{path}")` is only
/// valid where paths start with `/`; on Windows it yields `file://C:\a\b`,
/// which never matches the server's `file:///C:/a/b`.
fn path_uri(path: impl AsRef<std::path::Path>) -> String {
    let path = path.as_ref();
    tower_lsp::lsp_types::Url::from_file_path(path)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| format!("file://{}", path.display()))
}

/// A canonical URI rewritten the way VS Code spells it: on Windows the drive
/// letter is lower-cased and its colon percent-encoded (`file:///d%3A/…`), which
/// is not what `Url::from_file_path` writes (#319). A no-op elsewhere, where the
/// two already agree.
fn client_spelling(uri: &str) -> String {
    #[cfg(windows)]
    {
        if let Some(rest) = uri.strip_prefix("file:///")
            && let Some((drive, tail)) = rest.split_once(':')
            && drive.len() == 1
        {
            return format!("file:///{}%3A{tail}", drive.to_ascii_lowercase());
        }
    }
    uri.to_string()
}

/// Scratch home for every server this binary spawns. It has to outlive the
/// children, so it is a static: leaking the dir at process exit is the point,
/// the OS temp cleaner takes it from there.
static SCRATCH_HOME: std::sync::LazyLock<tempfile::TempDir> =
    std::sync::LazyLock::new(|| tempfile::tempdir().expect("failed to create scratch home"));

fn cwtools_server_cmd() -> Command {
    let bin = assert_cmd::cargo::cargo_bin("cwtools-server");
    let mut cmd = Command::new(bin);
    cmd.env("RUST_LOG", "");
    // Pin HOME and the cache dirs at a scratch tree so a server never discovers
    // a locally installed base game (and spends the test scanning it), and never
    // writes into the developer's real cwtools cache. A test that wants
    // discovery sets its own HOME afterwards; the later `.env` wins.
    let home = SCRATCH_HOME.path();
    cmd.env("HOME", home);
    cmd.env("XDG_CACHE_HOME", home.join("cache"));
    cmd.env("LOCALAPPDATA", home.join("localappdata"));
    cmd
}

fn write_frame(child: &mut std::process::Child, body: &str) -> std::io::Result<()> {
    write_frame_to(child.stdin.as_mut().unwrap(), body)
}

/// [`write_frame`] against a bare stdin handle, for the worker thread in
/// [`run_with_deadline`] which owns the pipe rather than the `Child`.
fn write_frame_to(stdin: &mut impl Write, body: &str) -> std::io::Result<()> {
    write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    stdin.flush()
}

/// Hand the server's pipes to a worker thread and wait at most `secs` for its
/// result. `read_frame` blocks with no timeout, so a test that waits on a
/// notification the server never sends would wedge the whole binary instead of
/// failing on its own. `None` means the deadline passed.
fn run_with_deadline<T: Send + 'static>(
    mut stdin: std::process::ChildStdin,
    mut reader: BufReader<std::process::ChildStdout>,
    secs: u64,
    f: impl FnOnce(&mut std::process::ChildStdin, &mut BufReader<std::process::ChildStdout>) -> T
    + Send
    + 'static,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f(&mut stdin, &mut reader));
    });
    rx.recv_timeout(std::time::Duration::from_secs(secs)).ok()
}

/// Read one LSP frame, or `Err` once the server closes its stdout.
///
/// `read_line` reports EOF as `Ok(0)`, which used to fall through the empty
/// header line into `Ok(String::new())` — indistinguishable from a frame with
/// no body. Every caller here branches on `Err` to stop and skips an empty
/// `Ok`, so a dead server produced an endless run of empty frames instead:
/// [`spawn_frame_collector`]'s thread span at 100% of a core for the rest of
/// the binary's run rather than dropping its sender, and the waits built on it
/// timed out blaming a missing notification. Reporting EOF as the error the
/// callers already handle is what makes a server exit visible (#198).
fn read_frame(reader: &mut BufReader<std::process::ChildStdout>) -> std::io::Result<String> {
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the server closed its stdout",
            ));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some(val) = trimmed.strip_prefix("Content-Length: ") {
            content_length = val.parse().unwrap_or(0);
        }
    }
    if content_length == 0 {
        return Ok(String::new());
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    String::from_utf8(body).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn read_response(reader: &mut BufReader<std::process::ChildStdout>) -> std::io::Result<String> {
    loop {
        let raw = read_frame(reader)?;
        if raw.is_empty() {
            return Ok(raw);
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
            if val.get("id").is_some() {
                return Ok(raw);
            }
        } else {
            return Ok(raw);
        }
    }
}

/// Drain server frames until the `publishDiagnostics` notification whose URI
/// ends with `rel_path` arrives. did_open publishes diagnostics for a file only
/// after its index write lands, so this is the readiness signal that the file's
/// exports (value_set members, enum values, type instances) are queryable.
/// Without it a following completion races the index write — tower-lsp dispatches
/// handlers `buffer_unordered`, so there is no happens-before between a notify
/// handler finishing and the next request's handler running. Matches by path
/// suffix since the server canonicalises the URI (`file://` vs `file:///`).
fn wait_for_diagnostics(reader: &mut BufReader<std::process::ChildStdout>, rel_path: &str) {
    for _ in 0..400 {
        let raw = match read_frame(reader) {
            Ok(r) => r,
            Err(_) => return,
        };
        if raw.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw)
            && v["method"] == "textDocument/publishDiagnostics"
            && v["params"]["uri"]
                .as_str()
                .is_some_and(|u| u.ends_with(rel_path))
        {
            return;
        }
    }
    panic!("no publishDiagnostics for {rel_path}");
}

fn jsonrpc_request(id: i64, method: &str, params: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
    .to_string()
}

fn jsonrpc_notification(method: &str, params: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    })
    .to_string()
}

// ── Full lifecycle: initialize → initialized → shutdown ──────────────────────

#[test]
fn test_lsp_rejects_an_oversized_frame_without_waiting_for_the_body() {
    const MAX_LSP_FRAME_BYTES: usize = 64 * 1024 * 1024;

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    {
        let stdin = child.stdin.as_mut().unwrap();
        write!(stdin, "Content-Length: {}\r\n\r\n", MAX_LSP_FRAME_BYTES + 1).unwrap();
        stdin.flush().unwrap();
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    child.kill().ok();
    child.wait().ok();
    panic!("server waited for the oversized frame body");
}

#[test]
fn test_lsp_full_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    let uri = path_uri(tmp.path());

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");

    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": uri,
            "capabilities": {}
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let resp_str = read_response(&mut reader).expect("no init response");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(resp["id"], 1);
    assert!(resp["result"]["capabilities"].is_object());

    let body = jsonrpc_notification("initialized", serde_json::json!({}));
    write_frame(&mut child, &body).unwrap();

    let body = jsonrpc_request(2, "shutdown", serde_json::json!(null));
    write_frame(&mut child, &body).unwrap();
    let resp_str = read_response(&mut reader).expect("no shutdown response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(resp["id"], 2);
    assert!(resp["result"].is_null());
}

// ── Unknown notification does not crash ──────────────────────────────────────

#[test]
fn test_lsp_unknown_notification_does_not_crash() {
    let tmp = tempfile::tempdir().unwrap();
    let uri = path_uri(tmp.path());

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");

    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": uri,
            "capabilities": {}
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader);

    let body = jsonrpc_notification("nonexistent/method", serde_json::json!({}));
    write_frame(&mut child, &body).unwrap();

    let body = jsonrpc_request(99, "shutdown", serde_json::json!(null));
    write_frame(&mut child, &body).unwrap();
    let resp_str = read_response(&mut reader).expect("server should respond");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(resp["id"], 99);
}

// ── Client URI spellings fold onto one index key (#319) ──────────────────────

/// A client may spell a `file:` URI differently from `Url::from_file_path`
/// without naming a different file — VS Code percent-encodes the Windows drive
/// colon (`file:///d%3A/…`), and percent-encoding is legal for any path byte.
/// Every index the server keeps is a map on the raw URI string, so a spelling
/// it does not fold is a second key for a file it already has: the workspace
/// scan's "skip documents the editor has open" guard misses, the file is
/// indexed twice, and CW261 reports every instance of a `## unique` type in it
/// as defined twice.
///
/// Encoding a plain letter (`dup.txt` → `%64up.txt`) exercises the same fold on
/// every platform; the drive-letter half is covered by the `paths` unit tests.
/// The workspace folder is sent in the client's spelling too, because
/// `workspace_prefix` is derived from it and every logical path is stripped
/// against it.
#[test]
fn test_did_open_folds_a_percent_encoded_uri_onto_the_canonical_one() {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("test_rules.cwt"), COMPLETION_RULES).unwrap();

    let rel_path = "common/decisions/dup.txt";
    let file_path = ws.path().join(rel_path);
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    let text = "my_decision = {\n    cost = 5\n}\n";
    std::fs::write(&file_path, text).unwrap();

    let canonical_uri = path_uri(&file_path);
    // The client's spelling: same file, `d` written as `%64`.
    let client_uri = client_spelling(&canonical_uri).replace("dup.txt", "%64up.txt");
    assert_ne!(client_uri, canonical_uri, "the spellings must differ");
    let client_root = client_spelling(&path_uri(ws.path()));

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": client_root,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");

    let body = jsonrpc_notification(
        "textDocument/didOpen",
        serde_json::json!({
            "textDocument": {
                "uri": client_uri,
                "languageId": "hoi4",
                "version": 1,
                "text": text,
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();

    // The URI the diagnostics come back under is the key the document was
    // indexed under, so it is what proves the fold happened.
    let mut published = None;
    for _ in 0..400 {
        let raw = match read_frame(&mut reader) {
            Ok(r) => r,
            Err(_) => break,
        };
        if raw.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw)
            && v["method"] == "textDocument/publishDiagnostics"
            && let Some(u) = v["params"]["uri"].as_str()
            && u.ends_with("up.txt")
        {
            published = Some(u.to_string());
            break;
        }
    }
    child.kill().ok();

    assert_eq!(
        published.as_deref(),
        Some(canonical_uri.as_str()),
        "did_open must key the document by the canonical URI, not the client's spelling"
    );
}

// ── Context-aware completion round-trips ─────────────────────────────────────

/// Rules covering both regressions from cwtools-vscode#11: trigger aliases
/// (`has_completed_focus`) and the MIO `equipment_bonus` typed-key descent
/// into `alias_name[modifier]`.
const COMPLETION_RULES: &str = r#"
types = {
    type[focus] = {
        path = "game/common/national_focus"
    }
    type[decision] = {
        path = "game/common/decisions"
    }
    type[mio] = {
        path = "game/common/military_industrial_organization/organizations"
    }
    type[event] = {
        path = "game/events"
    }
}
decision = {
    allowed = {
        alias_name[trigger] = alias_match_left[trigger]
    }
    cost = int
    set_math = {
        value_set[variable] = math_expr
        value_set[variable] = scalar
    }
}
focus = {
    id = scalar
    x = int
    y = int
    cost = float
    completion_reward = {
        alias_name[effect] = alias_match_left[effect]
    }
    available = {
        alias_name[trigger] = alias_match_left[trigger]
    }
}
event = {
    id = scalar
    title = scalar
    trigger = {
        alias_name[trigger] = alias_match_left[trigger]
    }
    immediate = {
        alias_name[effect] = alias_match_left[effect]
    }
    option = {
        name = scalar
    }
}
alias[mathexpr:add] = math_expr
alias[mathexpr:subtract] = math_expr
alias[mathexpr:multiply] = math_expr
mio = {
    name = scalar
    equipment_bonus = {
        <equipment> = {
            alias_name[modifier] = alias_match_left[modifier]
        }
    }
}
alias[trigger:has_completed_focus] = <focus>
### Always evaluates to true.
alias[trigger:always] = bool
alias[effect:add_political_power] = int
modifiers = {
    build_cost_ic = economy
    production_speed_factor = economy
}
"#;

/// Spawn a server with COMPLETION_RULES loaded, open `rel_path` with `text`,
/// request completion at (line0, char0), and return the completion labels.
fn completion_labels(rel_path: &str, text: &str, line0: u32, char0: u32) -> Vec<String> {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("test_rules.cwt"), COMPLETION_RULES).unwrap();

    let file_path = ws.path().join(rel_path);
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, text).unwrap();

    let ws_uri = path_uri(ws.path());
    let doc_uri = path_uri(&file_path);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");

    let body = jsonrpc_notification(
        "textDocument/didOpen",
        serde_json::json!({
            "textDocument": {
                "uri": doc_uri,
                "languageId": "hoi4",
                "version": 1,
                "text": text,
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();
    // Wait for the file's index write to land before requesting completion.
    wait_for_diagnostics(&mut reader, rel_path);

    let body = jsonrpc_request(
        2,
        "textDocument/completion",
        serde_json::json!({
            "textDocument": { "uri": doc_uri },
            "position": { "line": line0, "character": char0 },
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let resp_str = read_response(&mut reader).expect("no completion response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(resp["id"], 2, "got: {}", resp_str);
    let items = resp["result"]
        .as_array()
        .cloned()
        .or_else(|| resp["result"]["items"].as_array().cloned())
        .unwrap_or_default();
    items
        .iter()
        .filter_map(|i| i["label"].as_str().map(|s| s.to_string()))
        .collect()
}

#[test]
fn test_completion_trigger_alias_in_allowed_block() {
    let text = "my_decision = {\n    allowed = {\n        \n    }\n    cost = 5\n}\n";
    // Cursor on the blank line inside `allowed = { ... }` (line 2, col 8).
    let labels = completion_labels("common/decisions/test.txt", text, 2, 8);
    assert!(
        labels.iter().any(|l| l == "has_completed_focus"),
        "trigger aliases should be offered inside allowed, got: {:?}",
        labels
    );
    assert!(labels.iter().any(|l| l == "always"), "got: {:?}", labels);
    // The sibling decision field `cost` lives one level up, not inside `allowed`.
    // It must not leak in, or context-awareness is broken.
    assert!(
        !labels.iter().any(|l| l == "cost"),
        "out-of-context field `cost` should not appear inside allowed, got: {:?}",
        labels
    );
}

/// Send a `completionItem/resolve` request for `item` over an already-running
/// session and return the resolved `CompletionItem` JSON. Mirrors the shape of
/// `jsonrpc_request`/`write_frame`/`read_response` used by the completion
/// helpers above — `completionItem/resolve`'s params ARE the completion item
/// itself (no wrapper), per the LSP spec.
fn resolve_request(
    child: &mut std::process::Child,
    reader: &mut BufReader<std::process::ChildStdout>,
    id: i64,
    item: serde_json::Value,
) -> serde_json::Value {
    let body = jsonrpc_request(id, "completionItem/resolve", item);
    write_frame(child, &body).unwrap();
    let resp_str = read_response(reader).expect("no resolve response");
    serde_json::from_str(&resp_str).unwrap()
}

#[test]
fn test_completion_resolve_fills_alias_documentation() {
    // perf/completion-responsiveness: the `### docs` comment on an alias is
    // deferred out of the initial completion response (payload shrink) and
    // recomputed by `completionItem/resolve` — see `completion::resolve`.
    // `always` carries a `### Always evaluates to true.` doc in COMPLETION_RULES.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("test_rules.cwt"), COMPLETION_RULES).unwrap();
    let rel_path = "common/decisions/test.txt";
    let text = "my_decision = {\n    allowed = {\n        \n    }\n    cost = 5\n}\n";
    let file_path = ws.path().join(rel_path);
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, text).unwrap();
    let ws_uri = path_uri(ws.path());
    let doc_uri = path_uri(&file_path);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let init_resp = read_response(&mut reader).expect("no init response");
    // The server must advertise resolve support so the client knows to call
    // completionItem/resolve at all.
    let init: serde_json::Value = serde_json::from_str(&init_resp).unwrap();
    assert_eq!(
        init["result"]["capabilities"]["completionProvider"]["resolveProvider"], true,
        "server must advertise completion resolveProvider, got: {}",
        init_resp
    );

    let body = jsonrpc_notification(
        "textDocument/didOpen",
        serde_json::json!({
            "textDocument": {
                "uri": doc_uri,
                "languageId": "hoi4",
                "version": 1,
                "text": text,
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();
    wait_for_diagnostics(&mut reader, rel_path);

    let body = jsonrpc_request(
        2,
        "textDocument/completion",
        serde_json::json!({
            "textDocument": { "uri": doc_uri },
            // Blank line inside `allowed = { ... }` (line 2, col 8).
            "position": { "line": 2, "character": 8 },
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let resp_str = read_response(&mut reader).expect("no completion response");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let items = resp["result"]["items"]
        .as_array()
        .or_else(|| resp["result"].as_array())
        .cloned()
        .unwrap_or_default();
    let always = items
        .iter()
        .find(|i| i["label"] == "always")
        .unwrap_or_else(|| panic!("`always` not offered, got: {}", resp_str));
    // The initial response must NOT carry the deferred documentation — this
    // is the payload shrink the deferral exists for.
    assert!(
        always.get("documentation").is_none() || always["documentation"].is_null(),
        "documentation must be deferred out of the initial response, got: {}",
        always
    );
    assert!(
        !always["data"].is_null(),
        "a deferred item must carry `data` for resolve to key off, got: {}",
        always
    );

    let resolved = resolve_request(&mut child, &mut reader, 3, always.clone());
    child.kill().ok();
    let doc = &resolved["result"]["documentation"];
    let doc_text = doc.as_str().or_else(|| doc["value"].as_str());
    assert_eq!(
        doc_text,
        Some("Always evaluates to true."),
        "resolve should repopulate the alias's ### doc, got: {}",
        resolved
    );
}

#[test]
fn test_completion_on_blank_line_after_field() {
    // Completing on a fresh line after `cost = 5` must offer the block's other
    // fields (the parser's leaf range absorbs the trailing blank line, which used
    // to resolve to the cost value and return nothing) (cwtools-vscode#20).
    let text = "my_decision = {\n    cost = 5\n    \n}\n";
    // Cursor on the blank line after `cost = 5` (line 2, col 4).
    let labels = completion_labels("common/decisions/test.txt", text, 2, 4);
    assert!(
        labels.iter().any(|l| l == "allowed"),
        "blank line after a field should offer sibling fields, got: {:?}",
        labels
    );
}

#[test]
fn test_small_context_completion_is_complete() {
    // Strategy A (perf/completion-responsiveness): a resolved-context list at
    // or under CONTEXT_COMPLETE_THRESHOLD is returned unfiltered and marked
    // `is_incomplete: false` — small enough that VS Code filters and
    // re-filters it client-side for free as the user keeps typing, with zero
    // further requests until a word boundary or trigger char forces a
    // re-query. (Large/filtered/fallback lists still must stay
    // `is_incomplete: true` — see test_completion_in_half_typed_state.)
    let resp = completion_response(
        "common/decisions/test.txt",
        "my_decision = {\n    cost = 5\n}\n",
        1,
        4,
    );
    let is_incomplete = resp["result"]["isIncomplete"]
        .as_bool()
        .or_else(|| resp["result"]["is_incomplete"].as_bool());
    assert_eq!(
        is_incomplete,
        Some(false),
        "a small resolved-context list must be marked complete, got: {}",
        resp
    );
}

#[test]
fn test_completion_after_change_with_stale_ast_stays_incomplete() {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("test_rules.cwt"), COMPLETION_RULES).unwrap();
    let rel_path = "common/decisions/test.txt";
    let initial_text = "my_decision = {\n    cost = 5\n    \n}\n";
    let changed_text = "my_decision = {\n    allowed = {\n        \n";
    let file_path = ws.path().join(rel_path);
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, initial_text).unwrap();
    let ws_uri = path_uri(ws.path());
    let doc_uri = path_uri(&file_path);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": doc_uri,
                    "languageId": "hoi4",
                    "version": 1,
                    "text": initial_text,
                }
            }),
        ),
    )
    .unwrap();
    wait_for_diagnostics(&mut reader, rel_path);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": doc_uri, "version": 2 },
                "contentChanges": [{ "text": changed_text }]
            }),
        ),
    )
    .unwrap();

    let body = jsonrpc_request(
        2,
        "textDocument/completion",
        serde_json::json!({
            "textDocument": { "uri": doc_uri },
            "position": { "line": 2, "character": 8 },
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let resp_str = read_response(&mut reader).expect("no completion response");
    child.kill().ok();
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let is_incomplete = resp["result"]["isIncomplete"]
        .as_bool()
        .or_else(|| resp["result"]["is_incomplete"].as_bool());
    assert_eq!(
        is_incomplete,
        Some(true),
        "completion resolved from a stale or dirty AST must stay incomplete, got: {}",
        resp
    );
}

#[test]
fn test_completion_in_half_typed_state() {
    // User scenario: type a partial block, walk away, come back, start
    // typing more. The parser fails on the partial text, so the last good
    // AST is None and `rules_at_pos` has nothing to walk. Completion must
    // still return SOMETHING (the cached fallback, the loc list, or a
    // re-parsed AST) and flag `is_incomplete` so the popup re-engages on
    // the next keystroke. Regression for the "super unresponsive when you
    // come back to a half-typed file" complaint.
    let text =
        "my_decision = {\n    allowed = {\n        has_completed_focus = \n    }\n    cost = 5\n";
    let resp = completion_response("common/decisions/test.txt", text, 3, 32);
    // Either a context-aware list (from the re-parsed AST) or the fallback
    // list (if even the re-parse failed) — both are valid, but the
    // response must not be empty and must be marked incomplete.
    let items = resp["result"]["items"]
        .as_array()
        .or_else(|| resp["result"].as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        !items.is_empty(),
        "half-typed state must return some completions, got: {}",
        resp
    );
    let is_incomplete = resp["result"]["isIncomplete"]
        .as_bool()
        .or_else(|| resp["result"]["is_incomplete"].as_bool());
    assert_eq!(
        is_incomplete,
        Some(true),
        "half-typed completion must be marked is_incomplete, got: {}",
        resp
    );
}

#[test]
fn test_completion_items_carry_text_edit_anchor() {
    // Every item must carry an explicit `textEdit` so the client filters and
    // inserts against the cursor range instead of guessing a word boundary (the
    // guess breaks right after a backspace across `=` / `<` / `>`). Blank-line
    // key position inside the decision block (line 2, col 4): the range anchors
    // at the cursor and `filterText` is pinned to the label. (Non-empty token
    // ranges are covered by paths::test_current_token_range.)
    let text = "my_decision = {\n    cost = 5\n    \n}\n";
    let resp = completion_response("common/decisions/test.txt", text, 2, 4);
    let items = resp["result"]["items"]
        .as_array()
        .or_else(|| resp["result"].as_array())
        .cloned()
        .unwrap_or_default();
    assert!(!items.is_empty(), "expected items, got: {}", resp);
    let allowed = items
        .iter()
        .find(|i| i["label"] == "allowed")
        .unwrap_or_else(|| panic!("`allowed` not offered, got: {}", resp));
    let range = &allowed["textEdit"]["range"];
    assert_eq!(range["start"]["line"], 2, "got: {}", allowed);
    assert_eq!(
        range["start"]["character"], 4,
        "replace range must anchor at the cursor token, got: {}",
        allowed
    );
    assert_eq!(range["end"]["character"], 4, "got: {}", allowed);
    // filterText is pinned to the label so the client never filters a snippet.
    assert_eq!(allowed["filterText"], "allowed", "got: {}", allowed);
}

#[test]
fn test_completion_offers_mathexpr_operators_in_math_block() {
    // Inside a `math_expr` block (`set_math = { x = { | } }`), completion must
    // offer `value` and the registered mathexpr operators (add/subtract/…),
    // resolved by the position descent into the synthesized math-clause rules —
    // not the flat fallback.
    let text = "d = {\n    set_math = {\n        x = {\n            \n        }\n    }\n}\n";
    // Blank line inside the innermost math block `x = { ... }` (line 3, col 12).
    let labels = completion_labels("common/decisions/test.txt", text, 3, 12);
    assert!(
        labels.iter().any(|l| l == "add") && labels.iter().any(|l| l == "subtract"),
        "math operators should be offered inside a math block, got: {:?}",
        labels
    );
    assert!(
        labels.iter().any(|l| l == "value"),
        "`value` base should be offered inside a math block, got: {:?}",
        labels
    );
}

#[test]
fn test_completion_math_block_value_excludes_effects() {
    // Value position after `add = ` inside a math block (line 3, col 18).
    let text = "d = {\n    set_math = {\n        x = {\n            add = \n        }\n    }\n}\n";
    let labels = completion_labels("common/decisions/test.txt", text, 3, 18);
    assert!(
        !labels.iter().any(|l| l == "add_political_power"),
        "effects must not appear at math value position, got: {:?}",
        labels
    );
}

#[test]
fn test_completion_math_leaf_value_excludes_effects() {
    // Value position after `x = ` at the set_variable level (line 2, col 12).
    let text = "d = {\n    set_math = {\n        x = \n    }\n}\n";
    let labels = completion_labels("common/decisions/test.txt", text, 2, 12);
    assert!(
        !labels.iter().any(|l| l == "add_political_power"),
        "effects must not appear at math leaf value position, got: {:?}",
        labels
    );
}

fn completion_response(rel_path: &str, text: &str, line0: u32, char0: u32) -> serde_json::Value {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("test_rules.cwt"), COMPLETION_RULES).unwrap();
    let file_path = ws.path().join(rel_path);
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, text).unwrap();
    let ws_uri = path_uri(ws.path());
    let doc_uri = path_uri(&file_path);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");

    let body = jsonrpc_notification(
        "textDocument/didOpen",
        serde_json::json!({
            "textDocument": {
                "uri": doc_uri,
                "languageId": "hoi4",
                "version": 1,
                "text": text,
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();
    wait_for_diagnostics(&mut reader, rel_path);

    let body = jsonrpc_request(
        2,
        "textDocument/completion",
        serde_json::json!({
            "textDocument": { "uri": doc_uri },
            "position": { "line": line0, "character": char0 },
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let resp_str = read_response(&mut reader).expect("no completion response");
    child.kill().ok();
    serde_json::from_str(&resp_str).unwrap()
}

#[test]
fn test_cwt_completion_replaces_partial_constructs() {
    for (text, line, character, label, start) in [
        ("alias[", 0, 6, "alias", 0),
        ("types = {\n  type[", 1, 7, "type", 2),
        ("enums = {\n  enum[", 1, 7, "enum", 2),
        ("rule = filepath[", 0, 16, "filepath[folder,extension]", 7),
    ] {
        let response = completion_response("config/test.cwt", text, line, character);
        assert_eq!(response["result"]["isIncomplete"], true, "{response}");
        let item = response["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["label"] == label)
            .unwrap_or_else(|| panic!("missing {label}: {response}"));
        assert_eq!(item["textEdit"]["range"]["start"]["character"], start);
        assert_eq!(item["textEdit"]["range"]["end"]["character"], character);
    }
}

#[test]
fn test_localisation_completion_range_uses_utf16_columns() {
    let text = "l_english:\n KNOWN_KEY:0 \"known\"\n TEST:0 \"😀 $key\"";
    let response = completion_response("localisation/test_l_english.yml", text, 2, 16);
    let items = response["result"]
        .as_array()
        .or_else(|| response["result"]["items"].as_array())
        .unwrap_or_else(|| panic!("missing completion items: {response}"));
    let item = items
        .iter()
        .find(|item| item["label"] == "known_key")
        .unwrap_or_else(|| panic!("missing loc key: {response}"));
    assert_eq!(item["textEdit"]["range"]["start"]["character"], 13);
    assert_eq!(item["textEdit"]["range"]["end"]["character"], 16);
}

#[test]
fn test_localisation_completion_range_uses_utf32_columns() {
    let text = "l_english:\n KNOWN_KEY:0 \"known\"\n TEST:0 \"😀 $key\"";
    let files = &[("localisation/test_l_english.yml", text)];
    let caps = serde_json::json!({
        "general": { "positionEncodings": ["utf-32"] }
    });
    let result = feature_request(
        COMPLETION_RULES,
        files,
        &["localisation/test_l_english.yml"],
        caps,
        "localisation/test_l_english.yml",
        "textDocument/completion",
        serde_json::json!({ "position": { "line": 2, "character": 15 } }),
    );
    let items = result
        .as_array()
        .or_else(|| result["items"].as_array())
        .unwrap_or_else(|| panic!("missing completion items: {result}"));
    let item = items
        .iter()
        .find(|item| item["label"] == "known_key")
        .unwrap_or_else(|| panic!("missing loc key: {result}"));
    assert_eq!(item["textEdit"]["range"]["start"]["character"], 12);
    assert_eq!(item["textEdit"]["range"]["end"]["character"], 15);
}

#[test]
fn test_completion_skips_resource_files() {
    let response = completion_response("gfx/interface/test.dds", "anything", 0, 8);
    assert!(response["result"].is_null(), "{response}");
}

#[test]
fn test_completion_modifiers_in_mio_equipment_bonus() {
    let text = "my_org = {\n    name = org\n    equipment_bonus = {\n        some_equipment = {\n            \n        }\n    }\n}\n";
    // Cursor on the blank line inside the equipment block (line 4, col 12).
    let labels = completion_labels(
        "common/military_industrial_organization/organizations/test.txt",
        text,
        4,
        12,
    );
    assert!(
        labels.iter().any(|l| l == "build_cost_ic"),
        "modifier names should be offered inside an equipment_bonus entry, got: {:?}",
        labels
    );
    // `name` is a top-level mio field, not a modifier; it must not leak into the
    // equipment entry's modifier completions.
    assert!(
        !labels.iter().any(|l| l == "name"),
        "out-of-context field `name` should not appear inside an equipment entry, got: {:?}",
        labels
    );
}

// ── Dynamic value completion round-trips (real MIO/trigger shapes) ───────────

/// Rules mirroring the REAL HOI4 config shapes: MIO trait equipment_bonus is
/// keyed by enum[equipment_stat] (a complex enum collected from
/// common/script_enums.txt), has_idea reads enum[idea_name], and
/// has_country_flag reads value[country_flag] (members written by
/// set_country_flag).
const DYNAMIC_RULES: &str = r#"
types = {
    type[mio] = {
        path = "game/common/military_industrial_organization/organizations"
    }
    type[decision] = {
        path = "game/common/decisions"
    }
}
enums = {
    complex_enum[equipment_stat] = {
        path = "game/common"
        path_file = "script_enums.txt"
        start_from_root = yes
        name = {
            script_enum_equipment_stat = {
                enum_name
            }
        }
    }
    complex_enum[idea_name] = {
        path = "game/common/ideas"
        name = {
            scalar = {
                enum_name = {
                }
            }
        }
    }
}
mio = {
    name = scalar
    trait = {
        token = scalar
        equipment_bonus = {
            ## cardinality = ~1..inf
            enum[equipment_stat] = variable_field
            ## cardinality = 0..1
            instant = bool
        }
    }
}
decision = {
    allowed = {
        alias_name[trigger] = alias_match_left[trigger]
    }
    complete_effect = {
        alias_name[effect] = alias_match_left[effect]
    }
    cost = int
}
### Does the country have this idea
alias[trigger:has_idea] = enum[idea_name]
### Has the country flag been set
alias[trigger:has_country_flag] = value[country_flag]
alias[effect:set_country_flag] = value_set[country_flag]
"#;

/// Open `extra_files` (indexed on didOpen) then request completion in `text`
/// at (line0, char0); returns the labels.
fn completion_labels_with_files(
    rel_path: &str,
    text: &str,
    extra_files: &[(&str, &str)],
    line0: u32,
    char0: u32,
) -> Vec<String> {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("test_rules.cwt"), DYNAMIC_RULES).unwrap();

    for (rel, content) in extra_files.iter().chain([&(rel_path, text)]) {
        let p = ws.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
    }

    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");

    // didOpen every file so each is indexed deterministically (no reliance on
    // the async workspace scan). Wait for each file's diagnostics before sending
    // the next message so its index write (value_set members, enum values, type
    // instances) is queryable when the cross-file completion runs.
    for (rel, content) in extra_files.iter().chain([&(rel_path, text)]) {
        let uri = path_uri(ws.path().join(rel));
        let body = jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "hoi4",
                    "version": 1,
                    "text": content,
                }
            }),
        );
        write_frame(&mut child, &body).unwrap();
        wait_for_diagnostics(&mut reader, rel);
    }

    let doc_uri = path_uri(ws.path().join(rel_path));
    let body = jsonrpc_request(
        2,
        "textDocument/completion",
        serde_json::json!({
            "textDocument": { "uri": doc_uri },
            "position": { "line": line0, "character": char0 },
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let resp_str = read_response(&mut reader).expect("no completion response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(resp["id"], 2, "got: {}", resp_str);
    let items = resp["result"]
        .as_array()
        .cloned()
        .or_else(|| resp["result"]["items"].as_array().cloned())
        .unwrap_or_default();
    items
        .iter()
        .filter_map(|i| i["label"].as_str().map(|s| s.to_string()))
        .collect()
}

const SCRIPT_ENUMS: (&str, &str) = (
    "common/script_enums.txt",
    "script_enum_equipment_stat = {\n\tbuild_cost_ic\n\treliability\n\tsoft_attack\n}\n",
);

#[test]
fn test_completion_equipment_stats_in_mio_trait_bonus() {
    // The real MIO shape: equipment_bonus keyed by the equipment_stat complex
    // enum, collected from common/script_enums.txt.
    let text = "my_org = {\n    name = org\n    trait = {\n        token = t1\n        equipment_bonus = {\n            \n        }\n    }\n}\n";
    // Cursor on the blank line inside equipment_bonus (line 5, col 12).
    let labels = completion_labels_with_files(
        "common/military_industrial_organization/organizations/test.txt",
        text,
        &[SCRIPT_ENUMS],
        5,
        12,
    );
    assert!(
        labels.iter().any(|l| l == "build_cost_ic"),
        "equipment stats should be offered, got: {:?}",
        labels
    );
    assert!(
        labels.iter().any(|l| l == "soft_attack"),
        "got: {:?}",
        labels
    );
    assert!(labels.iter().any(|l| l == "instant"), "got: {:?}", labels);
}

#[test]
fn test_completion_focus_keys_after_clause_subblock() {
    // Cursor on the blank line AFTER `completion_reward = { ... }` must offer
    // focus-level keys (id, x, y, cost, …), not the effects inside the sub-block.
    // Regression: parser extends completion_reward's range past `}`, causing
    // descend() to enter the sub-block and return effect aliases.
    let text = "my_focus = {\n    completion_reward = {\n        add_political_power = 5\n    }\n    \n}\n";
    // Line 4, col 4: blank line after `}` of completion_reward, still inside focus.
    let labels = completion_labels("common/national_focus/test.txt", text, 4, 4);
    assert!(
        labels.iter().any(|l| l == "id"),
        "focus keys should be offered after a clause sub-block, got: {:?}",
        labels
    );
    assert!(
        labels.iter().any(|l| l == "cost"),
        "focus keys should be offered after a clause sub-block, got: {:?}",
        labels
    );
    assert!(
        !labels.iter().any(|l| l == "add_political_power"),
        "effect from sub-block must not leak into focus context, got: {:?}",
        labels
    );
}

#[test]
fn test_completion_focus_effects_inside_clause_subblock() {
    // Cursor on the blank line INSIDE `completion_reward = { }` must offer
    // effects, not focus-level keys.
    let text = "my_focus = {\n    completion_reward = {\n        \n    }\n}\n";
    // Line 2, col 8: blank line inside completion_reward.
    let labels = completion_labels("common/national_focus/test.txt", text, 2, 8);
    assert!(
        labels.iter().any(|l| l == "add_political_power"),
        "effects should be offered inside completion_reward, got: {:?}",
        labels
    );
    assert!(
        !labels.iter().any(|l| l == "id"),
        "focus key `id` must not appear inside completion_reward, got: {:?}",
        labels
    );
}

#[test]
fn test_completion_event_keys_after_clause_subblock() {
    // Cursor after `trigger = { ... }` inside an event block must offer event-level
    // keys (id, title, immediate, option, …), not trigger aliases.
    let text = "my_event = {\n    trigger = {\n        always = yes\n    }\n    \n}\n";
    // Line 4, col 4: blank line after `}` of trigger, still inside event.
    let labels = completion_labels("events/test.txt", text, 4, 4);
    assert!(
        labels.iter().any(|l| l == "title"),
        "event keys should be offered after a clause sub-block, got: {:?}",
        labels
    );
    assert!(
        labels.iter().any(|l| l == "immediate"),
        "event keys should be offered after a clause sub-block, got: {:?}",
        labels
    );
    assert!(
        !labels.iter().any(|l| l == "always"),
        "trigger alias must not leak into event context, got: {:?}",
        labels
    );
}

#[test]
fn test_completion_idea_names_for_has_idea() {
    // has_idea = | offers idea names collected via the idea_name complex enum.
    let ideas = (
        "common/ideas/test_ideas.txt",
        "ideas = {\n\tcountry = {\n\t\tmy_test_idea = {\n\t\t\tcost = 1\n\t\t}\n\t}\n}\n",
    );
    let text = "my_decision = {\n    allowed = {\n        has_idea = \n    }\n    cost = 5\n}\n";
    // Cursor right after `has_idea = ` (line 2, col 19).
    let labels = completion_labels_with_files("common/decisions/test.txt", text, &[ideas], 2, 19);
    assert!(
        labels.iter().any(|l| l == "my_test_idea"),
        "idea names should be offered for has_idea, got: {:?}",
        labels
    );
}

#[test]
fn test_completion_country_flags_for_has_country_flag() {
    // Flags written by set_country_flag anywhere in the workspace are offered
    // for has_country_flag = |.
    let setter = (
        "common/decisions/setter.txt",
        "other_decision = {\n    complete_effect = {\n        set_country_flag = my_war_flag\n    }\n    cost = 1\n}\n",
    );
    let text =
        "my_decision = {\n    allowed = {\n        has_country_flag = \n    }\n    cost = 5\n}\n";
    // Cursor right after `has_country_flag = ` (line 2, col 27).
    let labels = completion_labels_with_files("common/decisions/test.txt", text, &[setter], 2, 27);
    assert!(
        labels.iter().any(|l| l == "my_war_flag"),
        "collected country flags should be offered, got: {:?}",
        labels
    );
}

// ── #74/#75/#79: matched-but-empty value positions must not dump variables ────

#[test]
fn test_completion_focus_int_value_no_variable_dump() {
    // #75: at a focus `x = ` (typed `int`), completion must offer NOTHING rather
    // than dumping every saved variable. A variable is seeded in another file so
    // the old fallback would have surfaced it.
    let vars = (
        "common/decisions/vars.txt",
        "seed_dec = {\n    set_math = {\n        my_saved_var = 5\n    }\n}\n",
    );
    // Control: the seeded variable IS offered at a math value position, proving it
    // is indexed — so the absence below is the guard working, not an empty index.
    let control = completion_labels_custom_rules(
        COMPLETION_RULES,
        "common/decisions/test.txt",
        "d = {\n    set_math = {\n        foo = \n    }\n}\n",
        std::slice::from_ref(&vars),
        2,
        14,
    );
    assert!(
        control.iter().any(|l| l == "my_saved_var"),
        "control: seeded variable must be indexed and offered at a math value position, got: {:?}",
        control
    );
    // The fix: at focus `x = ` (int), no variable dump.
    let labels = completion_labels_custom_rules(
        COMPLETION_RULES,
        "common/national_focus/test.txt",
        "my_focus = {\n    x = \n}\n",
        &[vars],
        1,
        8,
    );
    assert!(
        !labels.iter().any(|l| l == "my_saved_var"),
        "focus int value must not dump saved variables, got: {:?}",
        labels
    );
}

const LOCALISATION_RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
    type[focus] = { path = "game/common/national_focus" }
}
decision = {
    set_math = {
        value_set[variable] = math_expr
        value_set[variable] = scalar
    }
    loc_name = localisation
}
focus = {
    id = scalar
}
"#;

#[test]
fn test_completion_localisation_value_offers_keys_not_variable_dump() {
    // #74: a `localisation`-typed value position must offer indexed loc keys,
    // not the flat variable dump.
    let vars = (
        "common/decisions/vars.txt",
        "seed_dec = {\n    set_math = {\n        my_saved_var = 5\n    }\n}\n",
    );
    let loc = (
        "localisation/test_l_english.yml",
        "l_english:\n MY_FOCUS:0 \"A focus\"\n",
    );
    let labels = completion_labels_custom_rules(
        LOCALISATION_RULES,
        "common/decisions/test.txt",
        "my_dec = {\n    loc_name = \n}\n",
        &[vars, loc],
        1,
        15,
    );
    assert!(
        labels.iter().any(|l| l == "my_focus"),
        "localisation value should offer indexed loc keys, got: {:?}",
        labels
    );
    assert!(
        !labels.iter().any(|l| l == "my_saved_var"),
        "localisation value must not dump saved variables, got: {:?}",
        labels
    );
}

const TWO_OVERLOAD_FLAG_RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
decision = {
    allowed = {
        alias_name[trigger] = alias_match_left[trigger]
    }
    complete_effect = {
        alias_name[effect] = alias_match_left[effect]
    }
    set_math = {
        value_set[variable] = math_expr
        value_set[variable] = scalar
    }
    cost = int
}
alias[trigger:has_country_flag] = value[country_flag]
alias[trigger:has_country_flag] = {
    flag = value[country_flag]
    ## cardinality = 0..1
    days = int
}
alias[effect:set_country_flag] = value_set[country_flag]
alias[effect:set_country_flag] = {
    flag = value_set[country_flag]
    ## cardinality = 0..1
    days = int
}
"#;

#[test]
fn test_completion_has_country_flag_two_overloads_no_dump() {
    // #79: with both a value and a block overload of has_country_flag /
    // set_country_flag declared, the value-form flag set must still resolve, and
    // an empty flag set must NOT fall back to the generic variable dump.
    let setter = (
        "common/decisions/setter.txt",
        "other = {\n    complete_effect = {\n        set_country_flag = my_war_flag\n    }\n    cost = 1\n}\n",
    );
    let flag_read =
        "my_decision = {\n    allowed = {\n        has_country_flag = \n    }\n    cost = 5\n}\n";

    // Flags still resolve past the two-overload interaction.
    let with_flag = completion_labels_custom_rules(
        TWO_OVERLOAD_FLAG_RULES,
        "common/decisions/test.txt",
        flag_read,
        std::slice::from_ref(&setter),
        2,
        27,
    );
    assert!(
        with_flag.iter().any(|l| l == "my_war_flag"),
        "two overloads: collected country flags must still resolve, got: {:?}",
        with_flag
    );

    // Empty flag set (no setter) + a seeded variable: the reader must offer
    // neither the (absent) flag nor the variable dump.
    let vars = (
        "common/decisions/vars.txt",
        "seed_dec = {\n    set_math = {\n        my_saved_var = 5\n    }\n}\n",
    );
    let empty_set = completion_labels_custom_rules(
        TWO_OVERLOAD_FLAG_RULES,
        "common/decisions/test.txt",
        flag_read,
        &[vars],
        2,
        27,
    );
    assert!(
        !empty_set.iter().any(|l| l == "my_saved_var"),
        "empty flag set must not dump saved variables, got: {:?}",
        empty_set
    );
}

// ── Issues #64, #65: type-pattern alias and alias_keys_field completions ──────

/// Spawn a server with custom `rules` text, open `extra_files` + the main file,
/// and return the completion labels at `(line0, char0)`.  Mirrors
/// `completion_labels_with_files` but the rules come from the caller.
fn completion_labels_custom_rules(
    rules: &str,
    rel_path: &str,
    text: &str,
    extra_files: &[(&str, &str)],
    line0: u32,
    char0: u32,
) -> Vec<String> {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("test_rules.cwt"), rules).unwrap();

    for (rel, content) in extra_files.iter().chain([&(rel_path, text)]) {
        let p = ws.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
    }

    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");

    for (rel, content) in extra_files.iter().chain([&(rel_path, text)]) {
        let uri = path_uri(ws.path().join(rel));
        let body = jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "hoi4",
                    "version": 1,
                    "text": content,
                }
            }),
        );
        write_frame(&mut child, &body).unwrap();
        wait_for_diagnostics(&mut reader, rel);
    }

    let doc_uri = path_uri(ws.path().join(rel_path));
    let body = jsonrpc_request(
        2,
        "textDocument/completion",
        serde_json::json!({
            "textDocument": { "uri": doc_uri },
            "position": { "line": line0, "character": char0 },
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let resp_str = read_response(&mut reader).expect("no completion response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(resp["id"], 2, "got: {}", resp_str);
    let items = resp["result"]
        .as_array()
        .cloned()
        .or_else(|| resp["result"]["items"].as_array().cloned())
        .unwrap_or_default();
    items
        .iter()
        .filter_map(|i| i["label"].as_str().map(|s| s.to_string()))
        .collect()
}

/// Like `completion_labels_custom_rules` but returns `(label, sortText)` pairs so
/// tests can assert scope-aware ranking, not just membership.
fn completion_items_custom_rules(
    rules: &str,
    rel_path: &str,
    text: &str,
    extra_files: &[(&str, &str)],
    line0: u32,
    char0: u32,
) -> Vec<(String, Option<String>)> {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("test_rules.cwt"), rules).unwrap();

    for (rel, content) in extra_files.iter().chain([&(rel_path, text)]) {
        let p = ws.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
    }

    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");

    for (rel, content) in extra_files.iter().chain([&(rel_path, text)]) {
        let uri = path_uri(ws.path().join(rel));
        let body = jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "hoi4",
                    "version": 1,
                    "text": content,
                }
            }),
        );
        write_frame(&mut child, &body).unwrap();
        wait_for_diagnostics(&mut reader, rel);
    }

    let doc_uri = path_uri(ws.path().join(rel_path));
    let body = jsonrpc_request(
        2,
        "textDocument/completion",
        serde_json::json!({
            "textDocument": { "uri": doc_uri },
            "position": { "line": line0, "character": char0 },
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let resp_str = read_response(&mut reader).expect("no completion response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(resp["id"], 2, "got: {}", resp_str);
    let items = resp["result"]
        .as_array()
        .cloned()
        .or_else(|| resp["result"]["items"].as_array().cloned())
        .unwrap_or_default();
    items
        .iter()
        .filter_map(|i| {
            i["label"]
                .as_str()
                .map(|s| (s.to_string(), i["sortText"].as_str().map(|t| t.to_string())))
        })
        .collect()
}

/// Rules mirroring the real HOI4 MIO shapes for the #76/#78 tests: a `mio:` scope
/// link (links.cwt), the MIO scope (scopes.cwt), a MIO-category and a country-only
/// modifier (modifiers.cwt + modifier_categories.cwt), and effect aliases split by
/// `## scope`. The `equipment_bonus` block `## push_scope`s into the MIO scope so
/// its modifier completions resolve against `military_industrial_organization`.
const MIO_SCOPE_RULES: &str = r#"
types = {
    type[military_industrial_organization] = {
        path = "game/common/military_industrial_organization/organizations"
    }
    type[scripted_effect] = {
        path = "game/common/scripted_effects"
    }
    type[decision] = {
        path = "game/common/decisions"
    }
    type[country_tag] = {
        path = "game/common/country_tags"
    }
}
links = {
    mio = {
        prefix = mio:
        output_scope = military_industrial_organization
        input_scopes = country
        from_data = yes
        data_source = <military_industrial_organization>
    }
    country_ref = {
        output_scope = country
        input_scopes = country
        from_data = yes
        data_source = <country_tag>
    }
}
scopes = {
    Country = {
        aliases = { country }
    }
    "Military Industrial Organizations" = {
        aliases = { military_industrial_organization }
    }
}
modifiers = {
    military_industrial_organization_funds_gain = military_industrial_organization
    war_support_factor = country
}
modifier_categories = {
    military_industrial_organization = {
        supported_scopes = { military_industrial_organization }
    }
    country = {
        supported_scopes = { country }
    }
}
decision = {
    complete_effect = {
        alias_name[effect] = alias_match_left[effect]
    }
}
military_industrial_organization = {
    name = scalar
    ## push_scope = military_industrial_organization
    equipment_bonus = {
        alias_name[modifier] = alias_match_left[modifier]
    }
}
scripted_effect = {
    alias_name[effect] = alias_match_left[effect]
}
alias[effect:scope_field] = { alias_name[effect] = alias_match_left[effect] }
### MIO-scope effect
## scope = military_industrial_organization
alias[effect:add_mio_funds] = int
### Country-only effect
## scope = country
alias[effect:add_political_power] = int
"#;

const MIO_INSTANCE: (&str, &str) = (
    "common/military_industrial_organization/organizations/orgs.txt",
    "MY_ORG = {\n    name = org\n}\n",
);

#[test]
fn test_completion_scope_link_keys_in_effect_block() {
    // #76: at a key position inside an effect block, the `mio:` scope-switch key
    // is offered per MIO instance (`mio:MY_ORG = { … }`). The scope_field alias
    // makes the block accept a scope switch; the key was never suggested before.
    let text = "my_dec = {\n    complete_effect = {\n        \n    }\n}\n";
    let country_tags = ("common/country_tags/tags.txt", "GER = {\n}\n");
    let labels = completion_labels_custom_rules(
        MIO_SCOPE_RULES,
        "common/decisions/d.txt",
        text,
        &[MIO_INSTANCE, country_tags],
        2,
        8,
    );
    assert!(
        labels.iter().any(|l| l == "mio:MY_ORG"),
        "scope-link key mio:MY_ORG must be offered in an effect block, got: {:?}",
        labels
    );
    // The prefix-less `<country_tag>` from-data link must NOT be flooded in as a
    // bare scope-switch key: a raw country tag is high-cardinality and rarely the
    // way a scope switch is completed (#76 wanted only the prefixed keys).
    assert!(
        !labels.iter().any(|l| l == "GER"),
        "bare country-tag scope-link key must not flood the list, got: {:?}",
        labels
    );
}

#[test]
fn test_completion_effects_scope_filtered_in_mio_block() {
    // #78 layer 1: inside `mio:MY_ORG = { … }` the current scope is
    // military_industrial_organization. A MIO-scope effect must appear and rank in
    // the top bucket; a country-only effect must NOT be dropped (scope tracking is
    // imperfect) but de-ranked into the bottom bucket, behind the matching one.
    let text = "my_dec = {\n    complete_effect = {\n        mio:MY_ORG = {\n            \n        }\n    }\n}\n";
    let items = completion_items_custom_rules(
        MIO_SCOPE_RULES,
        "common/decisions/d.txt",
        text,
        &[MIO_INSTANCE],
        3,
        12,
    );
    let labels: Vec<&str> = items.iter().map(|(l, _)| l.as_str()).collect();
    let mio_effect = items.iter().find(|(l, _)| l == "add_mio_funds");
    let country_effect = items.iter().find(|(l, _)| l == "add_political_power");
    assert!(
        mio_effect.is_some(),
        "MIO-scope effect must be offered inside a MIO block, got: {:?}",
        labels
    );
    let mio_sort = mio_effect.and_then(|(_, s)| s.clone());
    assert!(
        mio_sort.as_deref().is_some_and(|s| s.starts_with("0_")),
        "MIO-scope effect must rank in the top bucket, got sortText: {:?}",
        mio_sort
    );
    assert!(
        country_effect.is_some(),
        "country-only effect must NOT be dropped for scope mismatch, got: {:?}",
        labels
    );
    let country_sort = country_effect.and_then(|(_, s)| s.clone());
    assert!(
        country_sort.as_deref().is_some_and(|s| s.starts_with("z_")),
        "scope-mismatched effect must sink to the bottom bucket, got sortText: {:?}",
        country_sort
    );
    assert!(
        mio_sort < country_sort,
        "MIO-scope effect must sort ahead of the mismatched one, got {:?} vs {:?}",
        mio_sort,
        country_sort
    );
}

#[test]
fn test_completion_modifiers_scope_filtered_in_mio_block() {
    // #78 layer 2: inside a `## push_scope`d MIO `equipment_bonus` block, the
    // MIO-category modifier ranks top and the country-category one is de-ranked to
    // the bottom bucket (not dropped) — the modifier→category→supported_scopes
    // plumbing at work.
    let text = "my_org = {\n    name = org\n    equipment_bonus = {\n        \n    }\n}\n";
    let items = completion_items_custom_rules(
        MIO_SCOPE_RULES,
        "common/military_industrial_organization/organizations/test.txt",
        text,
        &[],
        3,
        8,
    );
    let labels: Vec<&str> = items.iter().map(|(l, _)| l.as_str()).collect();
    let mio_mod = items
        .iter()
        .find(|(l, _)| l == "military_industrial_organization_funds_gain");
    let country_mod = items.iter().find(|(l, _)| l == "war_support_factor");
    assert!(
        mio_mod.is_some(),
        "MIO-category modifier must be offered in a MIO-scope modifier block, got: {:?}",
        labels
    );
    let mio_sort = mio_mod.and_then(|(_, s)| s.clone());
    assert!(
        mio_sort.as_deref().is_some_and(|s| s.starts_with("0_")),
        "MIO-category modifier must rank in the top bucket, got sortText: {:?}",
        mio_sort
    );
    assert!(
        country_mod.is_some(),
        "country-category modifier must NOT be dropped for scope mismatch, got: {:?}",
        labels
    );
    let country_sort = country_mod.and_then(|(_, s)| s.clone());
    assert!(
        country_sort.as_deref().is_some_and(|s| s.starts_with("z_")),
        "scope-mismatched modifier must sink to the bottom bucket, got sortText: {:?}",
        country_sort
    );
    assert!(
        mio_sort < country_sort,
        "MIO-category modifier must sort ahead of the mismatched one, got {:?} vs {:?}",
        mio_sort,
        country_sort
    );
}

#[test]
fn test_completion_effects_unfiltered_when_scope_unknown() {
    // #78 regression guard: a scripted_effects file is scope-agnostic (SCOPE_ANY),
    // so no scope filtering applies — both the MIO-scope and country-only effects
    // must still be listed.
    let text = "my_se = {\n    \n}\n";
    let labels = completion_labels_custom_rules(
        MIO_SCOPE_RULES,
        "common/scripted_effects/se.txt",
        text,
        &[],
        1,
        4,
    );
    assert!(
        labels.iter().any(|l| l == "add_mio_funds"),
        "MIO-scope effect must still be listed when scope is unknown, got: {:?}",
        labels
    );
    assert!(
        labels.iter().any(|l| l == "add_political_power"),
        "country-only effect must still be listed when scope is unknown, got: {:?}",
        labels
    );
}

/// Rules for the #64 and #66 integration tests.
const SCRIPTED_EFFECT_RULES: &str = r#"
types = {
    type[scripted_effect] = {
        path = "game/common/scripted_effects"
    }
    type[decision] = {
        path = "game/common/decisions"
    }
}
decision = {
    complete_effect = {
        alias_name[effect] = alias_match_left[effect]
    }
}
alias[effect:<scripted_effect>] = yes
scripted_effect = {
    alias_name[effect] = alias_match_left[effect]
}
"#;

#[test]
fn test_completion_scripted_effects_in_effect_block() {
    // #64: `alias[effect:<scripted_effect>] = yes` means every scripted_effect
    // instance must appear as a KEYWORD completion inside effect blocks. The bug
    // was that the raw placeholder `<scripted_effect>` appeared instead of actual
    // instance names.
    let se_file = (
        "common/scripted_effects/my_effects.txt",
        "my_special_effect = {\n}\n",
    );
    // Blank line inside `complete_effect = { }` of the decision.
    let text = "my_dec = {\n    complete_effect = {\n        \n    }\n}\n";
    let labels = completion_labels_custom_rules(
        SCRIPTED_EFFECT_RULES,
        "common/decisions/d.txt",
        text,
        &[se_file],
        2,
        8,
    );
    assert!(
        labels.iter().any(|l| l == "my_special_effect"),
        "scripted_effect instances must be offered in effect blocks, got: {:?}",
        labels
    );
    assert!(
        !labels.iter().any(|l| l == "<scripted_effect>"),
        "raw placeholder must not appear in labels, got: {:?}",
        labels
    );
}

/// Rules for the #65 integration test: a `dynamic_modifier` type whose body uses
/// `alias_keys_field[modifier]` as the key pattern.
const DYNAMIC_MODIFIER_RULES: &str = r#"
types = {
    type[dynamic_modifier] = {
        path = "game/common/dynamic_modifiers"
    }
}
modifiers = {
    build_cost_ic = economy
    production_speed_factor = economy
}
dynamic_modifier = {
    ## cardinality = 0..inf
    alias_keys_field[modifier] = float
}
"#;

#[test]
fn test_completion_alias_keys_field_in_dynamic_modifier() {
    // #65: a block whose children use `alias_keys_field[modifier]` as their key
    // (common/dynamic_modifiers/*.txt in HOI4) must offer modifier names.
    let text = "my_dmod = {\n    \n}\n";
    let labels = completion_labels_custom_rules(
        DYNAMIC_MODIFIER_RULES,
        "common/dynamic_modifiers/test.txt",
        text,
        &[],
        1,
        4,
    );
    assert!(
        labels.iter().any(|l| l == "build_cost_ic"),
        "modifier names must be offered inside a dynamic_modifier block, got: {:?}",
        labels
    );
    assert!(
        labels.iter().any(|l| l == "production_speed_factor"),
        "all modifier keys must be offered, got: {:?}",
        labels
    );
}

// ── Backspace robustness: same-context completions must not evaporate when the
//    value is deleted, and the flat variable fallback must NOT be substituted
//    for the context-aware list. The cwt rules define the context; deleting
//    characters in the value doesn't change which block the cursor is in. ─────

/// Like `completion_labels_with_files` but issues a `didChange` to `new_text`
/// (full-sync) before requesting completion, so the test exercises the
/// backspace-into-blank case end to end. `wait_after_change` controls whether
/// the test waits for the debounced validate to republish diagnostics before
/// requesting completion — pass `true` to land on the post-debounce AST,
/// `false` to land on the stale AST (the realistic mid-typing snapshot).
fn completion_labels_after_change(
    rel_path: &str,
    open_text: &str,
    extra_files: &[(&str, &str)],
    new_text: &str,
    line0: u32,
    char0: u32,
    wait_after_change: bool,
) -> Vec<String> {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("test_rules.cwt"), DYNAMIC_RULES).unwrap();

    for (rel, content) in extra_files.iter().chain([&(rel_path, open_text)]) {
        let p = ws.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
    }

    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");

    for (rel, content) in extra_files.iter().chain([&(rel_path, open_text)]) {
        let uri = path_uri(ws.path().join(rel));
        let body = jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "hoi4",
                    "version": 1,
                    "text": content,
                }
            }),
        );
        write_frame(&mut child, &body).unwrap();
        wait_for_diagnostics(&mut reader, rel);
    }

    // didChange to the new (backspaced) text. Bump the version so the LSP
    // accepts it. Full-sync: server ignores the range and replaces the whole
    // document text.
    let doc_uri = path_uri(ws.path().join(rel_path));
    let body = jsonrpc_notification(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": { "uri": &doc_uri, "version": 2 },
            "contentChanges": [{ "text": new_text }],
        }),
    );
    write_frame(&mut child, &body).unwrap();
    if wait_after_change {
        wait_for_diagnostics(&mut reader, rel_path);
    }

    let body = jsonrpc_request(
        2,
        "textDocument/completion",
        serde_json::json!({
            "textDocument": { "uri": doc_uri },
            "position": { "line": line0, "character": char0 },
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let resp_str = read_response(&mut reader).expect("no completion response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(resp["id"], 2, "got: {}", resp_str);
    let items = resp["result"]
        .as_array()
        .cloned()
        .or_else(|| resp["result"]["items"].as_array().cloned())
        .unwrap_or_default();
    items
        .iter()
        .filter_map(|i| i["label"].as_str().map(|s| s.to_string()))
        .collect()
}

#[test]
fn test_completion_value_deleted_then_reoffered_keeps_context() {
    // User scenario: a decision's `allowed` block has `has_country_flag = my_war_flag`
    // with a working completion. They backspace the value, leaving
    // `has_country_flag = ` (or shorter). The same flag set must still be
    // offered — the block context (`allowed = { ... }`) hasn't changed, only
    // the value text has. The flat variable dump must NOT be substituted.
    let setter = (
        "common/decisions/setter.txt",
        "other_decision = {\n    complete_effect = {\n        set_country_flag = my_war_flag\n    }\n    cost = 1\n}\n",
    );
    let open_text = "my_decision = {\n    allowed = {\n        has_country_flag = my_war_flag\n    }\n    cost = 5\n}\n";
    // After backspacing the value, the cursor is right after `= ` on line 2.
    let new_text =
        "my_decision = {\n    allowed = {\n        has_country_flag = \n    }\n    cost = 5\n}\n";

    // Post-debounce (the AST has caught up to the new text): same flag set.
    let labels_post = completion_labels_after_change(
        "common/decisions/test.txt",
        open_text,
        std::slice::from_ref(&setter),
        new_text,
        2,
        27,
        true,
    );
    assert!(
        labels_post.iter().any(|l| l == "my_war_flag"),
        "post-debounce: backspaced value must still offer my_war_flag, got: {:?}",
        labels_post
    );

    // Mid-debounce (the AST is still the open_text one — the user is typing
    // fast and the next completion request arrives before the 250ms validate
    // has fired). Same expected behavior: the context-aware list, not the
    // generic variable dump.
    let labels_mid = completion_labels_after_change(
        "common/decisions/test.txt",
        open_text,
        &[setter],
        new_text,
        2,
        27,
        false,
    );
    assert!(
        labels_mid.iter().any(|l| l == "my_war_flag"),
        "mid-debounce: backspaced value must still offer my_war_flag, got: {:?}",
        labels_mid
    );
}

// ── Hover: localisation display ──────────────────────────────────────────────

/// Spawn a server with DYNAMIC_RULES, write `loc_files` (each: filename under
/// `localisation/`, full text including the `l_xxx:` header; a UTF-8 BOM is
/// prepended) and the one script file, run the workspace scan, then return
/// hovers at (line, character) before and after each live settings update.
/// `extra_init` is merged into the init options.
fn hover_markdowns_with_live_settings(
    loc_files: &[(&str, &str)],
    script_rel: &str,
    script_text: &str,
    line: u32,
    character: u32,
    extra_init: serde_json::Value,
    live_settings: &[(serde_json::Value, &str)],
) -> Vec<String> {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("test_rules.cwt"), DYNAMIC_RULES).unwrap();
    // Named scopes so the registry resolves `country` (HOI4 has no hardcoded
    // scope table); lets the hover surface the current scope context.
    std::fs::write(
        rules_dir.path().join("scopes.cwt"),
        "scopes = { country = { } state = { } }\n",
    )
    .unwrap();

    let loc_dir = ws.path().join("localisation");
    std::fs::create_dir_all(&loc_dir).unwrap();
    for (name, content) in loc_files {
        let mut bytes: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(content.as_bytes());
        std::fs::write(loc_dir.join(name), &bytes).unwrap();
    }

    let p = ws.path().join(script_rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, script_text).unwrap();

    // The workspace scan (which builds the loc index) early-returns when there
    // are no game files. Real mods always have some; drop a tiny one so the scan
    // runs even when the opened document is itself a .yml.
    let trigger = ws.path().join("common/_scan_trigger.txt");
    std::fs::create_dir_all(trigger.parent().unwrap()).unwrap();
    std::fs::write(&trigger, "# scan trigger\n").unwrap();

    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let mut init_opts = serde_json::json!({
        "language": "hoi4",
        "rulesCache": rules_dir.path().to_string_lossy(),
    });
    if let Some(obj) = extra_init.as_object() {
        for (k, v) in obj {
            init_opts[k] = v.clone();
        }
    }
    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": init_opts,
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");

    // initialized — triggers the background workspace scan that rebuilds loc_text
    let body = jsonrpc_notification("initialized", serde_json::json!({}));
    write_frame(&mut child, &body).unwrap();

    let doc_uri = path_uri(&p);
    let body = jsonrpc_notification(
        "textDocument/didOpen",
        serde_json::json!({
            "textDocument": {
                "uri": doc_uri,
                "languageId": "hoi4",
                "version": 1,
                "text": script_text,
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();

    // Poll hover until the initial loc index is populated, then after each
    // live-setting update until its distinctive text appears. The notification
    // has no response, so a subsequent request is the observable completion.
    let mut expected_values: Vec<(Option<&serde_json::Value>, &str)> = vec![(None, "Localisation")];
    expected_values.extend(
        live_settings
            .iter()
            .map(|(settings, expected)| (Some(settings), *expected)),
    );
    let mut hover_values = Vec::with_capacity(expected_values.len());
    let mut request_id = 2;
    for (settings, expected) in expected_values {
        if let Some(settings) = settings {
            write_frame(
                &mut child,
                &jsonrpc_notification(
                    "workspace/didChangeConfiguration",
                    serde_json::json!({ "settings": settings }),
                ),
            )
            .unwrap();
        }
        let mut hover_value = String::new();
        for _ in 0..120 {
            let hover_req = jsonrpc_request(
                request_id,
                "textDocument/hover",
                serde_json::json!({
                    "textDocument": { "uri": doc_uri },
                    "position": { "line": line, "character": character },
                }),
            );
            request_id += 1;
            write_frame(&mut child, &hover_req).unwrap();
            let resp_str = read_response(&mut reader).expect("no hover response");
            let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
            hover_value = resp["result"]["contents"]["value"]
                .as_str()
                .unwrap_or("")
                .to_string();
            if hover_value.contains(expected) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        hover_values.push(hover_value);
    }
    child.kill().ok();
    hover_values
}

fn hover_markdown(
    loc_files: &[(&str, &str)],
    script_rel: &str,
    script_text: &str,
    line: u32,
    character: u32,
    extra_init: serde_json::Value,
) -> String {
    hover_markdowns_with_live_settings(
        loc_files,
        script_rel,
        script_text,
        line,
        character,
        extra_init,
        &[],
    )
    .into_iter()
    .next()
    .unwrap_or_default()
}

#[test]
fn test_hover_shows_current_scope() {
    // Anything hovered inside a scoped block shows the current scope context,
    // independent of whether the rule declares a required scope. The decisions
    // file is country-scoped, so a trigger value there reads as `country`.
    let hover = hover_markdown(
        &[("test_l_english.yml", "l_english:\n my_focus:0 \"Focus\"\n")],
        "common/decisions/d.txt",
        "my_dec = {\n    allowed = {\n        has_completed_focus = my_focus\n    }\n}\n",
        2,
        32,
        serde_json::json!({}),
    );
    assert!(
        hover.contains("**Scope**: country"),
        "hover should surface the current scope, got: {hover}"
    );
}

#[test]
fn test_hover_nested_loc_key_in_yml() {
    // Hovering a `$MY_KEY$` reference inside a .yml loc value resolves to the
    // referenced loc entry's text (nested loc keys / dynamic bindings).
    let hover = hover_markdown(
        &[("test_l_english.yml", "l_english:\n MY_KEY:0 \"My Value\"\n")],
        "localisation/english/ref_l_english.yml",
        "\u{FEFF}l_english:\n OTHER:0 \"see $MY_KEY$\"\n",
        1,
        17,
        serde_json::json!({}),
    );
    assert!(
        hover.contains("My Value"),
        "hover on $MY_KEY$ should resolve to the loc entry text, got: {hover}"
    );
}

#[test]
fn test_hover_shows_localisation() {
    // A reference: the loc key appears as a leaf value (`name = my_idea`).
    let hover = hover_markdown(
        &[(
            "test_l_english.yml",
            "l_english:\n my_idea:0 \"My Awesome Idea\"\n",
        )],
        "common/countries/test.txt",
        "my_country = {\n    name = my_idea\n}\n",
        1,
        14,
        serde_json::json!({}),
    );
    assert!(
        hover.contains("Localisation"),
        "hover should include localisation section, got: {hover}"
    );
    assert!(
        hover.contains("My Awesome Idea"),
        "hover should include loc text, got: {hover}"
    );
    assert!(
        hover.contains("English"),
        "hover should include language label, got: {hover}"
    );
}

#[test]
fn test_loc_edit_updates_hover_without_a_rescan() {
    // #53 / #292: loc_text is patched on a loc didChange. A revert that only
    // rebuilt it on the workspace scan would keep serving the old hover.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("test_rules.cwt"), DYNAMIC_RULES).unwrap();
    std::fs::write(
        rules_dir.path().join("scopes.cwt"),
        "scopes = { country = { } state = { } }\n",
    )
    .unwrap();

    let loc_dir = ws.path().join("localisation");
    std::fs::create_dir_all(&loc_dir).unwrap();
    let loc_rel = "localisation/test_l_english.yml";
    let loc_old = "l_english:\n my_idea:0 \"Old Name\"\n other_idea:0 \"Other Text\"\n";
    let loc_new = "l_english:\n my_idea:0 \"New Name\"\n other_idea:0 \"Other Text\"\n";
    let mut loc_bytes: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
    loc_bytes.extend_from_slice(loc_old.as_bytes());
    std::fs::write(loc_dir.join("test_l_english.yml"), &loc_bytes).unwrap();

    let script_rel = "common/countries/test.txt";
    let script_text = "my_country = {\n    name = my_idea\n    title = other_idea\n}\n";
    let script_path = ws.path().join(script_rel);
    std::fs::create_dir_all(script_path.parent().unwrap()).unwrap();
    std::fs::write(&script_path, script_text).unwrap();

    let trigger = ws.path().join("common/_scan_trigger.txt");
    std::fs::write(&trigger, "# scan trigger\n").unwrap();

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(ws.path()),
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    let script_uri = path_uri(&script_path);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": script_uri,
                    "languageId": "hoi4",
                    "version": 1,
                    "text": script_text,
                }
            }),
        ),
    )
    .unwrap();
    wait_for_diagnostics(&mut reader, script_rel);

    let hover_at = |child: &mut std::process::Child,
                    reader: &mut BufReader<std::process::ChildStdout>,
                    line: u32,
                    character: u32,
                    expect: &str,
                    id0: i64|
     -> String {
        let mut last = String::new();
        for attempt in 0..120 {
            write_frame(
                child,
                &jsonrpc_request(
                    id0 + attempt,
                    "textDocument/hover",
                    serde_json::json!({
                        "textDocument": { "uri": script_uri },
                        "position": { "line": line, "character": character },
                    }),
                ),
            )
            .unwrap();
            let resp: serde_json::Value =
                serde_json::from_str(&read_response(reader).expect("no hover response")).unwrap();
            last = resp["result"]["contents"]["value"]
                .as_str()
                .unwrap_or("")
                .to_string();
            if last.contains(expect) {
                return last;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        last
    };

    let before = hover_at(&mut child, &mut reader, 1, 14, "Old Name", 100);
    assert!(
        before.contains("Old Name"),
        "scan-built loc_text must show the on-disk loc, got: {before}"
    );

    let loc_uri = path_uri(ws.path().join(loc_rel));
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": loc_uri,
                    "languageId": "hoi4",
                    "version": 1,
                    "text": loc_old,
                }
            }),
        ),
    )
    .unwrap();
    wait_for_diagnostics(&mut reader, loc_rel);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": loc_uri, "version": 2 },
                "contentChanges": [{ "text": loc_new }],
            }),
        ),
    )
    .unwrap();
    let _ = diags_for(&mut reader, loc_rel, 1);

    let after = hover_at(&mut child, &mut reader, 1, 14, "New Name", 300);
    assert!(
        after.contains("New Name"),
        "a loc didChange must patch hover without a rescan, got: {after}"
    );
    assert!(
        !after.contains("Old Name"),
        "the pre-edit loc text must not linger after the patch, got: {after}"
    );

    let neighbour = hover_at(&mut child, &mut reader, 2, 16, "Other Text", 500);
    child.kill().ok();
    assert!(
        neighbour.contains("Other Text"),
        "editing one key must leave an untouched key's hover alone, got: {neighbour}"
    );
}

#[test]
fn test_hover_idea_definition_shows_name_and_desc() {
    // A definition key: the idea token IS the loc key, with `<key>_desc` for the
    // description. Hover the key itself (not a value reference).
    let hover = hover_markdown(
        &[(
            "test_l_english.yml",
            "l_english:\n my_great_idea:0 \"Great Idea\"\n my_great_idea_desc:0 \"It is great.\"\n",
        )],
        "common/ideas/test.txt",
        "my_great_idea = {\n    cost = 5\n}\n",
        0,
        3,
        serde_json::json!({}),
    );
    assert!(
        hover.contains("Great Idea"),
        "hover on an idea key should show its name loc, got: {hover}"
    );
    assert!(
        hover.contains("It is great."),
        "hover on an idea key should show its _desc loc, got: {hover}"
    );
}

#[test]
fn test_hover_value_field_previews_localisation() {
    // #317: a leaf whose right-hand is `value[...]` (e.g. a script variable
    // reference like `has_country_flag = "my_war_flag"`) must preview the
    // localisation the value resolves to, the same way a `name = my_idea`
    // (LocalisationField) reference does. The right-hand classification is
    // `VariableGetField("country_flag")` here, not the scalar/localisation path.
    let hover = hover_markdown(
        &[(
            "test_l_english.yml",
            "l_english:\n my_war_flag:0 \"War Flag\"\n",
        )],
        "common/decisions/test.txt",
        "my_dec = {\n    allowed = { has_country_flag = \"my_war_flag\" }\n    cost = 1\n}\n",
        1,
        36,
        serde_json::json!({}),
    );
    assert!(
        hover.contains("War Flag"),
        "hover on a value[...] flag must preview the loc key, got: {hover}"
    );
}

#[test]
fn test_hover_default_hides_other_languages() {
    // Default (hoverShowAllLanguages off): only the primary language is shown.
    let hover = hover_markdown(
        &[
            (
                "test_l_english.yml",
                "l_english:\n my_idea:0 \"English Name\"\n",
            ),
            (
                "test_l_french.yml",
                "l_french:\n my_idea:0 \"Nom Francais\"\n",
            ),
        ],
        "common/countries/test.txt",
        "my_country = {\n    name = my_idea\n}\n",
        1,
        14,
        serde_json::json!({}),
    );
    assert!(
        hover.contains("English Name"),
        "hover should show the primary (English) loc, got: {hover}"
    );
    assert!(
        !hover.contains("Nom Francais"),
        "hover should not show other languages by default, got: {hover}"
    );
}

#[test]
fn test_hover_show_all_languages_flag() {
    // With hoverShowAllLanguages on, every collected language is shown.
    let hover = hover_markdown(
        &[
            (
                "test_l_english.yml",
                "l_english:\n my_idea:0 \"English Name\"\n",
            ),
            (
                "test_l_french.yml",
                "l_french:\n my_idea:0 \"Nom Francais\"\n",
            ),
        ],
        "common/countries/test.txt",
        "my_country = {\n    name = my_idea\n}\n",
        1,
        14,
        serde_json::json!({ "hoverShowAllLanguages": true }),
    );
    assert!(
        hover.contains("English Name"),
        "hover should show English loc, got: {hover}"
    );
    assert!(
        hover.contains("Nom Francais"),
        "hover with the flag on should show French loc too, got: {hover}"
    );
}

#[test]
fn test_live_configuration_rebuilds_localisation_hover() {
    let hovers = hover_markdowns_with_live_settings(
        &[
            (
                "test_l_english.yml",
                "l_english:\n my_idea:0 \"English Name\"\n",
            ),
            (
                "test_l_french.yml",
                "l_french:\n my_idea:0 \"Nom Francais\"\n",
            ),
        ],
        "common/countries/test.txt",
        "my_country = {\n    name = my_idea\n}\n",
        1,
        14,
        serde_json::json!({ "localisationLanguages": ["English"] }),
        &[
            (
                serde_json::json!({
                    "localisationLanguages": ["French"],
                    "hoverShowAllLanguages": false,
                }),
                "Nom Francais",
            ),
            (
                serde_json::json!({
                    "localisationLanguages": ["French"],
                    "hoverShowAllLanguages": true,
                }),
                "English Name",
            ),
        ],
    );
    assert!(
        hovers[0].contains("English Name") && !hovers[0].contains("Nom Francais"),
        "initial hover should use English only, got: {:?}",
        hovers[0]
    );
    assert!(
        hovers[1].contains("Nom Francais") && !hovers[1].contains("English Name"),
        "changing languages should rebuild the hover map, got: {:?}",
        hovers[1]
    );
    assert!(
        hovers[2].contains("English Name") && hovers[2].contains("Nom Francais"),
        "enabling all-language hovers should rebuild the hover map, got: {:?}",
        hovers[2]
    );
}

// ── Go-to-definition ─────────────────────────────────────────────────────────

/// Rules exercising every navigable reference kind goto must resolve.
const GOTO_RULES: &str = r#"
types = {
    type[focus] = { path = "game/common/national_focus" }
    type[oob] = { path = "game/history/units" }
    type[character] = { path = "game/common/characters" }
    type[special_project] = { path = "game/common/special_projects" }
    type[scripted_effect] = { path = "game/common/scripted_effects" }
    type[decision] = { path = "game/common/decisions" }
    type[on_action] = { path = "game/common/on_actions" }
    ## type_key_filter = on_weekly
    type[on_weekly] = {
        path = "game/common/on_actions"
        skip_root_key = on_actions
    }
}
links = {
    sp = {
        prefix = sp:
        output_scope = special_project
        input_scopes = country
        from_data = yes
        data_source = <special_project>
    }
    character = {
        output_scope = character
        input_scopes = country
        from_data = yes
        data_source = <character>
    }
}
decision = {
    ## cardinality = 0..1
    has_focus = <focus>
    ## cardinality = 0..1
    load_oob = <oob>
    ## cardinality = 0..1
    localization_key = localisation
    ## cardinality = 0..1
    complete_special_project = scope[special_project]
    ## cardinality = 0..1
    terrain = enum[terrain]
    ## cardinality = 0..1
    available = {
        alias_name[trigger] = alias_match_left[trigger]
    }
    ## cardinality = 0..1
    complete_effect = {
        alias_name[effect] = alias_match_left[effect]
    }
    ## cardinality = 0..inf
    <character> = {
        is_enabled = bool
    }
}
alias[trigger:always] = bool
alias[effect:<scripted_effect>] = yes
focus = { x = bool }
oob = { y = bool }
character = { name = scalar }
special_project = { z = bool }
scripted_effect = { alias_name[effect] = alias_match_left[effect] }
on_action = {
    ## cardinality = 0..inf
    on_weekly = single_alias_right[country_event_effect]
}
single_alias[country_event_effect] = {
    ## cardinality = 0..inf
    effect = {
        alias_name[effect] = alias_match_left[effect]
    }
}
enums = {
    enum[terrain] = {
        plains
        forest
    }
}
"#;

/// Spawn a server with `rules`, write the loc `.yml` files under `localisation/`
/// and the script files, then resolve textDocument/definition at (line, char) on
/// `doc_rel`. Polls until a non-empty result arrives (the loc index and type
/// index land via the async workspace scan). Returns `(uri, start_line)` pairs.
fn goto_def(
    rules: &str,
    loc_files: &[(&str, &str)],
    files: &[(&str, &str)],
    doc_rel: &str,
    line0: u32,
    char0: u32,
) -> Vec<(String, u32)> {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("test_rules.cwt"), rules).unwrap();

    let loc_dir = ws.path().join("localisation");
    std::fs::create_dir_all(&loc_dir).unwrap();
    for (name, content) in loc_files {
        let mut bytes: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(content.as_bytes());
        std::fs::write(loc_dir.join(name), &bytes).unwrap();
    }

    for (rel, content) in files {
        let p = ws.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
    }

    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");

    let body = jsonrpc_notification("initialized", serde_json::json!({}));
    write_frame(&mut child, &body).unwrap();

    for (rel, content) in files {
        let uri = path_uri(ws.path().join(rel));
        let body = jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": uri, "languageId": "hoi4", "version": 1, "text": content,
                }
            }),
        );
        write_frame(&mut child, &body).unwrap();
        wait_for_diagnostics(&mut reader, rel);
    }

    let doc_uri = path_uri(ws.path().join(doc_rel));
    let mut out: Vec<(String, u32)> = Vec::new();
    // Loc-key goto depends on the async workspace scan populating loc_locations;
    // under parallel test load that can lag far beyond the fast no-load case, so
    // poll very generously.
    for attempt in 0..200 {
        let req = jsonrpc_request(
            100 + attempt,
            "textDocument/definition",
            serde_json::json!({
                "textDocument": { "uri": doc_uri },
                "position": { "line": line0, "character": char0 },
            }),
        );
        write_frame(&mut child, &req).unwrap();
        let resp_str = read_response(&mut reader).expect("no definition response");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        let arr = resp["result"]
            .as_array()
            .cloned()
            .or_else(|| {
                resp["result"]
                    .as_object()
                    .map(|o| vec![serde_json::Value::Object(o.clone())])
            })
            .unwrap_or_default();
        out = arr
            .iter()
            .filter_map(|l| {
                let uri = l["uri"].as_str()?.to_string();
                let line = l["range"]["start"]["line"].as_u64()? as u32;
                Some((uri, line))
            })
            .collect();
        if !out.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    child.kill().ok();
    out
}

#[test]
fn test_goto_focus_value() {
    // has_focus = MY_FOCUS — goto on the value jumps to the focus definition.
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        (
            "common/decisions/d.txt",
            "my_dec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
    ];
    // Cursor on MY_FOCUS (line 1, col ~16).
    let locs = goto_def(GOTO_RULES, &[], files, "common/decisions/d.txt", 1, 16);
    assert!(
        locs.iter()
            .any(|(u, _)| u.ends_with("national_focus/f.txt")),
        "goto should resolve focus def, got: {:?}",
        locs
    );
}

#[test]
fn test_goto_quoted_oob_value() {
    // load_oob = "MY_OOB" — the quoted value must be unquoted before the index
    // lookup, else nothing resolves.
    let files = &[
        ("history/units/o.txt", "MY_OOB = { y = yes }\n"),
        (
            "common/decisions/d.txt",
            "my_dec = {\n    load_oob = \"MY_OOB\"\n}\n",
        ),
    ];
    // Cursor inside the quoted value (line 1, col ~17).
    let locs = goto_def(GOTO_RULES, &[], files, "common/decisions/d.txt", 1, 17);
    assert!(
        locs.iter().any(|(u, _)| u.ends_with("units/o.txt")),
        "goto should resolve quoted oob def, got: {:?}",
        locs
    );
}

#[test]
fn test_goto_enum_value_lands_on_the_config_member() {
    // terrain = forest — an enum member has no definition in the game files,
    // so goto jumps to the member inside `enum[terrain]` in the rules folder.
    let files = &[(
        "common/decisions/d.txt",
        "my_dec = {\n    terrain = forest\n}\n",
    )];
    // Cursor on `forest` (line 1, col 15).
    let locs = goto_def(GOTO_RULES, &[], files, "common/decisions/d.txt", 1, 15);
    let member_line = GOTO_RULES
        .lines()
        .position(|l| l.trim() == "forest")
        .expect("the enum member") as u32;
    assert!(
        locs.iter()
            .any(|(u, l)| u.ends_with("test_rules.cwt") && *l == member_line),
        "goto should land on the enum member at line {}, got: {:?}",
        member_line,
        locs
    );
}

#[test]
fn test_goto_nested_loc_key_in_yml() {
    // Goto on a `$MY_KEY$` reference inside a .yml jumps to the loc entry it
    // names. A game file is present so the workspace scan (which builds
    // loc_locations) runs.
    let loc = &[("def_l_english.yml", "l_english:\n MY_KEY:0 \"My Value\"\n")];
    let files = &[
        ("common/scan_trigger.txt", "# trigger\n"),
        (
            "localisation/english/use_l_english.yml",
            "\u{FEFF}l_english:\n OTHER:0 \"$MY_KEY$\"\n",
        ),
    ];
    // Cursor inside `$MY_KEY$` on line 1 (col 12).
    let locs = goto_def(
        GOTO_RULES,
        loc,
        files,
        "localisation/english/use_l_english.yml",
        1,
        12,
    );
    assert!(
        locs.iter().any(|(u, _)| u.ends_with("def_l_english.yml")),
        "goto on $MY_KEY$ should resolve to its loc definition, got: {:?}",
        locs
    );
}

#[test]
fn test_goto_character_key() {
    // MY_CHAR = { ... } used as a <character> key — the reference is on the key,
    // which only resolves with the key-side classifier.
    let files = &[
        ("common/characters/c.txt", "MY_CHAR = { name = bob }\n"),
        (
            "common/decisions/d.txt",
            "my_dec = {\n    MY_CHAR = { is_enabled = yes }\n}\n",
        ),
    ];
    // Cursor on the MY_CHAR key (line 1, col 6).
    let locs = goto_def(GOTO_RULES, &[], files, "common/decisions/d.txt", 1, 6);
    assert!(
        locs.iter().any(|(u, _)| u.ends_with("characters/c.txt")),
        "goto should resolve character key def, got: {:?}",
        locs
    );
}

#[test]
fn test_goto_localisation_key() {
    // localization_key = MY_KEY — goto jumps to the .yml entry.
    let loc = &[("test_l_english.yml", "l_english:\n MY_KEY:0 \"Text\"\n")];
    let files = &[(
        "common/decisions/d.txt",
        "my_dec = {\n    localization_key = MY_KEY\n}\n",
    )];
    // Cursor on MY_KEY (line 1, col ~25).
    let locs = goto_def(GOTO_RULES, loc, files, "common/decisions/d.txt", 1, 25);
    assert!(
        locs.iter().any(|(u, _)| u.ends_with("test_l_english.yml")),
        "goto should resolve loc key to the yml, got: {:?}",
        locs
    );
}

#[test]
fn test_goto_special_project_sp_prefix() {
    // complete_special_project = sp:MY_PROJ — the sp: prefix resolves through the
    // matching link's data_source <special_project>.
    let files = &[
        ("common/special_projects/p.txt", "MY_PROJ = { z = yes }\n"),
        (
            "common/decisions/d.txt",
            "my_dec = {\n    complete_special_project = sp:MY_PROJ\n}\n",
        ),
    ];
    // Cursor inside the value after the sp: prefix (line 1, col ~34).
    let locs = goto_def(GOTO_RULES, &[], files, "common/decisions/d.txt", 1, 34);
    assert!(
        locs.iter()
            .any(|(u, _)| u.ends_with("special_projects/p.txt")),
        "goto should resolve sp: special_project, got: {:?}",
        locs
    );
}

#[test]
fn test_goto_loc_key_prefers_english() {
    // The key exists in both English and Brazilian Portuguese; goto must land on
    // the English (primary) entry, not whichever was scanned first.
    let loc = &[
        ("test_l_braz_por.yml", "l_braz_por:\n MY_KEY:0 \"Texto\"\n"),
        ("test_l_english.yml", "l_english:\n MY_KEY:0 \"Text\"\n"),
    ];
    let files = &[(
        "common/decisions/d.txt",
        "my_dec = {\n    localization_key = MY_KEY\n}\n",
    )];
    let locs = goto_def(GOTO_RULES, loc, files, "common/decisions/d.txt", 1, 25);
    assert!(
        locs.iter().any(|(u, _)| u.ends_with("test_l_english.yml")),
        "goto should prefer the English loc file, got: {:?}",
        locs
    );
    assert!(
        !locs.iter().any(|(u, _)| u.ends_with("braz_por.yml")),
        "goto must not land on braz_por, got: {:?}",
        locs
    );
}

#[test]
fn test_goto_character_scope_link_key() {
    // A character used as a scope key inside a trigger block matches no rule
    // (value_rules is empty); it resolves via the `character` link's data_source
    // <character>. This is the real MD case the rule-based path missed.
    let files = &[
        ("common/characters/c.txt", "MY_CHAR = { name = bob }\n"),
        (
            "common/decisions/d.txt",
            "my_dec = {\n    available = {\n        MY_CHAR = { always = yes }\n    }\n}\n",
        ),
    ];
    // Cursor on the MY_CHAR key (line 2, col 8).
    let locs = goto_def(GOTO_RULES, &[], files, "common/decisions/d.txt", 2, 8);
    assert!(
        locs.iter().any(|(u, _)| u.ends_with("characters/c.txt")),
        "goto should resolve scope-link character key, got: {:?}",
        locs
    );
}

#[test]
fn test_goto_scripted_effect_call() {
    // A scripted_effect call (`my_se = yes`) resolves through the
    // `alias[effect:<scripted_effect>]` rule whose left field names the type.
    let files = &[
        (
            "common/scripted_effects/e.txt",
            "my_se = { log = \"hi\" }\n",
        ),
        (
            "common/decisions/d.txt",
            "my_dec = {\n    complete_effect = {\n        my_se = yes\n    }\n}\n",
        ),
    ];
    // Cursor on the my_se call key (line 2, col 8).
    let locs = goto_def(GOTO_RULES, &[], files, "common/decisions/d.txt", 2, 8);
    assert!(
        locs.iter()
            .any(|(u, _)| u.ends_with("scripted_effects/e.txt")),
        "goto should resolve scripted_effect call, got: {:?}",
        locs
    );
}

#[test]
fn test_goto_scripted_effect_in_on_actions() {
    // A `*_on_actions`-style scripted_effect call inside an on_actions effect
    // block. The call site sits behind skip_root_key=on_actions + an inlined
    // single_alias_right effect block — a far deeper path than the decision case.
    let files = &[
        (
            "common/scripted_effects/e.txt",
            "my_se = { log = \"hi\" }\n",
        ),
        (
            "common/on_actions/x.txt",
            "on_actions = {\n    on_weekly = {\n        effect = {\n            my_se = yes\n        }\n    }\n}\n",
        ),
    ];
    // Cursor on the my_se call key (line 3, col 12).
    let locs = goto_def(GOTO_RULES, &[], files, "common/on_actions/x.txt", 3, 12);
    assert!(
        locs.iter()
            .any(|(u, _)| u.ends_with("scripted_effects/e.txt")),
        "goto should resolve scripted_effect call inside on_actions, got: {:?}",
        locs
    );
}

#[test]
fn test_goto_scripted_effect_in_scripted_effect_body() {
    // A scripted_effect call nested inside another scripted_effect's body
    // (common/scripted_effects/), not behind a decision/event effect block.
    let files = &[
        (
            "common/scripted_effects/e.txt",
            "my_se = { log = \"hi\" }\n",
        ),
        (
            "common/scripted_effects/caller.txt",
            "my_caller = {\n    my_se = yes\n}\n",
        ),
    ];
    // Cursor on the my_se call key (line 1, col 4).
    let locs = goto_def(
        GOTO_RULES,
        &[],
        files,
        "common/scripted_effects/caller.txt",
        1,
        4,
    );
    assert!(
        locs.iter()
            .any(|(u, _)| u.ends_with("scripted_effects/e.txt")),
        "goto should resolve scripted_effect call inside a scripted_effect body, got: {:?}",
        locs
    );
}

#[test]
fn test_goto_vanilla_definition_resolves_to_vanilla_file() {
    // Issue #62: goto-definition on a reference to a base-game (vanilla)
    // definition must land in the real vanilla file, not fall back to a bogus
    // line in whatever document the user has open. Before the fix, vanilla
    // instances were merged under the "<vanilla-cache>" sentinel, which failed
    // to parse as a URI and resolved to the request document.
    let ws = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("test_rules.cwt"), GOTO_RULES).unwrap();

    // A base-game focus defined only in the vanilla install.
    let vfocus = vanilla.path().join("common/national_focus/base.txt");
    std::fs::create_dir_all(vfocus.parent().unwrap()).unwrap();
    std::fs::write(&vfocus, "VANILLA_FOCUS = { x = yes }\n").unwrap();

    // A mod decision that references it.
    let decision_rel = "common/decisions/d.txt";
    let decision = ws.path().join(decision_rel);
    std::fs::create_dir_all(decision.parent().unwrap()).unwrap();
    let decision_text = "my_dec = {\n    has_focus = VANILLA_FOCUS\n}\n";
    std::fs::write(&decision, decision_text).unwrap();

    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "vanilla": vanilla.path().to_string_lossy(),
                "cacheDir": cache.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();

    let doc_uri = path_uri(&decision);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": doc_uri, "languageId": "hoi4", "version": 1, "text": decision_text,
                }
            }),
        ),
    )
    .unwrap();
    wait_for_diagnostics(&mut reader, decision_rel);

    // Cursor on VANILLA_FOCUS (line 1, col 16). Poll: the vanilla index lands
    // via the async workspace scan, so goto is empty until the merge completes.
    let mut out: Vec<(String, u32)> = Vec::new();
    for attempt in 0..50 {
        let req = jsonrpc_request(
            100 + attempt,
            "textDocument/definition",
            serde_json::json!({
                "textDocument": { "uri": doc_uri },
                "position": { "line": 1, "character": 16 },
            }),
        );
        write_frame(&mut child, &req).unwrap();
        let resp_str = read_response(&mut reader).expect("no definition response");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        let arr = resp["result"]
            .as_array()
            .cloned()
            .or_else(|| {
                resp["result"]
                    .as_object()
                    .map(|o| vec![serde_json::Value::Object(o.clone())])
            })
            .unwrap_or_default();
        out = arr
            .iter()
            .filter_map(|l| {
                Some((
                    l["uri"].as_str()?.to_string(),
                    l["range"]["start"]["line"].as_u64()? as u32,
                ))
            })
            .collect();
        if !out.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    child.kill().ok();

    assert!(
        out.iter()
            .any(|(u, _)| u.ends_with("national_focus/base.txt")),
        "goto should resolve to the vanilla focus file, got: {:?}",
        out
    );
    assert!(
        !out.iter().any(|(u, _)| u.ends_with("decisions/d.txt")),
        "goto must NOT fall back to the request document (the #62 bug), got: {:?}",
        out
    );
}

#[test]
fn test_vanilla_loc_is_read_once_and_the_mod_wins_a_shared_key() {
    // #89: the base game's loc is read on the first scan and kept for the rest
    // of the session — a rescan walks the workspace only. Two things follow,
    // and both are asserted here: a key the mod redefines resolves to the mod's
    // file, and a base-game-only key still resolves after a re-index even
    // though the install's loc is no longer on disk (nothing re-read it).
    let ws = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("test_rules.cwt"), GOTO_RULES).unwrap();

    let vanilla_loc = vanilla.path().join("localisation/english");
    std::fs::create_dir_all(&vanilla_loc).unwrap();
    std::fs::write(
        vanilla_loc.join("base_l_english.yml"),
        "\u{FEFF}l_english:\n SHARED_KEY:0 \"Base text\"\n BASE_ONLY_KEY:0 \"Base only\"\n",
    )
    .unwrap();

    let ws_loc = ws.path().join("localisation/english");
    std::fs::create_dir_all(&ws_loc).unwrap();
    std::fs::write(
        ws_loc.join("def_l_english.yml"),
        "\u{FEFF}l_english:\n SHARED_KEY:0 \"Mod text\"\n",
    )
    .unwrap();
    let use_rel = "localisation/english/use_l_english.yml";
    let use_path = ws.path().join(use_rel);
    let use_text =
        "\u{FEFF}l_english:\n A:0 \"$SHARED_KEY$\"\n B:0 \"$BASE_ONLY_KEY$\"\n".to_string();
    std::fs::write(&use_path, &use_text).unwrap();
    let trigger = ws.path().join("common/scan_trigger.txt");
    std::fs::create_dir_all(trigger.parent().unwrap()).unwrap();
    std::fs::write(&trigger, "# trigger\n").unwrap();

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(ws.path()),
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "vanilla": vanilla.path().to_string_lossy(),
                    "cacheDir": cache.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();

    let doc_uri = path_uri(&use_path);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": doc_uri, "languageId": "hoi4", "version": 1, "text": use_text,
                }
            }),
        ),
    )
    .unwrap();
    wait_for_diagnostics(&mut reader, use_rel);

    // `$SHARED_KEY$` on line 1 col 10, `$BASE_ONLY_KEY$` on line 2 col 10.
    let goto = |child: &mut std::process::Child,
                reader: &mut BufReader<std::process::ChildStdout>,
                line: u32,
                id_base: i64|
     -> Vec<String> {
        // loc_locations lands via the async scan, so poll until it answers.
        for attempt in 0..50 {
            write_frame(
                child,
                &jsonrpc_request(
                    id_base + attempt,
                    "textDocument/definition",
                    serde_json::json!({
                        "textDocument": { "uri": doc_uri },
                        "position": { "line": line, "character": 10 },
                    }),
                ),
            )
            .unwrap();
            let resp: serde_json::Value =
                serde_json::from_str(&read_response(reader).expect("no definition response"))
                    .unwrap();
            let arr = resp["result"]
                .as_array()
                .cloned()
                .or_else(|| {
                    resp["result"]
                        .as_object()
                        .map(|o| vec![serde_json::Value::Object(o.clone())])
                })
                .unwrap_or_default();
            let out: Vec<String> = arr
                .iter()
                .filter_map(|l| Some(l["uri"].as_str()?.to_string()))
                .collect();
            if !out.is_empty() {
                return out;
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        Vec::new()
    };

    let shared = goto(&mut child, &mut reader, 1, 100);
    assert!(
        shared.iter().any(|u| u.ends_with("def_l_english.yml")),
        "a key the mod redefines should resolve to the mod's file, got: {:?}",
        shared
    );
    assert!(
        !shared.iter().any(|u| u.ends_with("base_l_english.yml")),
        "the base game must not win over the mod, got: {:?}",
        shared
    );
    let base_only = goto(&mut child, &mut reader, 2, 200);
    assert!(
        base_only.iter().any(|u| u.ends_with("base_l_english.yml")),
        "a base-game-only key should resolve to the install, got: {:?}",
        base_only
    );

    // Take the install's loc away and re-index. The memo still has it, so the
    // key resolves; a rescan that re-read the install would lose it.
    std::fs::remove_dir_all(vanilla.path().join("localisation")).unwrap();
    write_frame(
        &mut child,
        &jsonrpc_request(
            300,
            "workspace/executeCommand",
            serde_json::json!({ "command": "reindexWorkspace", "arguments": [] }),
        ),
    )
    .unwrap();
    let reindexed: serde_json::Value =
        serde_json::from_str(&read_response(&mut reader).expect("no reindex response")).unwrap();
    assert_eq!(
        reindexed["result"].as_str(),
        Some("Workspace re-indexed."),
        "re-index should have run, got: {reindexed}"
    );

    let after = goto(&mut child, &mut reader, 2, 400);
    child.kill().ok();
    assert!(
        after.iter().any(|u| u.ends_with("base_l_english.yml")),
        "the base game's loc must survive a re-index without being re-read, got: {:?}",
        after
    );
}

// ── did_open re-validates open dependents (stale scripted_effect bug) ─────────

/// Read frames until a publishDiagnostics for a URI ending in `suffix` arrives
/// (after at least `min_skips` matching ones already seen), returning its codes.
/// Returns None on timeout.
fn diags_for(
    reader: &mut BufReader<std::process::ChildStdout>,
    suffix: &str,
    occurrence: usize,
) -> Option<Vec<String>> {
    let mut seen = 0usize;
    for _ in 0..2000 {
        let raw = read_frame(reader).ok()?;
        if raw.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        if v["method"] == "textDocument/publishDiagnostics"
            && v["params"]["uri"]
                .as_str()
                .is_some_and(|u| u.ends_with(suffix))
        {
            seen += 1;
            if seen >= occurrence {
                return Some(
                    v["params"]["diagnostics"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|d| d["code"].as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default(),
                );
            }
        }
    }
    None
}

/// Read frames until the `loadingBar` notification with `enable=false` arrives,
/// i.e. the workspace scan finished (index_ready is now set).
///
/// Panics if the scan never finishes, naming the reason. This used to return
/// quietly on a read error and on running out of frames, so a server that died
/// during startup was handed back to the caller as a ready one; the test then
/// failed much later, on whatever it waited for next, blaming that instead
/// (#198). Report a server exit here, where it happened.
fn wait_for_scan_done(reader: &mut BufReader<std::process::ChildStdout>) {
    for _ in 0..5000 {
        let raw = match read_frame(reader) {
            Ok(raw) => raw,
            Err(e) => panic!("the server exited before its scan finished: {e}"),
        };
        if raw.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw)
            && v["method"] == "loadingBar"
            && v["params"]["enable"] == serde_json::Value::Bool(false)
        {
            return;
        }
    }
    panic!("no loadingBar(false) scan-finished signal in the first 5000 frames");
}

/// Read frames from a [`spawn_frame_collector`] channel until one satisfies
/// `want`, handing every frame passed over to `saw`. Returns the match.
///
/// The single bounded wait the progress and response waits are built from, and
/// the one place that tells the two ways of not getting a frame apart. The
/// collector drops its sender the moment the server's stdout hits EOF, so a
/// server that exited comes back as `Disconnected` and fails immediately with
/// that as the reason; only a server that is alive but slow can spend the whole
/// `budget`. Waits that treated the two alike reported a dead server as a
/// missing notification and blamed the scan for it (#198).
///
/// `what` completes "waiting for …", so phrase it as the thing awaited.
fn recv_frame_until(
    rx: &std::sync::mpsc::Receiver<serde_json::Value>,
    budget: std::time::Duration,
    what: &str,
    mut saw: impl FnMut(&serde_json::Value),
    mut want: impl FnMut(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let start = std::time::Instant::now();
    let deadline = start + budget;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out after {budget:?} waiting for {what}; the server was still running"
        );
        match rx.recv_timeout(remaining) {
            Ok(v) if want(&v) => return v,
            Ok(v) => saw(&v),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => panic!(
                "the server exited after {:?} without sending {what}",
                start.elapsed()
            ),
        }
    }
}

/// A `loadingBar` with `enable=true`: a scan has started.
fn is_scan_started(v: &serde_json::Value) -> bool {
    v["method"] == "loadingBar" && v["params"]["enable"] == serde_json::Value::Bool(true)
}

/// Wait for a scan-started signal — `loadingBar` with `enable=true` — on a
/// [`spawn_frame_collector`] channel. `what` names the scan awaited, so a
/// failure says which one never started.
///
/// `CWTOOLS_SCAN_HOLD_MS` (and `CWTOOLS_SCAN_HOLD_FILE`, which holds while the
/// named path exists) holds the scan open *after* this signal fires (see
/// `scan::hold_scan_for_tests`), so the wait for the started signal
/// is a measure of server command-processing latency under load, not of the
/// hold. Size `budget` to that latency (a generous fixed value), not to the
/// hold magnitude: the hold cannot expire before the signal it follows, so a
/// larger budget never lets the scan finish first (#212).
fn wait_for_scan_started(
    rx: &std::sync::mpsc::Receiver<serde_json::Value>,
    budget: std::time::Duration,
    what: &str,
) {
    recv_frame_until(rx, budget, what, |_| {}, is_scan_started);
}

/// Send `reindexWorkspace` until it actually starts a scan, returning once the
/// scan-started signal is in hand. The gate for every test that needs a scan
/// running — and holding, under `CWTOOLS_SCAN_HOLD_MS` — before it acts.
///
/// Waiting on the bar-on alone is not enough, because the command is allowed to
/// do nothing: `Backend::validate_entire_workspace` returns `false` and sends no
/// `loadingBar` at all when it loses the re-entrancy CAS, and the previous
/// scan's bar-off goes out *before* that guard drops. `storm_server_env`
/// returns on the startup scan's bar-off, so a `reindexWorkspace` sent straight
/// afterwards can land in exactly that window, skip, and leave the test waiting
/// for a signal that is never coming. No budget fixes that; the test has to
/// notice the command answered without scanning and ask again (#198).
///
/// A skipped command answers immediately, whereas a scan that runs sends its
/// bar-on before the hold and answers only after — so "answered with no bar-on
/// yet" identifies the skip without depending on the message text. The ids come
/// from a private range so they cannot collide with a caller's own requests.
fn reindex_until_scan_starts(
    child: &mut std::process::Child,
    rx: &std::sync::mpsc::Receiver<serde_json::Value>,
) {
    for attempt in 0..20 {
        let id = 7900 + attempt;
        write_frame(
            child,
            &jsonrpc_request(
                id,
                "workspace/executeCommand",
                serde_json::json!({ "command": "reindexWorkspace", "arguments": [] }),
            ),
        )
        .unwrap();
        let v = recv_frame_until(
            rx,
            std::time::Duration::from_secs(30),
            "the reindex scan to start, or the command to answer without starting one",
            |_| {},
            |v| is_scan_started(v) || v["id"] == id,
        );
        if is_scan_started(&v) {
            return;
        }
        // Answered with no bar: it lost the CAS to the tail of the previous
        // scan. That scan is on its way out, so give the guard a moment to drop
        // rather than spending the retries inside the same window.
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    panic!("reindexWorkspace answered without starting a scan 20 times running");
}

/// Wait for the response to request `id`, reporting whether any scan started
/// before it arrived. Pairs with [`wait_for_scan_started`] for the commands
/// whose contract is about *which* side of a scan the answer lands on.
fn wait_for_response_watching_scans(
    rx: &std::sync::mpsc::Receiver<serde_json::Value>,
    id: i64,
    budget: std::time::Duration,
    what: &str,
) -> (serde_json::Value, bool) {
    let mut saw_scan = false;
    let response = recv_frame_until(
        rx,
        budget,
        what,
        |v| saw_scan |= is_scan_started(v),
        |v| v["id"] == id,
    );
    (response, saw_scan)
}

/// Assert no response to `id` arrives for `quiet`. A server that exited would
/// satisfy that vacuously, so a closed channel fails rather than passes.
fn assert_no_response_within(
    rx: &std::sync::mpsc::Receiver<serde_json::Value>,
    id: i64,
    quiet: std::time::Duration,
    what: &str,
) {
    let deadline = std::time::Instant::now() + quiet;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        match rx.recv_timeout(remaining) {
            Ok(v) => assert!(v["id"] != id, "{what}: {v}"),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the server exited during the window in which {what}")
            }
        }
    }
}

#[test]
fn test_did_open_definition_clears_open_caller_stale_error() {
    // Caller B references scripted_effect my_se; the defining file A is opened
    // afterwards. Opening A must re-validate B so its "undefined" diagnostic
    // (CW263 — the call matches no `<scripted_effect>` alias until my_se is
    // indexed) clears without a manual re-save.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap(); // empty dir → index marked complete
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    // Only the caller exists on disk at first; the definition is added later.
    let b_rel = "common/decisions/b.txt";
    let b_path = ws.path().join(b_rel);
    std::fs::create_dir_all(b_path.parent().unwrap()).unwrap();
    std::fs::write(
        &b_path,
        "my_dec = {\n    complete_effect = {\n        my_se = yes\n    }\n}\n",
    )
    .unwrap();

    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "vanilla": vanilla.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &init).unwrap();
    let _ = read_response(&mut reader);
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    // Wait until the scan finishes so diagnostics are no longer deferred.
    wait_for_scan_done(&mut reader);

    // Open the caller; the definition is absent, so B shows CW263.
    let b_uri = path_uri(&b_path);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":b_uri,"languageId":"hoi4","version":1,
                "text":"my_dec = {\n    complete_effect = {\n        my_se = yes\n    }\n}\n"}}),
        ),
    )
    .unwrap();
    let before = diags_for(&mut reader, "b.txt", 1).expect("B diagnostics");
    assert!(
        before.contains(&"CW263".to_string()),
        "expected CW263 before the definition is opened, got: {:?}",
        before
    );

    // Now create + open the defining scripted_effect file.
    let a_rel = "common/scripted_effects/a.txt";
    let a_path = ws.path().join(a_rel);
    std::fs::create_dir_all(a_path.parent().unwrap()).unwrap();
    std::fs::write(&a_path, "my_se = { log = \"hi\" }\n").unwrap();
    let a_uri = path_uri(&a_path);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":a_uri,"languageId":"hoi4","version":1,
                "text":"my_se = { log = \"hi\" }\n"}}),
        ),
    )
    .unwrap();

    // The did_open dependent sweep must re-publish B without the CW263.
    let after = diags_for(&mut reader, "b.txt", 1).expect("B re-validated");
    child.kill().ok();
    assert!(
        !after.contains(&"CW263".to_string()),
        "opening the definition file should clear B's stale CW263, got: {:?}",
        after
    );
}

// ── CW239: unused should_be_used instances in the LSP (issue #123) ──────────

const UNUSED_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        should_be_used = yes
    }
    type[user] = { path = "game/common/users" }
}
thing = { x = scalar }
user = { uses = <thing> }
"#;

/// Spawn a server over a workspace with one `should_be_used` type: `a.txt`
/// defines `used_thing` (referenced from `b.txt`) and `lone_thing` (referenced
/// from nowhere). Returns the child, reader, and the two file paths.
#[allow(clippy::type_complexity)]
fn spawn_unused_workspace() -> (
    tempfile::TempDir,
    std::process::Child,
    BufReader<std::process::ChildStdout>,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap(); // empty dir → index marked complete
    std::fs::write(rules_dir.path().join("r.cwt"), UNUSED_RULES).unwrap();

    let a_path = ws.path().join("common/things/a.txt");
    std::fs::create_dir_all(a_path.parent().unwrap()).unwrap();
    std::fs::write(&a_path, A_TEXT).unwrap();
    let b_path = ws.path().join("common/users/b.txt");
    std::fs::create_dir_all(b_path.parent().unwrap()).unwrap();
    std::fs::write(&b_path, B_TEXT).unwrap();

    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let reader = BufReader::new(child.stdout.take().unwrap());

    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "vanilla": vanilla.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &init).unwrap();
    let mut reader = reader;
    let _ = read_response(&mut reader);
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    // rules_dir/vanilla TempDirs may drop once the scan has read them; the
    // workspace must outlive the test, so it is returned.
    (ws, child, reader, a_path, b_path)
}

const A_TEXT: &str = "used_thing = { x = a }\nlone_thing = { x = b }\n";
const B_TEXT: &str = "a_user = { uses = used_thing }\n";

const ALIAS_BRANCH_LIMIT_RULES: &str = r#"
types = {
    type[user] = { path = "game/common/users" }
}
user = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
## severity = warning
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
alias[effect:needs_int] = int
"#;

/// A `common/users` file whose alias usages exhaust the branch budget, preceded
/// by `noise` single-overload value errors. Those cost no budget, so the caller
/// can push the file past the server's per-file diagnostic cap without changing
/// where the cap falls.
///
/// Every usage is its own, so there is nothing for the alias memo to reuse and
/// the budget is what stops the file: one two-overload usage past capacity.
fn capped_alias_file(noise: usize) -> String {
    let mut text = String::from("a_user = {\n");
    for _ in 0..noise {
        text.push_str("needs_int = nope\n");
    }
    for _ in 0..32_769 {
        text.push_str("recurse = { }\n");
    }
    text.push_str("}\n");
    text
}

/// Write `files` into a fresh workspace under `rules` and return the
/// initialized server. The temp dirs come back so the caller keeps them alive
/// for the duration of the scan.
fn spawn_alias_workspace(
    rules: &str,
    files: &[(&str, &str)],
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    tempfile::TempDir,
    std::process::Child,
    BufReader<std::process::ChildStdout>,
) {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), rules).unwrap();

    for (rel, text) in files {
        let file_path = ws.path().join(rel);
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, text).unwrap();
    }

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(ws.path()),
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "vanilla": vanilla.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    (ws, rules_dir, vanilla, child, reader)
}

const CAPPED_ALIAS_REL_PATH: &str = "common/users/capped.txt";

/// [`ALIAS_BRANCH_LIMIT_RULES`] plus a `should_be_used` type, so the same capped
/// file also decides whether the unused-instance check can run.
const CAPPED_UNUSED_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        should_be_used = yes
    }
    type[user] = { path = "game/common/users" }
}
thing = { x = scalar }
user = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
## severity = warning
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
alias[effect:needs_int] = int
"#;

#[test]
fn test_scan_reports_unused_should_be_used_instance() {
    // The workspace scan runs the batch-style two-phase pass, so a definition
    // nothing in the workspace references gets CW239 without any file open.
    let (_ws, mut child, mut reader, _a, _b) = spawn_unused_workspace();
    let a_diags = diags_for(&mut reader, "a.txt", 1).expect("a.txt scan diagnostics");
    child.kill().ok();
    assert!(
        a_diags.contains(&"CW239".to_string()),
        "lone_thing is referenced nowhere, expected CW239, got: {a_diags:?}"
    );
}

#[test]
fn test_scan_reports_alias_branch_limit() {
    let (_ws, _rules, _vanilla, mut child, mut reader) = spawn_alias_workspace(
        ALIAS_BRANCH_LIMIT_RULES,
        &[(CAPPED_ALIAS_REL_PATH, capped_alias_file(0).as_str())],
    );
    let diagnostics =
        diags_for(&mut reader, CAPPED_ALIAS_REL_PATH, 1).expect("capped-file diagnostics");
    child.kill().ok();
    assert_eq!(
        diagnostics
            .iter()
            .filter(|code| code.as_str() == "CW277")
            .count(),
        1,
        "the LSP scan must publish one alias branch-limit diagnostic: {diagnostics:?}"
    );
}

/// The branch-limit diagnostic is emitted last, and a capped file is exactly the
/// kind that also floods. It has to survive the server's per-file diagnostic cap
/// (`MAX_FILE_ERRORS`), or the editor shows a truncated list with no sign that
/// validation stopped early.
#[test]
fn test_alias_branch_limit_survives_the_per_file_diagnostic_cap() {
    let (_ws, _rules, _vanilla, mut child, mut reader) = spawn_alias_workspace(
        ALIAS_BRANCH_LIMIT_RULES,
        &[(CAPPED_ALIAS_REL_PATH, capped_alias_file(150).as_str())],
    );
    let diagnostics =
        diags_for(&mut reader, CAPPED_ALIAS_REL_PATH, 1).expect("capped-file diagnostics");
    child.kill().ok();
    let (limit, rest): (Vec<_>, Vec<_>) = diagnostics.iter().partition(|c| c.as_str() == "CW277");
    assert_eq!(
        limit.len(),
        1,
        "the branch limit must survive truncation: {diagnostics:?}"
    );
    // MAX_FILE_ERRORS is 100 and is not visible from an integration test. The
    // count pins that the flood really did trip the cap, so the assertion above
    // can't pass just because the file stayed small.
    assert_eq!(
        rest.len(),
        100,
        "the rest of the file should be truncated at the cap: {} diagnostics",
        rest.len()
    );
}

/// A capped file cannot establish every use, so the server marks every tracked
/// instance used rather than reporting definitions as unused on incomplete
/// evidence. That suppression must lift once the file validates fully again —
/// the server keeps its per-file use store across edits, so a stale "everything
/// is used" entry would silence CW239 for the whole workspace for good.
#[test]
fn test_capped_file_suppresses_cw239_until_it_validates_again() {
    const LONE: &str = "lone_thing = { x = a }\n";
    let (ws, _rules, _vanilla, mut child, mut reader) = spawn_alias_workspace(
        CAPPED_UNUSED_RULES,
        &[
            ("common/things/a.txt", LONE),
            (CAPPED_ALIAS_REL_PATH, capped_alias_file(0).as_str()),
        ],
    );
    wait_for_scan_done(&mut reader);

    let a_uri = path_uri(ws.path().join("common/things/a.txt"));
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":a_uri,"languageId":"hoi4","version":1,
                "text": LONE}}),
        ),
    )
    .unwrap();
    let while_capped = diags_for(&mut reader, "a.txt", 1).expect("a.txt diagnostics");
    assert!(
        !while_capped.contains(&"CW239".to_string()),
        "a capped file can't prove nothing uses lone_thing: {while_capped:?}"
    );

    // Same file without the recursion: it validates fully, records no uses, and
    // lone_thing really is referenced nowhere.
    let capped_uri = path_uri(ws.path().join(CAPPED_ALIAS_REL_PATH));
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":capped_uri,"languageId":"hoi4","version":1,
                "text": capped_alias_file(0)}}),
        ),
    )
    .unwrap();
    let _ = diags_for(&mut reader, "capped.txt", 1).expect("capped.txt diagnostics");
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": capped_uri, "version": 2 },
                "contentChanges": [{ "text": "a_user = { }\n" }]
            }),
        ),
    )
    .unwrap();
    let after_fix = diags_for(&mut reader, "a.txt", 1).expect("a.txt re-validated after the fix");
    child.kill().ok();
    drop(ws);
    assert!(
        after_fix.contains(&"CW239".to_string()),
        "with the cap lifted lone_thing is unreferenced and must report: {after_fix:?}"
    );
}

#[test]
fn test_edit_toggling_reference_updates_open_cw239() {
    // Editing a reference in one open file must flip CW239 on the open file
    // that DEFINES the instance, through the dependent sweep — no rescan.
    let (ws, mut child, mut reader, a_path, b_path) = spawn_unused_workspace();
    wait_for_scan_done(&mut reader);

    let a_uri = path_uri(&a_path);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":a_uri,"languageId":"hoi4","version":1,
                "text": A_TEXT}}),
        ),
    )
    .unwrap();
    let before = diags_for(&mut reader, "a.txt", 1).expect("a.txt diagnostics");
    assert!(
        before.contains(&"CW239".to_string()),
        "expected CW239 on lone_thing while unreferenced, got: {before:?}"
    );

    let b_uri = path_uri(&b_path);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":b_uri,"languageId":"hoi4","version":1,
                "text": B_TEXT}}),
        ),
    )
    .unwrap();
    let _ = diags_for(&mut reader, "b.txt", 1).expect("b.txt diagnostics");

    // Add a reference to lone_thing: the sweep must republish a.txt clean.
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": b_uri, "version": 2 },
                "contentChanges": [{ "text":
                    "a_user = { uses = used_thing }\nb_user = { uses = lone_thing }\n" }]
            }),
        ),
    )
    .unwrap();
    let after_add = diags_for(&mut reader, "a.txt", 1).expect("a.txt re-validated after add");
    assert!(
        !after_add.contains(&"CW239".to_string()),
        "adding a reference should clear lone_thing's CW239, got: {after_add:?}"
    );

    // Remove it again: the CW239 must come back the same way.
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": b_uri, "version": 3 },
                "contentChanges": [{ "text": B_TEXT }]
            }),
        ),
    )
    .unwrap();
    let after_remove = diags_for(&mut reader, "a.txt", 1).expect("a.txt re-validated after remove");
    child.kill().ok();
    drop(ws);
    assert!(
        after_remove.contains(&"CW239".to_string()),
        "removing the only reference should resurrect CW239, got: {after_remove:?}"
    );
}

#[test]
fn test_closing_a_buffer_with_discarded_edits_restores_disk_uses() {
    // Close b.txt with unsaved edits that dropped its reference to used_thing.
    // Disk still has the reference, and did_close re-indexes from disk, so the
    // recorded uses must come from disk too, or a.txt keeps the CW239 the
    // discarded buffer earned with nothing to clear it (#133).
    let (ws, mut child, mut reader, a_path, b_path) = spawn_unused_workspace();
    wait_for_scan_done(&mut reader);

    let a_uri = path_uri(&a_path);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":a_uri,"languageId":"hoi4","version":1,
                "text": A_TEXT}}),
        ),
    )
    .unwrap();
    let before = diags_for(&mut reader, "a.txt", 1).expect("a.txt diagnostics");
    assert_eq!(
        before.iter().filter(|c| *c == "CW239").count(),
        1,
        "only lone_thing is unreferenced at rest, got: {before:?}"
    );

    let b_uri = path_uri(&b_path);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":b_uri,"languageId":"hoi4","version":1,
                "text": B_TEXT}}),
        ),
    )
    .unwrap();
    let _ = diags_for(&mut reader, "b.txt", 1).expect("b.txt diagnostics");

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": b_uri, "version": 2 },
                "contentChanges": [{ "text": "a_user = { }\n" }]
            }),
        ),
    )
    .unwrap();
    let while_edited = diags_for(&mut reader, "a.txt", 1).expect("a.txt re-validated after edit");
    assert_eq!(
        while_edited.iter().filter(|c| *c == "CW239").count(),
        2,
        "the unsaved edit drops used_thing's only reference, got: {while_edited:?}"
    );

    // Close without saving: b.txt on disk still references used_thing.
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didClose",
            serde_json::json!({"textDocument":{"uri":b_uri}}),
        ),
    )
    .unwrap();
    // Collected with a budget rather than read straight off the pipe: without
    // the refresh nothing republishes a.txt at all, which must fail the test
    // rather than block it.
    let rx = spawn_frame_collector(reader);
    let frames = drain_after_first(
        &rx,
        std::time::Duration::from_millis(800),
        std::time::Duration::from_secs(6),
    );
    child.kill().ok();
    drop(ws);

    let republished = frames
        .iter()
        .rev()
        .find(|v| {
            v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with("a.txt"))
        })
        .expect("closing b.txt must re-validate a.txt");
    let codes: Vec<&str> = republished["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["code"].as_str())
        .collect();
    assert_eq!(
        codes.iter().filter(|c| **c == "CW239").count(),
        1,
        "discarding the edit restores the reference, so only lone_thing stays \
         unused, got: {codes:?}"
    );
}

#[test]
fn test_closed_file_cw239_only_catches_up_on_the_next_scan() {
    // The other half of the staleness contract ERROR_CODES.md states for CW239:
    // an open file's answer moves with every edit (the test above), a closed
    // file's waits for the next scan. Only the first half was covered, so the
    // dependent sweep growing a "republish closed files too" path — which would
    // mean re-reading and re-validating arbitrarily many files per keystroke —
    // would break nothing.
    let (ws, mut child, mut reader, _a_path, b_path) = spawn_unused_workspace();
    let scanned = diags_for(&mut reader, "a.txt", 1).expect("a.txt scan diagnostics");
    assert!(
        scanned.contains(&"CW239".to_string()),
        "lone_thing starts out referenced by nothing, got: {scanned:?}"
    );
    wait_for_scan_done(&mut reader);

    let b_uri = path_uri(&b_path);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":b_uri,"languageId":"hoi4","version":1,
                "text": B_TEXT}}),
        ),
    )
    .unwrap();
    let _ = diags_for(&mut reader, "b.txt", 1).expect("b.txt diagnostics");

    // Reference lone_thing from the open buffer. a.txt, which defines it, is
    // closed and stays closed.
    let rx = spawn_frame_collector(reader);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": b_uri, "version": 2 },
                "contentChanges": [{ "text":
                    "a_user = { uses = used_thing }\nb_user = { uses = lone_thing }\n" }]
            }),
        ),
    )
    .unwrap();
    let frames = drain_after_first(
        &rx,
        std::time::Duration::from_millis(800),
        std::time::Duration::from_secs(6),
    );
    assert_eq!(
        count_publishes(&frames, "b.txt"),
        1,
        "the edit must be validated, or nothing below is being tested"
    );
    assert_eq!(
        count_publishes(&frames, "a.txt"),
        0,
        "a closed file must not be republished by the open-file sweep"
    );

    // The scan is what refreshes it, and it counts the open buffer's unsaved
    // reference: the diagnostic clears without b.txt ever being written to disk.
    reindex_until_scan_starts(&mut child, &rx);
    let republished = recv_frame_until(
        &rx,
        std::time::Duration::from_secs(30),
        "the reindex to republish a.txt",
        |_| {},
        |v| {
            v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with("a.txt"))
        },
    );
    child.kill().ok();
    drop(ws);

    let codes: Vec<&str> = republished["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["code"].as_str())
        .collect();
    assert!(
        !codes.contains(&"CW239"),
        "the scan must catch the closed file up on the reference added since, \
         got: {codes:?}"
    );
}

// ── B5/B7: document symbols, folding, highlight, cross-file references/rename ──

/// Spawn a server with `rules`, write `files` to disk, initialize with
/// `client_caps`, run the workspace scan (which indexes every file, open or
/// not), didOpen the `open` files, then issue `method` against `doc_rel` with
/// `extra` merged into the request params. Polls until a non-empty `result`
/// arrives. Returns the JSON `result`.
fn feature_request(
    rules: &str,
    files: &[(&str, &str)],
    open: &[&str],
    client_caps: serde_json::Value,
    doc_rel: &str,
    method: &str,
    extra: serde_json::Value,
) -> serde_json::Value {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap(); // empty dir → index marked complete
    std::fs::write(rules_dir.path().join("r.cwt"), rules).unwrap();
    for (rel, content) in files {
        let p = ws.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
    }
    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": client_caps,
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "vanilla": vanilla.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &init).unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    for &rel in open {
        let content = files
            .iter()
            .find(|(r, _)| *r == rel)
            .map(|(_, c)| *c)
            .unwrap();
        let uri = path_uri(ws.path().join(rel));
        write_frame(
            &mut child,
            &jsonrpc_notification(
                "textDocument/didOpen",
                serde_json::json!({
                    "textDocument": {"uri": uri, "languageId": "hoi4", "version": 1, "text": content}
                }),
            ),
        )
        .unwrap();
        wait_for_diagnostics(&mut reader, rel);
    }

    let doc_uri = path_uri(ws.path().join(doc_rel));
    let mut result = serde_json::Value::Null;
    for attempt in 0..40 {
        let mut params = serde_json::json!({ "textDocument": { "uri": doc_uri } });
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                params[k.as_str()] = v.clone();
            }
        }
        let req = jsonrpc_request(100 + attempt, method, params);
        write_frame(&mut child, &req).unwrap();
        let resp_str = read_response(&mut reader).expect("no response");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        result = resp["result"].clone();
        let empty = result.is_null() || result.as_array().map(|a| a.is_empty()).unwrap_or(false);
        if !empty {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    child.kill().ok();
    result
}

#[test]
fn test_document_symbols_nested() {
    // A focus tree with two focuses: the outline is a nested tree, each block
    // named by its `id` child so repeated `focus` keys stay distinct.
    let doc = "focus_tree = {\n    id = my_tree\n    focus = {\n        id = focus_a\n        x = 1\n    }\n    focus = {\n        id = focus_b\n    }\n}\n";
    let files = &[("common/national_focus/f.txt", doc)];
    let caps = serde_json::json!({
        "textDocument": { "documentSymbol": { "hierarchicalDocumentSymbolSupport": true } }
    });
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/national_focus/f.txt"],
        caps,
        "common/national_focus/f.txt",
        "textDocument/documentSymbol",
        serde_json::json!({}),
    );
    let syms = result.as_array().expect("nested symbols array");
    let tree = &syms[0];
    assert_eq!(
        tree["name"], "my_tree",
        "top symbol named by id, got: {}",
        result
    );
    // selection_range ⊆ range: they share a start, selection ends within range.
    assert_eq!(tree["selectionRange"]["start"], tree["range"]["start"]);
    let children = tree["children"].as_array().expect("nested children");
    assert!(
        children.iter().any(|c| c["name"] == "focus_a"),
        "expected nested focus_a, got: {}",
        result
    );
    assert!(
        children.iter().any(|c| c["name"] == "focus_b"),
        "expected nested focus_b, got: {}",
        result
    );
}

#[test]
fn test_folding_ranges_nested_blocks() {
    let doc = "outer = {\n    inner = {\n        x = 1\n    }\n}\n";
    let files = &[("common/national_focus/f.txt", doc)];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/national_focus/f.txt"],
        serde_json::json!({}),
        "common/national_focus/f.txt",
        "textDocument/foldingRange",
        serde_json::json!({}),
    );
    let ranges = result.as_array().expect("folding ranges");
    let has = |s: u64, e: u64| {
        ranges
            .iter()
            .any(|r| r["startLine"] == s && r["endLine"] == e)
    };
    assert!(has(0, 4), "expected outer fold 0..4, got: {}", result);
    assert!(has(1, 3), "expected inner fold 1..3, got: {}", result);
}

#[test]
fn test_folding_ranges_comments_and_regions() {
    // A comment block folds as kind "comment"; #region/#endregion pairs fold
    // as kind "region"; brace folds are unchanged.
    let doc = "# header one\n# header two\n# header three\n#region Alpha\na = {\n    x = 1\n}\n#endregion\n";
    let files = &[("common/national_focus/f.txt", doc)];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/national_focus/f.txt"],
        serde_json::json!({}),
        "common/national_focus/f.txt",
        "textDocument/foldingRange",
        serde_json::json!({}),
    );
    let ranges = result.as_array().expect("folding ranges");
    let has = |s: u64, e: u64, k: &str| {
        ranges
            .iter()
            .any(|r| r["startLine"] == s && r["endLine"] == e && r["kind"] == k)
    };
    assert!(has(0, 2, "comment"), "comment block fold, got: {}", result);
    assert!(has(3, 7, "region"), "#region marker fold, got: {}", result);
    assert!(has(4, 6, "region"), "brace fold, got: {}", result);
}

#[test]
fn test_selection_range_expands_token_to_blocks() {
    // Expanding from `bar`: token, then block content, then the full block.
    let doc = "a = {\n    foo = bar\n}\n";
    let files = &[("common/national_focus/f.txt", doc)];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/national_focus/f.txt"],
        serde_json::json!({}),
        "common/national_focus/f.txt",
        "textDocument/selectionRange",
        serde_json::json!({ "positions": [{ "line": 1, "character": 10 }] }),
    );
    let chain = &result.as_array().expect("one entry per position")[0];
    assert_eq!(
        chain["range"]["start"]["character"], 10,
        "token start: {}",
        result
    );
    assert_eq!(
        chain["range"]["end"]["character"], 13,
        "token end: {}",
        result
    );
    let parent = &chain["parent"];
    assert_eq!(
        parent["range"]["start"]["line"], 0,
        "content parent: {}",
        result
    );
    let grandparent = &parent["parent"];
    assert_eq!(
        grandparent["range"]["start"]["character"], 4,
        "full-pair grandparent starts at the open brace: {}",
        result
    );
}

#[test]
fn test_document_highlight_occurrences() {
    // `MY_FOCUS` appears three times; highlighting one returns all three.
    let doc = "a = {\n    has_focus = MY_FOCUS\n}\nb = {\n    has_focus = MY_FOCUS\n}\nc = {\n    load_oob = MY_FOCUS\n}\n";
    let files = &[("common/decisions/d.txt", doc)];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/d.txt"],
        serde_json::json!({}),
        "common/decisions/d.txt",
        "textDocument/documentHighlight",
        serde_json::json!({ "position": { "line": 1, "character": 16 } }),
    );
    let hl = result.as_array().expect("highlights array");
    assert_eq!(
        hl.len(),
        3,
        "expected 3 occurrences of MY_FOCUS, got: {}",
        result
    );
}

#[test]
fn test_document_highlight_skips_comments_and_reports_kinds() {
    // MY_FOCUS appears twice in code and once in a comment: the comment
    // occurrence must not be highlighted, and value occurrences report
    // kind Read (2) instead of Text (1).
    let doc = "a = {\n    has_focus = MY_FOCUS\n}\n# note MY_FOCUS legacy\nb = {\n    has_focus = MY_FOCUS\n}\n";
    let files = &[("common/decisions/d.txt", doc)];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/d.txt"],
        serde_json::json!({}),
        "common/decisions/d.txt",
        "textDocument/documentHighlight",
        serde_json::json!({ "position": { "line": 1, "character": 16 } }),
    );
    let hl = result.as_array().expect("highlights array");
    assert_eq!(
        hl.len(),
        2,
        "comment occurrence must be skipped, got: {}",
        result
    );
    assert!(
        hl.iter().all(|h| h["kind"] == 2),
        "value occurrences must report Read (2), got: {}",
        result
    );
}

#[test]
fn test_completion_label_details_carry_type_origin() {
    // A client advertising labelDetailsSupport sees the type origin next to
    // instance labels at build time (not deferred to resolve).
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        ("common/decisions/d.txt", "adec = {\n    has_focus = \n}\n"),
    ];
    let caps = serde_json::json!({
        "textDocument": { "completion": { "completionItem": { "labelDetailsSupport": true } } }
    });
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/d.txt"],
        caps,
        "common/decisions/d.txt",
        "textDocument/completion",
        serde_json::json!({ "position": { "line": 1, "character": 16 } }),
    );
    let items = result["items"].as_array().expect("completion items");
    let focus_item = items
        .iter()
        .find(|i| i["label"] == "MY_FOCUS")
        .unwrap_or_else(|| panic!("MY_FOCUS must be offered, got: {}", result));
    assert_eq!(
        focus_item["labelDetails"]["description"], "focus",
        "type origin as labelDetails, got: {}",
        result
    );
}

#[test]
fn test_completion_label_details_absent_without_support() {
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        ("common/decisions/d.txt", "adec = {\n    has_focus = \n}\n"),
    ];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/d.txt"],
        serde_json::json!({}),
        "common/decisions/d.txt",
        "textDocument/completion",
        serde_json::json!({ "position": { "line": 1, "character": 16 } }),
    );
    let items = result["items"].as_array().expect("completion items");
    let focus_item = items
        .iter()
        .find(|i| i["label"] == "MY_FOCUS")
        .unwrap_or_else(|| panic!("MY_FOCUS must be offered, got: {}", result));
    assert!(
        focus_item["labelDetails"].is_null(),
        "no labelDetails without client support, got: {}",
        result
    );
}

#[test]
fn test_cwt_goto_type_reference_jumps_to_definition() {
    // Goto on `<focus>` in an opened .cwt file lands on the `type[focus]`
    // definition inside the loaded rules folder.
    let files = &[("extra/my.cwt", "my_block = {\n    slot = <focus>\n}\n")];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["extra/my.cwt"],
        serde_json::json!({}),
        "extra/my.cwt",
        "textDocument/definition",
        serde_json::json!({ "position": { "line": 1, "character": 14 } }),
    );
    let locs = result.as_array().expect("definition locations");
    assert!(
        locs[0]["uri"].as_str().unwrap_or("").ends_with("r.cwt"),
        "goto must land in the rules file, got: {}",
        result
    );
    assert_eq!(
        locs[0]["range"]["start"]["line"], 2,
        "type[focus] line, got: {}",
        result
    );
}

#[test]
fn test_cwt_hover_type_reference_describes_type() {
    let files = &[("extra/my.cwt", "my_block = {\n    slot = <focus>\n}\n")];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["extra/my.cwt"],
        serde_json::json!({}),
        "extra/my.cwt",
        "textDocument/hover",
        serde_json::json!({ "position": { "line": 1, "character": 14 } }),
    );
    let md = result["contents"]["value"].as_str().unwrap_or("");
    assert!(
        md.contains("focus") && md.contains("type"),
        "hover must describe the type, got: {}",
        result
    );
    assert!(
        md.contains("common/national_focus"),
        "hover must list the type's path, got: {}",
        result
    );
}

const LINK_RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
decision = {
    ## cardinality = 0..1
    picture = filepath[gfx/,.dds]
    ## cardinality = 0..1
    icon = icon[gfx/interface]
}
"#;

#[test]
fn test_document_links_resolve_filepath_and_icon_leaves() {
    // `picture` is filepath[gfx/,.dds] and `icon` is icon[gfx/interface]:
    // both leaves become links to the files they reference. A value whose
    // target doesn't exist gets no link.
    let files = &[
        ("gfx/pic.dds", "dds"),
        ("gfx/interface/myicon.dds", "dds"),
        (
            "common/decisions/d.txt",
            "adec = {\n    picture = pic\n    icon = myicon\n}\n",
        ),
    ];
    let result = feature_request(
        LINK_RULES,
        files,
        &["common/decisions/d.txt"],
        serde_json::json!({}),
        "common/decisions/d.txt",
        "textDocument/documentLink",
        serde_json::json!({}),
    );
    let links = result.as_array().expect("document links");
    let link_at = |line: u64, ch: u64| {
        links
            .iter()
            .find(|l| l["range"]["start"]["line"] == line && l["range"]["start"]["character"] == ch)
            .unwrap_or_else(|| panic!("no link at {}:{}, got: {}", line, ch, result))
    };
    assert!(
        link_at(1, 14)["target"]
            .as_str()
            .unwrap_or("")
            .ends_with("gfx/pic.dds"),
        "picture link target, got: {}",
        result
    );
    assert!(
        link_at(2, 11)["target"]
            .as_str()
            .unwrap_or("")
            .ends_with("gfx/interface/myicon.dds"),
        "icon link target, got: {}",
        result
    );
}

#[test]
fn test_workspace_symbols_rank_and_cover_all_sources() {
    // Three symbol sources match "my": a focus instance (exact-prefix rank on
    // "my_focus"), a loc key (prefix), and an @-constant (substring). The
    // result must cover all three with distinct kinds and be ordered
    // rank-then-name, not hash order.
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        (
            "common/decisions/d.txt",
            "@my_const = 5\nadec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
        (
            "localisation/english/x_l_english.yml",
            "l_english:\n my_focus_tooltip:0 \"Text\"\n",
        ),
    ];
    let result = feature_request(
        GOTO_RULES,
        files,
        &[],
        serde_json::json!({}),
        "common/national_focus/f.txt",
        "workspace/symbol",
        serde_json::json!({ "query": "my" }),
    );
    let syms = result.as_array().expect("symbols array");
    let names: Vec<&str> = syms.iter().filter_map(|s| s["name"].as_str()).collect();
    assert_eq!(
        names,
        vec!["MY_FOCUS", "my_focus_tooltip", "@my_const"],
        "expected rank-then-name order, got: {}",
        result
    );
    let kind_of = |n: &str| {
        syms.iter()
            .find(|s| s["name"] == n)
            .map(|s| s["kind"].as_u64().unwrap())
    };
    assert_eq!(kind_of("MY_FOCUS"), Some(23), "focus instance is STRUCT");
    assert_eq!(kind_of("@my_const"), Some(14), "@-constant is CONSTANT");
    assert_eq!(kind_of("my_focus_tooltip"), Some(20), "loc key is KEY");
}

#[test]
fn test_workspace_symbols_empty_query_caps_at_limit_best_by_name() {
    // The picker's initial request carries an empty query, which admits every
    // symbol at substring rank. With 511 symbols in the workspace the answer
    // must be exactly the 500 lexicographically-first names, not an arbitrary
    // subset and not the whole universe.
    let mut loc = String::from("l_english:\n");
    for i in 0..510 {
        loc.push_str(&format!(" key_{i:03}:0 \"T\"\n"));
    }
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        ("localisation/english/big_l_english.yml", loc.as_str()),
    ];
    let result = feature_request(
        GOTO_RULES,
        files,
        &[],
        serde_json::json!({}),
        "common/national_focus/f.txt",
        "workspace/symbol",
        serde_json::json!({ "query": "" }),
    );
    let syms = result.as_array().expect("symbols array");
    let names: Vec<&str> = syms.iter().filter_map(|s| s["name"].as_str()).collect();
    assert_eq!(names.len(), 500, "capped at 500, got {}", names.len());
    // All ranks tie for the empty query, so order is by name: "MY_FOCUS"
    // (uppercase sorts before lowercase) then key_000..key_498; the last
    // eleven keys fall past the cap.
    assert_eq!(names[0], "MY_FOCUS");
    assert_eq!(names[1], "key_000");
    assert_eq!(names[499], "key_498");
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "response is name-ordered");
}

#[test]
fn test_references_finds_closed_file() {
    // A (open) and B (never opened) both reference focus MY_FOCUS. Find-refs from
    // A must reach B via the workspace reverse index.
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        (
            "common/decisions/a.txt",
            "adec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
        (
            "common/decisions/b.txt",
            "bdec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
    ];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/a.txt"],
        serde_json::json!({}),
        "common/decisions/a.txt",
        "textDocument/references",
        serde_json::json!({
            "position": { "line": 1, "character": 16 },
            "context": { "includeDeclaration": true }
        }),
    );
    let locs = result.as_array().expect("references array");
    assert!(
        locs.iter()
            .any(|l| l["uri"].as_str().unwrap_or("").ends_with("decisions/b.txt")),
        "references must include the closed file b.txt, got: {}",
        result
    );
}

#[test]
fn test_references_exclude_declaration_omits_definition() {
    // Same layout, but the client asks for includeDeclaration = false: the
    // definition site in national_focus/f.txt must be omitted while the use
    // sites in the decision files are still returned.
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        (
            "common/decisions/a.txt",
            "adec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
        (
            "common/decisions/b.txt",
            "bdec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
    ];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/a.txt"],
        serde_json::json!({}),
        "common/decisions/a.txt",
        "textDocument/references",
        serde_json::json!({
            "position": { "line": 1, "character": 16 },
            "context": { "includeDeclaration": false }
        }),
    );
    let locs = result.as_array().expect("references array");
    assert!(
        locs.iter()
            .any(|l| l["uri"].as_str().unwrap_or("").ends_with("decisions/b.txt")),
        "use sites must still be returned, got: {}",
        result
    );
    assert!(
        !locs.iter().any(|l| l["uri"]
            .as_str()
            .unwrap_or("")
            .ends_with("national_focus/f.txt")),
        "definition must be omitted when includeDeclaration is false, got: {}",
        result
    );
}

#[test]
fn test_rename_edits_closed_file() {
    // Renaming MY_FOCUS from the open file A must also edit the closed file B, at
    // the value column (16), not the key.
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        (
            "common/decisions/a.txt",
            "adec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
        (
            "common/decisions/b.txt",
            "bdec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
    ];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/a.txt"],
        serde_json::json!({}),
        "common/decisions/a.txt",
        "textDocument/rename",
        serde_json::json!({ "position": { "line": 1, "character": 16 }, "newName": "NEW_FOCUS" }),
    );
    let changes = result["changes"]
        .as_object()
        .expect("WorkspaceEdit changes");
    let b_key = changes
        .keys()
        .find(|u| u.ends_with("decisions/b.txt"))
        .unwrap_or_else(|| panic!("rename must edit closed file b.txt, got: {}", result));
    let edits = changes[b_key].as_array().expect("edits for b.txt");
    assert_eq!(
        edits[0]["range"]["start"]["character"], 16,
        "edit must target the value column, got: {}",
        result
    );
    assert_eq!(edits[0]["newText"], "NEW_FOCUS");
}

#[test]
fn test_rename_emits_versioned_document_changes_when_supported() {
    // A client that advertises workspace.workspaceEdit.documentChanges gets
    // TextDocumentEdits with versions: the open doc carries its version, the
    // closed file null.
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        (
            "common/decisions/a.txt",
            "adec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
        (
            "common/decisions/b.txt",
            "bdec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
    ];
    let caps = serde_json::json!({
        "workspace": { "workspaceEdit": { "documentChanges": true } }
    });
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/a.txt"],
        caps,
        "common/decisions/a.txt",
        "textDocument/rename",
        serde_json::json!({ "position": { "line": 1, "character": 16 }, "newName": "NEW_FOCUS" }),
    );
    assert!(
        result["changes"].is_null(),
        "legacy changes must be absent, got: {}",
        result
    );
    let doc_changes = result["documentChanges"]
        .as_array()
        .unwrap_or_else(|| panic!("documentChanges must be present, got: {}", result));
    let entry_for = |suffix: &str| {
        doc_changes
            .iter()
            .find(|e| {
                e["textDocument"]["uri"]
                    .as_str()
                    .unwrap_or("")
                    .ends_with(suffix)
            })
            .unwrap_or_else(|| panic!("no documentChanges entry for {}, got: {}", suffix, result))
    };
    assert_eq!(
        entry_for("decisions/a.txt")["textDocument"]["version"],
        1,
        "open doc carries its version, got: {}",
        result
    );
    assert!(
        entry_for("decisions/b.txt")["textDocument"]["version"].is_null(),
        "closed file version is null, got: {}",
        result
    );
}

/// One `@const` rename in a session with NO workspace folder, driven straight
/// through the wire so a refusal (`error`) is visible instead of being
/// flattened into a null `result` the way [`feature_request`] does. Nothing is
/// scanned and no folder is reported, so `editable_roots` is empty and the edit
/// boundary would refuse every path — the own-document exemption is the only
/// reason these renames still work.
fn at_const_rename_without_workspace(doc_uri: &str, text: &str) -> serde_json::Value {
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {"uri": doc_uri, "languageId": "hoi4", "version": 1, "text": text}
            }),
        ),
    )
    .unwrap();
    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "textDocument/rename",
            serde_json::json!({
                "textDocument": { "uri": doc_uri },
                "position": { "line": 0, "character": 2 },
                "newName": "@renamed",
            }),
        ),
    )
    .unwrap();
    let resp = read_response(&mut reader).expect("no rename response");
    child.kill().ok();
    serde_json::from_str(&resp).unwrap()
}

#[test]
fn test_rename_at_constant_refuses_an_untitled_document() {
    let doc = "@my_const = 5\nadec = {\n    x = @my_const\n}\n";
    let resp = at_const_rename_without_workspace("untitled:Untitled-1", doc);
    assert!(
        resp["error"].is_null(),
        "rename must answer cleanly: {resp}"
    );
    assert!(
        resp["result"].is_null(),
        "an untitled buffer must not enter open-document state: {resp}"
    );
}

#[test]
fn test_rename_at_constant_refuses_a_file_without_a_workspace_folder() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("loose.txt");
    let doc = "@my_const = 5\nadec = {\n    x = @my_const\n}\n";
    std::fs::write(&path, doc).unwrap();
    let doc_uri = path_uri(&path);
    let resp = at_const_rename_without_workspace(&doc_uri, doc);
    assert!(
        resp["error"].is_null(),
        "rename must answer cleanly: {resp}"
    );
    assert!(
        resp["result"].is_null(),
        "a buffer without a workspace root must not enter open-document state: {resp}"
    );
}

/// A rename whose edit set reaches a second document the edit boundary refuses
/// is cancelled outright. Here the definition lives in the base-game install
/// and the workspace only links to it, so the scan indexes it (the link is a
/// workspace path) but writing it would write through the link into the
/// install. Applying only the in-workspace half would leave the reference
/// pointing at a name that no longer matches its definition, so it is the whole
/// edit set or nothing (#160).
///
/// The in-workspace counterpart (a cross-file rename where every target is
/// inside the workspace, which now runs through the same choke point) is
/// `test_rename_edits_closed_file`.
#[cfg(unix)]
#[test]
fn test_rename_does_not_edit_through_a_symlink() {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    // The focus is defined in the base-game install; the workspace reaches it
    // only through a link. The scan rejects the symlink (#161), so the
    // definition is never indexed and rename only edits the in-workspace
    // reference, never the base-game file.
    let source = vanilla.path().join("f_source.txt");
    std::fs::write(&source, "MY_FOCUS = { x = yes }\n").unwrap();
    let linked_def = ws.path().join("common/national_focus/f.txt");
    std::fs::create_dir_all(linked_def.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&source, &linked_def).unwrap();

    let rel = "common/decisions/a.txt";
    let text = "adec = {\n    has_focus = MY_FOCUS\n}\n";
    let path = ws.path().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    let doc_uri = path_uri(&path);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(ws.path()),
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "vanilla": vanilla.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {"uri": doc_uri, "languageId": "hoi4", "version": 1, "text": text}
            }),
        ),
    )
    .unwrap();
    wait_for_diagnostics(&mut reader, rel);

    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "textDocument/rename",
            serde_json::json!({
                "textDocument": { "uri": doc_uri },
                "position": { "line": 1, "character": 16 },
                "newName": "NEW_FOCUS",
            }),
        ),
    )
    .unwrap();
    let resp_str = read_response(&mut reader).expect("no rename response");
    child.kill().ok();
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

    // The symlinked definition is rejected by the scan, so rename only edits
    // the in-workspace reference and never touches the base-game file.
    let changes = resp["result"]["changes"]
        .as_object()
        .unwrap_or_else(|| panic!("rename must carry edits, got: {resp_str}"));
    assert!(
        changes
            .keys()
            .all(|u| u.ends_with("common/decisions/a.txt")),
        "rename must only edit the in-workspace file, got: {resp_str}"
    );
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "MY_FOCUS = { x = yes }\n",
        "the base-game file is untouched"
    );
}

#[test]
fn test_rename_at_constant_renames_file_locally() {
    // `@` script constants are file-local: renaming from the definition
    // rewrites the definition and the read, but never the occurrence in a
    // trailing comment.
    let doc = "@my_const = 5\nadec = {\n    x = @my_const # @my_const stays\n}\n";
    let files = &[("common/decisions/d.txt", doc)];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/d.txt"],
        serde_json::json!({}),
        "common/decisions/d.txt",
        "textDocument/rename",
        serde_json::json!({ "position": { "line": 0, "character": 2 }, "newName": "@renamed" }),
    );
    let changes = result["changes"]
        .as_object()
        .expect("WorkspaceEdit changes");
    let key = changes
        .keys()
        .find(|u| u.ends_with("decisions/d.txt"))
        .unwrap_or_else(|| panic!("rename must edit d.txt, got: {}", result));
    let edits = changes[key].as_array().expect("edits for d.txt");
    let mut spans: Vec<(u64, u64)> = edits
        .iter()
        .map(|e| {
            (
                e["range"]["start"]["line"].as_u64().unwrap(),
                e["range"]["start"]["character"].as_u64().unwrap(),
            )
        })
        .collect();
    spans.sort_unstable();
    assert_eq!(
        spans,
        vec![(0, 0), (2, 8)],
        "definition and read, never the comment, got: {}",
        result
    );
    assert!(edits.iter().all(|e| e["newText"] == "@renamed"));
}

#[test]
fn test_prepare_rename_covers_at_constant_token() {
    let doc = "@my_const = 5\nadec = {\n    x = @my_const\n}\n";
    let files = &[("common/decisions/d.txt", doc)];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/d.txt"],
        serde_json::json!({}),
        "common/decisions/d.txt",
        "textDocument/prepareRename",
        serde_json::json!({ "position": { "line": 2, "character": 10 } }),
    );
    assert_eq!(result["start"]["character"], 8, "token start: {}", result);
    assert_eq!(result["end"]["character"], 17, "token end: {}", result);
}

#[test]
fn test_rename_targets_value_not_trailing_comment() {
    // Regression: a use site whose line repeats the instance name in a trailing
    // comment must rename the VALUE (col 16), never the comment occurrence. The
    // old value-column scan took the LAST match on the raw line, so it wrote the
    // new text into the comment and left the real value dangling (silent
    // corruption). The value is resolved as the first token after the `=`.
    let files = &[
        ("common/national_focus/f.txt", "MY_FOCUS = { x = yes }\n"),
        (
            "common/decisions/a.txt",
            "adec = {\n    has_focus = MY_FOCUS\n}\n",
        ),
        (
            "common/decisions/b.txt",
            "bdec = {\n    has_focus = MY_FOCUS   # keep MY_FOCUS until 1939\n}\n",
        ),
    ];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["common/decisions/a.txt"],
        serde_json::json!({}),
        "common/decisions/a.txt",
        "textDocument/rename",
        serde_json::json!({ "position": { "line": 1, "character": 16 }, "newName": "NEW_FOCUS" }),
    );
    let changes = result["changes"]
        .as_object()
        .expect("WorkspaceEdit changes");
    let b_key = changes
        .keys()
        .find(|u| u.ends_with("decisions/b.txt"))
        .unwrap_or_else(|| panic!("rename must edit closed file b.txt, got: {}", result));
    let edits = changes[b_key].as_array().expect("edits for b.txt");
    assert_eq!(
        edits[0]["range"]["start"]["character"], 16,
        "edit must target the value column, not the trailing comment, got: {}",
        result
    );
    assert_eq!(edits[0]["newText"], "NEW_FOCUS");
}

// ── Phase A0: MD-scale completion baseline (ignored, manual) ─────────────────
//
// Not a correctness test — spawns the real server against a full Millennium
// Dawn checkout (plus the real hoi4 rules and, if present, a vanilla HOI4
// install) and prints the `cwtools_completion` summary line for three
// representative cursor positions, fired twice each (cold/warm). Run with:
//
//   cargo test --release -p cwtools_lsp perf_completion_md -- --ignored --nocapture
//
// Paths default to this machine's checkouts; override with CWTOOLS_PERF_MOD /
// CWTOOLS_PERF_VANILLA / CWTOOLS_PERF_RULES. Skips (does not fail) when the mod
// dir isn't present, so it's harmless in CI.

/// `~/rest` → `$HOME/rest`; anything else is returned unchanged.
fn perf_expand_tilde(path: &str) -> std::path::PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => std::path::Path::new(&home).join(rest),
            None => std::path::PathBuf::from(path),
        },
        None => std::path::PathBuf::from(path),
    }
}

/// Like `wait_for_scan_done`, but bounded by wall-clock time instead of an
/// iteration count. MD's workspace scan publishes one diagnostics notification
/// per file (7000+) before the closing `loadingBar`, so a small fixed loop
/// either times out early or runs out mid-scan; a real mod + vanilla install
/// can take tens of seconds to a few minutes.
fn perf_wait_for_scan_done(
    reader: &mut BufReader<std::process::ChildStdout>,
    timeout: std::time::Duration,
) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if std::time::Instant::now() > deadline {
            panic!("workspace scan did not finish within {:?}", timeout);
        }
        let Ok(raw) = read_frame(reader) else {
            panic!("server closed stdout before the workspace scan finished");
        };
        if raw.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw)
            && v["method"] == "loadingBar"
            && v["params"]["enable"] == serde_json::Value::Bool(false)
        {
            return;
        }
    }
}

/// Strip ANSI SGR escapes (`\x1b[...m`). The plain `RUST_LOG` (non-profile)
/// path doesn't disable color on the fmt subscriber, and a piped stderr still
/// gets them here, so the summary line arrives as e.g.
/// `\x1b[3mtotal_us\x1b[2m=\x1b[0m161 ...`.
fn perf_strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c2 in chars.by_ref() {
                if c2 == 'm' {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Parse one `cwtools_completion` summary line (see
/// `log_completion_summary` in `completion/mod.rs`) into its `key=value`
/// fields. Tolerant of whatever the subscriber puts before the fields
/// (timestamp, level, target) since only whitespace-separated `k=v` tokens
/// are taken; returns `None` for lines that aren't a summary line.
fn perf_parse_summary(line: &str) -> Option<std::collections::HashMap<String, String>> {
    let line = perf_strip_ansi(line);
    let mut map = std::collections::HashMap::new();
    for tok in line.split_whitespace() {
        if let Some((k, v)) = tok.split_once('=') {
            map.insert(k.to_string(), v.trim_matches('"').to_string());
        }
    }
    map.contains_key("total_us").then_some(map)
}

#[test]
#[ignore]
fn perf_completion_md() {
    let mod_dir = perf_expand_tilde(
        &std::env::var("CWTOOLS_PERF_MOD")
            .unwrap_or_else(|_| "~/Documents/github-projects/Millennium-Dawn".to_string()),
    );
    let mod_path = mod_dir.clone();
    if !mod_path.is_dir() {
        eprintln!(
            "perf_completion_md: skipping, mod dir not found: {}",
            mod_dir.display()
        );
        return;
    }

    let vanilla_dir =
        perf_expand_tilde(&std::env::var("CWTOOLS_PERF_VANILLA").unwrap_or_else(|_| {
            "~/.local/share/Steam/steamapps/common/Hearts of Iron IV".to_string()
        }));
    let rules_repo = perf_expand_tilde(
        &std::env::var("CWTOOLS_PERF_RULES")
            .unwrap_or_else(|_| "~/Documents/github-projects/cwtools-hoi4-config".to_string()),
    );
    // The repo stores the raw `.cwt` files under `Config/`, not at the repo
    // root — matches how `rulesCache` is consumed in config.rs.
    let rules_dir = std::path::PathBuf::from(&rules_repo).join("Config");

    let mut init_opts = serde_json::json!({ "language": "hoi4" });
    if rules_dir.is_dir() {
        init_opts["rulesCache"] = serde_json::json!(rules_dir.to_string_lossy());
    } else {
        eprintln!(
            "perf_completion_md: rules dir not found: {} (context-aware completion will be empty)",
            rules_dir.display()
        );
    }
    if vanilla_dir.is_dir() {
        init_opts["vanilla"] = serde_json::json!(vanilla_dir.to_string_lossy());
    } else {
        eprintln!(
            "perf_completion_md: vanilla dir not found: {} (base-game references won't resolve)",
            vanilla_dir.display()
        );
    }
    let cache_dir = tempfile::tempdir().unwrap();
    init_opts["cacheDir"] = serde_json::json!(cache_dir.path().to_string_lossy());

    let ws_uri = path_uri(&mod_path);

    let mut cmd = cwtools_server_cmd();
    cmd.env("RUST_LOG", "cwtools_completion=info");
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn cwtools-server");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    // Drain stderr on a background thread so a full run's worth of tracing
    // output can't fill the pipe and stall the server; collected lines are
    // parsed for the `cwtools_completion` summaries once the run is done.
    let stderr = child.stderr.take().unwrap();
    let stderr_lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let stderr_lines_bg = stderr_lines.clone();
    let stderr_thread = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            stderr_lines_bg.lock().unwrap().push(line);
        }
    });

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": init_opts,
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();

    eprintln!("perf_completion_md: waiting for workspace scan to finish...");
    perf_wait_for_scan_done(&mut reader, std::time::Duration::from_secs(600));
    eprintln!("perf_completion_md: workspace scan done, firing completions");

    // (relative path, 0-based line, 0-based char, label) — see the branch
    // description for how each position was picked against a real MD file.
    let positions: [(&str, u32, u32, &str); 4] = [
        // Inside a small focus's block: cursor on the `cost` key column, a
        // sibling-key context resolving through `completions_from_rules`.
        (
            "common/national_focus/eritrea_puppet.txt",
            22,
            2,
            "block-key (focus)",
        ),
        // `add_state_core = 282`: cursor mid-value on a `<state>` reference,
        // resolving through `value_completions` against every state instance.
        (
            "common/national_focus/05_botswana.txt",
            1032,
            21,
            "state-ref (value)",
        ),
        // Cursor on the root key of a file under a path no type covers
        // (`common/technology_tags` has no `type[...]` path in
        // cwtools-hoi4-config): root_type_snippets is empty, so this falls
        // through to the flat variable/event-target fallback.
        (
            "common/technology_tags/00_technology.txt",
            3,
            0,
            "flat-fallback",
        ),
        // `add_dynamic_modifier` key inside `completion_reward = { ... }`: an
        // effect-alias key context resolving through `completions_from_rules`
        // (the `alias/effect/trigger ### docs` category). At MD's scale this
        // list is dominated by `<scripted_effect>` pattern-expanded instances
        // (thousands of them, never carrying docs either way) once capped/
        // sorted to CONTEXT_CAP, so `bytes` here doesn't move — the doc
        // deferral's payload win shows up whenever the returned list is
        // mostly genuinely-named aliases instead (see the branch description
        // for the controlled measurement against cwtools-hoi4-config's docs).
        (
            "common/national_focus/03_benelux_shared.txt",
            23,
            2,
            "effect-alias (key)",
        ),
    ];

    let mut next_id = 10i64;
    // Serialized response size per request, in request order (1:1 with the
    // `cwtools_completion` summaries collected below) — the payload-shrink
    // half of the completionItem/resolve deferral isn't visible in
    // total_us/build_us (the docs were strings, not compute), so measure it
    // directly.
    let mut response_bytes: Vec<usize> = Vec::new();
    for (rel_path, line0, char0, label) in positions {
        let file_path = mod_path.join(rel_path);
        if !file_path.is_file() {
            eprintln!(
                "perf_completion_md: skipping missing sample file {}",
                file_path.display()
            );
            continue;
        }
        let text = std::fs::read_to_string(&file_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", file_path.display(), e));
        let doc_uri = path_uri(&file_path);
        write_frame(
            &mut child,
            &jsonrpc_notification(
                "textDocument/didOpen",
                serde_json::json!({
                    "textDocument": {
                        "uri": doc_uri,
                        "languageId": "hoi4",
                        "version": 1,
                        "text": text,
                    }
                }),
            ),
        )
        .unwrap();
        wait_for_diagnostics(&mut reader, rel_path);

        for pass_label in ["cold", "warm"] {
            let id = next_id;
            next_id += 1;
            let body = jsonrpc_request(
                id,
                "textDocument/completion",
                serde_json::json!({
                    "textDocument": { "uri": doc_uri },
                    "position": { "line": line0, "character": char0 },
                }),
            );
            write_frame(&mut child, &body).unwrap();
            let resp_str = read_response(&mut reader).expect("no completion response");
            response_bytes.push(resp_str.len());
            let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
            assert_eq!(resp["id"], id, "{} {} got: {}", label, pass_label, resp_str);
        }
    }

    write_frame(
        &mut child,
        &jsonrpc_request(999, "shutdown", serde_json::json!(null)),
    )
    .unwrap();
    let _ = read_response(&mut reader);
    child.kill().ok();
    stderr_thread.join().ok();

    let lines = stderr_lines.lock().unwrap();
    let summaries: Vec<_> = lines.iter().filter_map(|l| perf_parse_summary(l)).collect();

    println!(
        "\n{:<22} {:<6} {:>10} {:>10} {:>7} {:>9} {:<10} {:<10} {:<10}",
        "position",
        "pass",
        "total_us",
        "build_us",
        "items",
        "bytes",
        "path",
        "strategy",
        "incomplete"
    );
    let labels: Vec<&str> = positions.iter().map(|(_, _, _, label)| *label).collect();
    let passes = ["cold", "warm"];
    // Each didOpen fires two completion requests; the summaries appear in
    // request order, so pair them off positionally (labels × cold/warm).
    for (i, summary) in summaries.iter().enumerate() {
        let label = labels.get(i / 2).copied().unwrap_or("?");
        let pass = passes.get(i % 2).copied().unwrap_or("?");
        let bytes = response_bytes
            .get(i)
            .map(ToString::to_string)
            .unwrap_or_else(|| "?".to_string());
        println!(
            "{:<22} {:<6} {:>10} {:>10} {:>7} {:>9} {:<10} {:<10} {:<10}",
            label,
            pass,
            summary.get("total_us").map(String::as_str).unwrap_or("?"),
            summary.get("build_us").map(String::as_str).unwrap_or("?"),
            summary.get("items").map(String::as_str).unwrap_or("?"),
            bytes,
            summary.get("path").map(String::as_str).unwrap_or("?"),
            summary.get("strategy").map(String::as_str).unwrap_or("?"),
            summary.get("incomplete").map(String::as_str).unwrap_or("?"),
        );
    }
    assert!(
        !summaries.is_empty(),
        "expected at least one cwtools_completion summary line in stderr"
    );
}

#[test]
fn test_rescan_prunes_deleted_file_from_index() {
    // Definition file A holds scripted_effect my_se; caller B references it.
    // A is deleted from disk (no watcher event — e.g. deleted while the server
    // wasn't watching, or the file was never open) and a rescan is forced via
    // `clearAllCaches`. The rescan re-indexes what's still on disk (just B) but
    // must also PRUNE A's now-stale entries, or my_se keeps "resolving" forever
    // and B's CW263 never comes back.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap(); // empty dir → index marked complete
    let cache_dir = tempfile::tempdir().unwrap(); // isolate clearAllCaches from the real cache
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let a_rel = "common/scripted_effects/a.txt";
    let a_path = ws.path().join(a_rel);
    std::fs::create_dir_all(a_path.parent().unwrap()).unwrap();
    std::fs::write(&a_path, "my_se = { log = \"hi\" }\n").unwrap();

    let b_rel = "common/decisions/b.txt";
    let b_path = ws.path().join(b_rel);
    std::fs::create_dir_all(b_path.parent().unwrap()).unwrap();
    std::fs::write(
        &b_path,
        "my_dec = {\n    complete_effect = {\n        my_se = yes\n    }\n}\n",
    )
    .unwrap();

    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "vanilla": vanilla.path().to_string_lossy(),
                "cacheDir": cache_dir.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &init).unwrap();
    let _ = read_response(&mut reader);
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();

    // Both files exist on disk for the initial scan, so my_se resolves.
    let before = diags_for(&mut reader, "b.txt", 1).expect("B diagnostics before delete");
    assert!(
        !before.contains(&"CW263".to_string()),
        "expected my_se to resolve while A exists, got: {:?}",
        before
    );

    // Delete the definition, then force a rescan (no file watcher in this test).
    std::fs::remove_file(&a_path).unwrap();
    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "workspace/executeCommand",
            serde_json::json!({"command": "clearAllCaches", "arguments": []}),
        ),
    )
    .unwrap();

    let after = diags_for(&mut reader, "b.txt", 1).expect("B diagnostics after rescan");
    child.kill().ok();
    assert!(
        after.contains(&"CW263".to_string()),
        "deleting A should resurrect B's CW263 once the rescan prunes it, got: {:?}",
        after
    );
}

/// Read frames until the response to request `id` arrives, returning its
/// `result`. Notifications and server-initiated requests are skipped.
fn result_for(
    reader: &mut BufReader<std::process::ChildStdout>,
    id: i64,
) -> Option<serde_json::Value> {
    for _ in 0..2000 {
        let raw = read_frame(reader).ok()?;
        if raw.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        if v["id"] == serde_json::json!(id) && v.get("result").is_some() {
            return Some(v["result"].clone());
        }
    }
    None
}

#[test]
fn test_clear_all_caches_only_deletes_cwtools_caches() {
    // #159: `cacheDir` is client input, and the purge used to recursively delete
    // `<cacheDir>/parse-cache` and every `vanilla-*` child by name. Point it at
    // a directory holding unrelated entries of both shapes: they must survive
    // the startup scan (which prunes) and the command (which purges), while the
    // genuine caches next to them are cleared.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();
    std::fs::write(ws.path().join("a.txt"), "my_dec = { }\n").unwrap();

    let foreign_dir = cache_dir.path().join("parse-cache").join("important");
    std::fs::create_dir_all(&foreign_dir).unwrap();
    let foreign_file = foreign_dir.join("notes.txt");
    std::fs::write(&foreign_file, b"keep me").unwrap();
    let foreign_named = cache_dir.path().join("vanilla-notes.txt");
    std::fs::write(&foreign_named, b"keep me").unwrap();
    let foreign_cwv = cache_dir.path().join("vanilla-holiday.cwv");
    std::fs::write(&foreign_cwv, b"keep me").unwrap();

    // Genuine caches, in the on-disk shape the engine writes: a fingerprint-named
    // directory with its 8-byte signature and an entry, and a `.cwv` carrying the
    // vanilla-cache header.
    let owned_dir = cache_dir
        .path()
        .join("parse-cache")
        .join("0123456789abcdef");
    std::fs::create_dir_all(&owned_dir).unwrap();
    std::fs::write(owned_dir.join("settings.sig"), 1u64.to_le_bytes()).unwrap();
    std::fs::write(owned_dir.join("0000000000000001.cwb"), b"CWB\0").unwrap();
    let owned_cwv = cache_dir.path().join("vanilla-hoi4-test.cwv");
    std::fs::write(&owned_cwv, b"CWV\0\x0a").unwrap();

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": path_uri(ws.path()),
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "vanilla": vanilla.path().to_string_lossy(),
                "cacheDir": cache_dir.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &init).unwrap();
    let _ = read_response(&mut reader);
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_diagnostics(&mut reader, "a.txt");

    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "workspace/executeCommand",
            serde_json::json!({"command": "clearAllCaches", "arguments": []}),
        ),
    )
    .unwrap();
    let result = result_for(&mut reader, 2).expect("clearAllCaches result");
    child.kill().ok();

    let message = result.as_str().unwrap_or_default();
    assert!(
        message.starts_with("Caches cleared"),
        "unexpected result: {message}"
    );
    assert_eq!(
        std::fs::read(&foreign_file).unwrap(),
        b"keep me",
        "an unrelated file under a `parse-cache` directory was deleted"
    );
    assert!(foreign_named.exists(), "vanilla-notes.txt was deleted");
    assert!(foreign_cwv.exists(), "a headerless .cwv was deleted");
    assert!(!owned_dir.exists(), "the real parse cache survived");
    assert!(!owned_cwv.exists(), "the real vanilla cache survived");
}

// ── Periodic background reindex ───────────────────────────────────────────

/// Read frames until a `publishDiagnostics` for a URI ending in `suffix`
/// arrives whose codes no longer include `missing_code`. Fails immediately if
/// a `loadingBar` notification is observed along the way — a quiet background
/// pass must never touch the status bar, unlike the startup scan or
/// `clearAllCaches`. Returns `Err` on a stray loadingBar or on timeout.
fn wait_for_cleared_diag_quiet(
    reader: &mut BufReader<std::process::ChildStdout>,
    suffix: &str,
    missing_code: &str,
) -> Result<(), String> {
    for _ in 0..10_000 {
        let raw = read_frame(reader).map_err(|e| format!("read error: {e}"))?;
        if raw.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        if v["method"] == "loadingBar" {
            return Err(format!(
                "unexpected loadingBar notification during quiet background pass: {v}"
            ));
        }
        if v["method"] == "textDocument/publishDiagnostics"
            && v["params"]["uri"]
                .as_str()
                .is_some_and(|u| u.ends_with(suffix))
        {
            let codes: Vec<String> = v["params"]["diagnostics"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|d| d["code"].as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            if !codes.iter().any(|c| c == missing_code) {
                return Ok(());
            }
        }
    }
    Err(format!(
        "timed out waiting for {suffix} diagnostics without {missing_code}"
    ))
}

/// [`wait_for_cleared_diag_quiet`], but pulls frames from a
/// `spawn_frame_collector` channel under an explicit `budget` instead of
/// blocking a raw reader for an unbounded (but not time-bounded) number of
/// frames. Used where a test also wants `fetch_profiling_log` afterward
/// (which needs the same channel) and where a stuck condition should read as
/// a test failure, not a hung CI job.
fn wait_for_cleared_diag_with_deadline(
    rx: &std::sync::mpsc::Receiver<serde_json::Value>,
    suffix: &str,
    missing_code: &str,
    budget: std::time::Duration,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + budget;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "timed out waiting for {suffix} diagnostics without {missing_code}"
            ));
        }
        let Ok(v) = rx.recv_timeout(remaining) else {
            return Err(format!(
                "timed out waiting for {suffix} diagnostics without {missing_code}"
            ));
        };
        if v["method"] == "loadingBar" {
            return Err(format!(
                "unexpected loadingBar notification during quiet background pass: {v}"
            ));
        }
        if v["method"] == "textDocument/publishDiagnostics"
            && v["params"]["uri"]
                .as_str()
                .is_some_and(|u| u.ends_with(suffix))
        {
            let codes: Vec<String> = v["params"]["diagnostics"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|d| d["code"].as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            if !codes.iter().any(|c| c == missing_code) {
                return Ok(());
            }
        }
    }
}

#[test]
fn test_background_reindex_picks_up_new_file_quietly() {
    // The periodic background pass (CWTOOLS_REINDEX_INTERVAL_SECS=1,
    // CWTOOLS_REINDEX_IDLE_SECS=0 so it fires almost immediately once the
    // interval elapses) must discover a file created directly on disk — no
    // didOpen, no didChangeWatchedFiles notification over stdio — the same
    // way a real file-watcher gap would (a git checkout that raced the
    // watcher, or a file appearing while the window had no focus). It must
    // also run quiet: unlike the startup scan or `clearAllCaches`, no
    // loadingBar notification should reach the client.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap(); // empty dir → index marked complete
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    // Only the caller exists on disk at first; the definition is added later,
    // directly on disk, simulating a watcher-missed change.
    let b_rel = "common/decisions/b.txt";
    let b_path = ws.path().join(b_rel);
    std::fs::create_dir_all(b_path.parent().unwrap()).unwrap();
    std::fs::write(
        &b_path,
        "my_dec = {\n    complete_effect = {\n        my_se = yes\n    }\n}\n",
    )
    .unwrap();

    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .env("CWTOOLS_REINDEX_INTERVAL_SECS", "1")
        .env("CWTOOLS_REINDEX_IDLE_SECS", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "vanilla": vanilla.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &init).unwrap();
    let _ = read_response(&mut reader);
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    // The startup scan runs non-quiet; drain its loadingBar traffic before the
    // quiet-observation window starts.
    wait_for_scan_done(&mut reader);

    // Open the caller; the definition is absent, so B shows CW263.
    let b_uri = path_uri(&b_path);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":b_uri,"languageId":"hoi4","version":1,
                "text":"my_dec = {\n    complete_effect = {\n        my_se = yes\n    }\n}\n"}}),
        ),
    )
    .unwrap();
    let before = diags_for(&mut reader, "b.txt", 1).expect("B diagnostics before background pass");
    assert!(
        before.contains(&"CW263".to_string()),
        "expected CW263 before the definition exists, got: {:?}",
        before
    );

    // Create the defining file directly on disk — no didOpen, no
    // didChangeWatchedFiles notification. Only the periodic background
    // pass's own filesystem walk can find it.
    let a_rel = "common/scripted_effects/a.txt";
    let a_path = ws.path().join(a_rel);
    std::fs::create_dir_all(a_path.parent().unwrap()).unwrap();
    std::fs::write(&a_path, "my_se = { log = \"hi\" }\n").unwrap();

    let result = wait_for_cleared_diag_quiet(&mut reader, "b.txt", "CW263");
    child.kill().ok();
    if let Err(e) = result {
        panic!("{e}");
    }
}

#[test]
fn test_background_reindex_survives_a_panicking_pass() {
    // #155: CWTOOLS_REINDEX_PANIC_ONCE makes the FIRST background pass panic
    // before it ever scans. `run_reindex_pass`'s wrapper must log-and-swallow
    // that panic and let `background_reindex_loop`'s own task keep going, so
    // the SECOND pass (one interval later) still discovers the file — same
    // setup as test_background_reindex_picks_up_new_file_quietly, but this
    // proves the loop survives one bad pass instead of just that it works.
    // Goes through `storm_server_env` (CWTOOLS_PROFILE=1) and asserts on the
    // profiling log so a build where the injection never actually fired
    // (env var drift, the injection point moved) fails here instead of
    // passing for the wrong reason — every other assertion below is equally
    // true of a pass that never panicked.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap(); // empty dir → index marked complete
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    // Only the caller exists on disk at first; the definition is added later,
    // directly on disk, simulating a watcher-missed change.
    let b_rel = "common/decisions/b.txt";
    let b_path = ws.path().join(b_rel);
    std::fs::create_dir_all(b_path.parent().unwrap()).unwrap();
    std::fs::write(
        &b_path,
        "my_dec = {\n    complete_effect = {\n        my_se = yes\n    }\n}\n",
    )
    .unwrap();

    let (mut child, mut reader) = storm_server_env(
        ws.path(),
        rules_dir.path(),
        vanilla.path(),
        &[
            ("CWTOOLS_REINDEX_INTERVAL_SECS", "1"),
            ("CWTOOLS_REINDEX_IDLE_SECS", "0"),
            ("CWTOOLS_REINDEX_PANIC_ONCE", "1"),
        ],
    );

    // Open the caller; the definition is absent, so B shows CW263.
    let b_uri = path_uri(&b_path);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":b_uri,"languageId":"hoi4","version":1,
                "text":"my_dec = {\n    complete_effect = {\n        my_se = yes\n    }\n}\n"}}),
        ),
    )
    .unwrap();
    let before = diags_for(&mut reader, "b.txt", 1).expect("B diagnostics before background pass");
    assert!(
        before.contains(&"CW263".to_string()),
        "expected CW263 before the definition exists, got: {:?}",
        before
    );

    // Create the defining file directly on disk — no didOpen, no
    // didChangeWatchedFiles notification. Only the periodic background
    // pass's own filesystem walk can find it.
    let a_rel = "common/scripted_effects/a.txt";
    let a_path = ws.path().join(a_rel);
    std::fs::create_dir_all(a_path.parent().unwrap()).unwrap();
    std::fs::write(&a_path, "my_se = { log = \"hi\" }\n").unwrap();

    let rx = spawn_frame_collector(reader);

    // The first pass (interval #1) panics and is swallowed; only the second
    // pass (interval #2, ~1s later) actually scans and finds a.txt. A 15s
    // budget comfortably covers both 1s intervals plus scan time — and turns
    // a stuck condition into a test failure instead of a hung CI job.
    let result = wait_for_cleared_diag_with_deadline(
        &rx,
        "b.txt",
        "CW263",
        std::time::Duration::from_secs(15),
    );
    let log = fetch_profiling_log(&mut child, &rx, 2001);
    child.kill().ok();

    if let Err(e) = result {
        panic!("{e}");
    }
    assert!(
        log.contains("background reindex pass panicked"),
        "expected the swallowed panic to be logged, got: {log}"
    );
}

#[test]
fn test_background_reindex_idle_window_from_init_option() {
    // `backgroundReindexIdleSeconds` in initializationOptions must drive the
    // idle gate — here 0, so the pass fires as soon as the 1s interval
    // elapses — with no CWTOOLS_REINDEX_IDLE_SECS test override in play.
    // Same shape as test_background_reindex_picks_up_new_file_quietly, but
    // deadline-bounded: if the option were ignored, the built-in 15s idle
    // window would blow the 10s budget.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap(); // empty dir → index marked complete
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let b_rel = "common/decisions/b.txt";
    let b_path = ws.path().join(b_rel);
    std::fs::create_dir_all(b_path.parent().unwrap()).unwrap();
    std::fs::write(
        &b_path,
        "my_dec = {\n    complete_effect = {\n        my_se = yes\n    }\n}\n",
    )
    .unwrap();

    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .env("CWTOOLS_REINDEX_INTERVAL_SECS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "vanilla": vanilla.path().to_string_lossy(),
                "backgroundReindexIdleSeconds": 0,
            }
        }),
    );
    write_frame(&mut child, &init).unwrap();
    let _ = read_response(&mut reader);
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    let b_uri = path_uri(&b_path);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":b_uri,"languageId":"hoi4","version":1,
                "text":"my_dec = {\n    complete_effect = {\n        my_se = yes\n    }\n}\n"}}),
        ),
    )
    .unwrap();
    let before = diags_for(&mut reader, "b.txt", 1).expect("B diagnostics before background pass");
    assert!(
        before.contains(&"CW263".to_string()),
        "expected CW263 before the definition exists, got: {:?}",
        before
    );

    let a_path = ws.path().join("common/scripted_effects/a.txt");
    std::fs::create_dir_all(a_path.parent().unwrap()).unwrap();
    std::fs::write(&a_path, "my_se = { log = \"hi\" }\n").unwrap();

    let rx = spawn_frame_collector(reader);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut cleared = false;
    while std::time::Instant::now() < deadline {
        let Ok(v) = rx.recv_timeout(std::time::Duration::from_millis(200)) else {
            continue;
        };
        if v["method"] == "loadingBar" {
            child.kill().ok();
            panic!("unexpected loadingBar during quiet background pass: {v}");
        }
        if v["method"] == "textDocument/publishDiagnostics"
            && v["params"]["uri"]
                .as_str()
                .is_some_and(|u| u.ends_with("b.txt"))
        {
            let codes: Vec<&str> = v["params"]["diagnostics"]
                .as_array()
                .map(|a| a.iter().filter_map(|d| d["code"].as_str()).collect())
                .unwrap_or_default();
            if !codes.contains(&"CW263") {
                cleared = true;
                break;
            }
        }
    }
    child.kill().ok();
    assert!(
        cleared,
        "config-driven idle window of 0s should let the background pass clear CW263 within 10s"
    );
}

#[test]
fn test_clear_all_caches_reports_reindexed_message() {
    // clearAllCaches purges the caches then re-indexes. With no competing scan
    // it wins the CAS on the first try, so the honest-reporting refactor (fix 1)
    // must still surface the success message — not silently no-op. The rescan
    // itself is covered by test_rescan_prunes_deleted_file_from_index; this
    // pins the returned status string.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap(); // empty dir → index marked complete
    let cache_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let seed_rel = "common/decisions/b.txt";
    let seed_path = ws.path().join(seed_rel);
    std::fs::create_dir_all(seed_path.parent().unwrap()).unwrap();
    std::fs::write(&seed_path, "my_dec = {\n}\n").unwrap();

    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "vanilla": vanilla.path().to_string_lossy(),
                "cacheDir": cache_dir.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &init).unwrap();
    let _ = read_response(&mut reader);
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "workspace/executeCommand",
            serde_json::json!({"command": "clearAllCaches", "arguments": []}),
        ),
    )
    .unwrap();
    let resp_str = read_response(&mut reader).expect("no clearAllCaches response");
    child.kill().ok();
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(resp["id"], 2, "got: {}", resp_str);
    // The file count in the middle depends on what the scan cached, so the two
    // halves that carry meaning are matched instead.
    let result = resp["result"].as_str().unwrap_or_default();
    assert!(
        result.starts_with("Caches cleared (") && result.ends_with("workspace re-indexed."),
        "clearAllCaches should report a successful re-index, got: {}",
        resp_str
    );
}

// ── #90: validate-storm coalescing (watched files, config, open/save) ────────

/// A game file that validates under GOTO_RULES (references an undefined focus,
/// so it produces one diagnostic). The storm tests count `[validate]` /
/// publishDiagnostics frames, so its exact content only matters where a test
/// checks for a non-empty diagnostic (the open-then-close race test).
const STORM_FILE: &str = "my_dec = {\n    has_focus = my_focus\n}\n";

/// Spin up a server on `ws` with GOTO_RULES and an empty vanilla, run the init
/// handshake, and wait for the startup scan so `index_ready` is set and
/// per-file validation publishes (and logs). Returns the child and its reader.
fn storm_server(
    ws: &std::path::Path,
    rules_dir: &std::path::Path,
    vanilla: &std::path::Path,
) -> (std::process::Child, BufReader<std::process::ChildStdout>) {
    storm_server_env(ws, rules_dir, vanilla, &[])
}

/// [`storm_server`] with extra environment variables (test-only server knobs
/// like `CWTOOLS_SCAN_HOLD_MS`).
fn storm_server_env(
    ws: &std::path::Path,
    rules_dir: &std::path::Path,
    vanilla: &std::path::Path,
    extra_env: &[(&str, &str)],
) -> (std::process::Child, BufReader<std::process::ChildStdout>) {
    let ws_uri = path_uri(ws);
    let mut cmd = cwtools_server_cmd();
    // The `[validate]` line is a tracing event now, read back via
    // exportProfilingLog. env_remove so init_tracing installs the `info`
    // filter instead of an empty RUST_LOG="" that would capture nothing.
    cmd.env_remove("RUST_LOG").env("CWTOOLS_PROFILE", "1");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.to_string_lossy(),
                "vanilla": vanilla.to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &init).unwrap();
    let _ = read_response(&mut reader);
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);
    (child, reader)
}

/// Move `reader` into a background thread that forwards every non-empty frame
/// onto a channel. The blocking reader can't detect "no more frames", so the
/// storm tests poll the channel with a timeout to observe a whole coalescing
/// window. Call after the init handshake (which must read responses directly).
fn spawn_frame_collector(
    mut reader: BufReader<std::process::ChildStdout>,
) -> std::sync::mpsc::Receiver<serde_json::Value> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        loop {
            match read_frame(&mut reader) {
                // An empty frame is EOF, not a frame to skip: `read_frame`
                // breaks its header loop on the empty line `read_line` returns
                // at end of stream, then returns early because it never saw a
                // Content-Length. `Err` is therefore never reached once the
                // server exits — and every one of these tests ends by killing
                // it. Continuing here spun the thread at 100% for the rest of
                // the process, and with 25 collector tests that saturated the
                // machine and starved whatever ran last.
                Ok(raw) if raw.is_empty() => break,
                Ok(raw) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw)
                        && tx.send(v).is_err()
                    {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

/// Collect frames from `rx` until none arrives for `quiet`, or `budget` elapses.
/// `quiet` must exceed the coalescing window so the drain doesn't stop before it
/// fires. Use this only where zero frames is a legitimate outcome (a no-op
/// guard that must not revalidate); anything asserting on what arrived wants
/// [`drain_after_first`].
fn drain_until_quiet(
    rx: &std::sync::mpsc::Receiver<serde_json::Value>,
    quiet: std::time::Duration,
    budget: std::time::Duration,
) -> Vec<serde_json::Value> {
    let start = std::time::Instant::now();
    let mut out = Vec::new();
    while start.elapsed() <= budget {
        match rx.recv_timeout(quiet) {
            Ok(v) => out.push(v),
            Err(_) => break,
        }
    }
    out
}

/// [`drain_until_quiet`], but the wait for the FIRST frame is the whole budget.
/// `quiet` bounds the gap between frames; spending it before anything has
/// arrived turns "how fast did the host answer" into "did the server do the
/// work", which is how these tests read as green on a dev machine and red on a
/// loaded CI runner that needs seconds to publish.
fn drain_after_first(
    rx: &std::sync::mpsc::Receiver<serde_json::Value>,
    quiet: std::time::Duration,
    budget: std::time::Duration,
) -> Vec<serde_json::Value> {
    let mut out = match rx.recv_timeout(budget) {
        Ok(v) => vec![v],
        Err(_) => return Vec::new(),
    };
    out.extend(drain_until_quiet(rx, quiet, budget));
    out
}

/// Fetch the server's in-memory profiling log (requires `CWTOOLS_PROFILE`,
/// which `storm_server` sets). Sends `exportProfilingLog` and reads its
/// response off the frame channel, skipping any publishes that arrive first.
/// The buffer is cumulative and non-draining, so callers compare CUMULATIVE
/// `[validate]` counts across successive windows.
fn fetch_profiling_log(
    child: &mut std::process::Child,
    rx: &std::sync::mpsc::Receiver<serde_json::Value>,
    id: i64,
) -> String {
    write_frame(
        child,
        &jsonrpc_request(
            id,
            "workspace/executeCommand",
            serde_json::json!({ "command": "exportProfilingLog", "arguments": [] }),
        ),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if let Ok(v) = rx.recv_timeout(std::time::Duration::from_millis(200))
            && v["id"] == id
        {
            return v["result"].as_str().unwrap_or("").to_string();
        }
    }
    panic!("no exportProfilingLog response for id {id}");
}

/// Count `[validate] (trigger)` lines in a fetched profiling log.
fn count_validate_log(log: &str, trigger: &str) -> usize {
    log.matches(&format!("[validate] ({trigger})")).count()
}

fn count_publishes(frames: &[serde_json::Value], suffix: &str) -> usize {
    frames
        .iter()
        .filter(|v| {
            v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with(suffix))
        })
        .count()
}

fn watched_changes(uris: &[String]) -> String {
    let changes: Vec<serde_json::Value> = uris
        .iter()
        .map(|u| serde_json::json!({ "uri": u, "type": 2 }))
        .collect();
    jsonrpc_notification(
        "workspace/didChangeWatchedFiles",
        serde_json::json!({ "changes": changes }),
    )
}

/// A `workspace/didChangeWatchedFiles` notification marking the URIs as newly
/// created (type 1). Distinct from `watched_changes` (type 2, CHANGED): the
/// loc-discovery cache is invalidated on create/delete but not on change.
fn watched_created(uris: &[String]) -> String {
    let changes: Vec<serde_json::Value> = uris
        .iter()
        .map(|u| serde_json::json!({ "uri": u, "type": 1 }))
        .collect();
    jsonrpc_notification(
        "workspace/didChangeWatchedFiles",
        serde_json::json!({ "changes": changes }),
    )
}

/// Write a decision file on disk (not opened) and return its `file://` URI.
fn write_disk_file(ws: &std::path::Path, rel: &str, content: &str) -> String {
    let path = ws.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
    path_uri(&path)
}

/// Write a BOM + `l_english:`-headed loc file on disk and return its URI.
fn write_loc_file(ws: &std::path::Path, rel: &str, body: &str) -> String {
    let path = ws.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(format!("l_english:\n{body}").as_bytes());
    std::fs::write(&path, &bytes).unwrap();
    path_uri(&path)
}

#[test]
fn test_watched_repeated_change_coalesces_to_one_validate() {
    // Ten CHANGED events for the same non-open file, each in its own
    // notification, must collapse into exactly one `(watched)` validation and
    // one publish — the 1:1 amplification that drove the #90 storm is gone.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let (mut child, reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());
    let uri = write_disk_file(ws.path(), "common/decisions/a.txt", STORM_FILE);
    let rx = spawn_frame_collector(reader);

    for _ in 0..10 {
        write_frame(&mut child, &watched_changes(std::slice::from_ref(&uri))).unwrap();
    }

    let frames = drain_after_first(
        &rx,
        std::time::Duration::from_millis(1200),
        std::time::Duration::from_secs(8),
    );
    let log = fetch_profiling_log(&mut child, &rx, 1001);
    child.kill().ok();

    assert_eq!(
        count_validate_log(&log, "watched"),
        1,
        "10 repeated CHANGED events should coalesce to one validate"
    );
    assert_eq!(
        count_publishes(&frames, "a.txt"),
        1,
        "should publish diagnostics for a.txt exactly once"
    );
}

#[test]
fn test_watched_batch_panic_is_recovered_and_retried() {
    // #155: CWTOOLS_WATCHED_BATCH_PANIC_ONCE makes the FIRST watched batch
    // panic before it validates anything. spawn_watched_batch_window's
    // recovery path must log-and-swallow that panic, put the drained change
    // back on the queue, and arm a fresh window directly (not through
    // arm_watched_batch's is_finished() gate, which would see its own
    // not-yet-returned handle as still running and skip the retry) — so the
    // file still validates and publishes exactly once, just one debounce
    // window late, instead of being silently dropped.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let (mut child, reader) = storm_server_env(
        ws.path(),
        rules_dir.path(),
        vanilla.path(),
        &[("CWTOOLS_WATCHED_BATCH_PANIC_ONCE", "1")],
    );
    let uri = write_disk_file(ws.path(), "common/decisions/a.txt", STORM_FILE);
    let rx = spawn_frame_collector(reader);

    write_frame(&mut child, &watched_changes(std::slice::from_ref(&uri))).unwrap();

    let frames = drain_after_first(
        &rx,
        std::time::Duration::from_millis(1200),
        std::time::Duration::from_secs(8),
    );
    let log = fetch_profiling_log(&mut child, &rx, 1010);
    child.kill().ok();

    // Pin the precondition: without this, the test would pass just as well on
    // a build where the injection never actually fired (env var drift, the
    // injection point moved, the one-shot AtomicBool already consumed), since
    // every other assertion here is also true of a batch that never panicked.
    assert!(
        log.contains("watched batch panicked"),
        "expected the injected panic to be logged, got: {log}"
    );
    assert_eq!(
        count_validate_log(&log, "watched"),
        1,
        "the panicking first attempt must not double- or zero-validate after retry"
    );
    assert_eq!(
        count_publishes(&frames, "a.txt"),
        1,
        "a.txt should still be published exactly once despite the first batch panicking"
    );
}

#[test]
fn test_debounced_validate_panic_is_logged_and_next_edit_recovers() {
    // #182: CWTOOLS_VALIDATE_PANIC_ONCE makes the FIRST debounced validation
    // panic before it validates anything. The task joining the debounce handle
    // must log that panic instead of letting it vanish with the handle, and the
    // document must still validate on the next edit (the retry #182 relies on).
    // The tail of the test pins the converse: an edit superseded inside the
    // debounce window is aborted, and an abort is not a panic to log.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let (mut child, reader) = storm_server_env(
        ws.path(),
        rules_dir.path(),
        vanilla.path(),
        &[("CWTOOLS_VALIDATE_PANIC_ONCE", "1")],
    );
    let uri = write_disk_file(ws.path(), "common/decisions/a.txt", STORM_FILE);
    let rx = spawn_frame_collector(reader);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{
                "uri": uri, "languageId": "hoi4", "version": 1, "text": STORM_FILE}}),
        ),
    )
    .unwrap();
    let after_open = drain_until_quiet(
        &rx,
        std::time::Duration::from_millis(1200),
        std::time::Duration::from_secs(8),
    );
    assert_eq!(
        count_publishes(&after_open, "a.txt"),
        0,
        "the panicking open validation should not have published"
    );

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{"text": STORM_FILE}]}),
        ),
    )
    .unwrap();
    let after_edit = drain_after_first(
        &rx,
        std::time::Duration::from_millis(1200),
        std::time::Duration::from_secs(8),
    );

    // Two edits back to back, so the first is superseded inside the debounce
    // window and its task is aborted rather than left to finish. That abort is
    // the other half of the join's contract: it must not be reported as a
    // panic, or every keystroke would log one. Nothing else here would notice,
    // since the abort is invisible in the publishes.
    for version in [3, 4] {
        write_frame(
            &mut child,
            &jsonrpc_notification(
                "textDocument/didChange",
                serde_json::json!({
                    "textDocument": {"uri": uri, "version": version},
                    "contentChanges": [{"text": STORM_FILE}]}),
            ),
        )
        .unwrap();
    }
    drain_after_first(
        &rx,
        std::time::Duration::from_millis(1200),
        std::time::Duration::from_secs(8),
    );
    let log = fetch_profiling_log(&mut child, &rx, 1011);
    child.kill().ok();

    // Pin the precondition: without this the test passes just as well on a
    // build where the injection never fired, since a validation that simply
    // published late satisfies everything else here.
    assert!(
        log.contains("document validation task panicked"),
        "expected the panicking validation to be logged, got: {log}"
    );
    assert_eq!(
        log.matches("document validation task panicked").count(),
        1,
        "the superseded edit's abort must not be logged as a panic, got: {log}"
    );
    assert_eq!(
        count_publishes(&after_edit, "a.txt"),
        1,
        "the next edit should still validate and publish once"
    );
}

#[test]
fn test_watched_distinct_files_each_validate_once() {
    // A burst of distinct non-open files in one window: each validates exactly
    // once, all after a single coalescing window.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let (mut child, reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());
    let m = 8usize;
    let uris: Vec<String> = (0..m)
        .map(|i| write_disk_file(ws.path(), &format!("common/decisions/f{i}.txt"), STORM_FILE))
        .collect();
    let rx = spawn_frame_collector(reader);

    write_frame(&mut child, &watched_changes(&uris)).unwrap();

    let frames = drain_after_first(
        &rx,
        std::time::Duration::from_millis(1200),
        std::time::Duration::from_secs(10),
    );
    let log = fetch_profiling_log(&mut child, &rx, 1002);
    child.kill().ok();

    assert_eq!(
        count_validate_log(&log, "watched"),
        m,
        "each of {m} distinct files should validate exactly once"
    );
    for i in 0..m {
        assert_eq!(
            count_publishes(&frames, &format!("f{i}.txt")),
            1,
            "f{i}.txt should be published exactly once"
        );
    }
}

/// #177: a watched event is client-supplied, so it goes through the same access
/// boundary as a request URI, and that boundary now applies the discovery walks'
/// symlink rule. The link here resolves back inside the workspace, so canonical
/// containment alone would wave it through: before the boundary tested the leaf
/// itself, touching it read, parsed, INDEXED and published diagnostics for a
/// file the startup scan had deliberately skipped (#161), which is also the file
/// `fixAllWorkspace` refuses to touch.
#[cfg(unix)]
#[test]
fn test_watched_change_on_a_symlink_neither_validates_nor_publishes() {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let (mut child, reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());
    let real_uri = write_disk_file(ws.path(), "common/decisions/real.txt", STORM_FILE);
    let link = ws.path().join("common/decisions/linked.txt");
    std::os::unix::fs::symlink(link.with_file_name("real.txt"), &link).unwrap();
    let link_uri = path_uri(&link);
    let rx = spawn_frame_collector(reader);

    write_frame(&mut child, &watched_changes(&[real_uri, link_uri])).unwrap();

    let frames = drain_after_first(
        &rx,
        std::time::Duration::from_millis(1200),
        std::time::Duration::from_secs(10),
    );
    let log = fetch_profiling_log(&mut child, &rx, 1003);
    child.kill().ok();

    // The real file is the precondition: without it a boundary that refused
    // everything would pass this test too.
    assert_eq!(
        count_publishes(&frames, "real.txt"),
        1,
        "the real file behind the link must still validate and publish"
    );
    assert_eq!(
        count_validate_log(&log, "watched"),
        1,
        "only the real file may validate, got: {log}"
    );
    assert_eq!(
        count_publishes(&frames, "linked.txt"),
        0,
        "the symlink must publish nothing, got: {frames:?}"
    );
}

#[test]
fn test_watched_bulk_flood_uses_rescan_not_per_file() {
    // More than WATCHED_BULK_CAP (200) distinct CHANGED events collapse into a
    // single workspace rescan (a rules re-clone / git checkout), so there are
    // zero per-file `(watched)` validations — the direct #90 amplification kill.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let (mut child, reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());
    let n = 205usize;
    let uris: Vec<String> = (0..n)
        .map(|i| write_disk_file(ws.path(), &format!("common/decisions/b{i}.txt"), STORM_FILE))
        .collect();
    let rx = spawn_frame_collector(reader);

    write_frame(&mut child, &watched_changes(&uris)).unwrap();

    let frames = drain_after_first(
        &rx,
        std::time::Duration::from_millis(1500),
        std::time::Duration::from_secs(20),
    );
    let log = fetch_profiling_log(&mut child, &rx, 1003);
    child.kill().ok();

    assert_eq!(
        count_validate_log(&log, "watched"),
        0,
        "an over-cap flood must not run any per-file watched validations"
    );
    let publishes = frames
        .iter()
        .filter(|v| v["method"] == "textDocument/publishDiagnostics")
        .count();
    assert!(
        publishes > 0,
        "the bulk rescan should still republish workspace diagnostics"
    );
}

#[test]
fn test_watched_overcap_batch_does_not_spin_against_running_scan() {
    // An over-cap batch that loses the rescan CAS to an in-flight scan used to
    // requeue and immediately re-arm, retrying (and logging "over cap") every
    // 500ms window for the scan's whole duration (#90 residual). It must park
    // the requeue for the scan winner to drain once: one over-cap line for the
    // losing drain, one for the winner-armed drain that runs the real rescan.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    // Hold every scan open for 4s so the watched drain reliably lands mid-scan.
    let (mut child, reader) = storm_server_env(
        ws.path(),
        rules_dir.path(),
        vanilla.path(),
        &[("CWTOOLS_SCAN_HOLD_MS", "4000")],
    );
    let n = 205usize;
    let uris: Vec<String> = (0..n)
        .map(|i| write_disk_file(ws.path(), &format!("common/decisions/s{i}.txt"), STORM_FILE))
        .collect();
    let rx = spawn_frame_collector(reader);

    // Start a scan, then flood while it holds the CAS. The hold begins after
    // the bar-on, so waiting for that signal keeps the flood's debounce window
    // from slipping past the scan and winning the CAS.
    reindex_until_scan_starts(&mut child, &rx);
    write_frame(&mut child, &watched_changes(&uris)).unwrap();

    // Quiet must outlast the held rescan so the whole story is observed.
    let _ = drain_until_quiet(
        &rx,
        std::time::Duration::from_millis(5500),
        std::time::Duration::from_secs(30),
    );
    let log = fetch_profiling_log(&mut child, &rx, 1004);
    child.kill().ok();

    assert_eq!(
        log.matches("watched batch over cap").count(),
        2,
        "a lost-CAS over-cap batch must be drained exactly once after the scan \
         (loser + winner-armed retry), not retried every window"
    );
    assert_eq!(
        count_validate_log(&log, "watched"),
        0,
        "the requeued flood must still collapse into a rescan, not per-file validations"
    );
}

#[test]
fn test_watched_loc_value_change_does_not_sweep_open_loc_files() {
    // A watched change to a NON-open loc file must stay out of the live overlay
    // (that map is open-doc state): before this fix its first sight treated the
    // whole file as changed and swept every other open loc file on EVERY event.
    // First sight seeds the watched overlay and may sweep once; a repeat change
    // that leaves the key set unchanged must not sweep at all.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();
    let watched_uri = write_loc_file(
        ws.path(),
        "localisation/watched_l_english.yml",
        " W_KEY:0 \"one\"\n",
    );
    let open_text = "\u{FEFF}l_english:\n OPEN_KEY:0 \"hi\"\n";
    let open_uri = write_loc_file(
        ws.path(),
        "localisation/open_l_english.yml",
        " OPEN_KEY:0 \"hi\"\n",
    );

    let (mut child, mut reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":open_uri,"languageId":"paradox","version":1,"text":open_text}}),
        ),
    )
    .unwrap();
    let _ = diags_for(&mut reader, "open_l_english.yml", 1).expect("didOpen publish");
    let rx = spawn_frame_collector(reader);

    // Round 1: first sight seeds the watched overlay (one batch sweep allowed).
    write_loc_file(
        ws.path(),
        "localisation/watched_l_english.yml",
        " W_KEY:0 \"two\"\n",
    );
    write_frame(
        &mut child,
        &watched_changes(std::slice::from_ref(&watched_uri)),
    )
    .unwrap();
    let frames = drain_after_first(
        &rx,
        std::time::Duration::from_millis(1200),
        std::time::Duration::from_secs(8),
    );
    assert_eq!(
        count_publishes(&frames, "watched_l_english.yml"),
        1,
        "the watched loc file itself should be validated and published once"
    );

    // Round 2: same keys again, new value — no sweep, no open-file republish.
    write_loc_file(
        ws.path(),
        "localisation/watched_l_english.yml",
        " W_KEY:0 \"three\"\n",
    );
    write_frame(
        &mut child,
        &watched_changes(std::slice::from_ref(&watched_uri)),
    )
    .unwrap();
    let frames = drain_after_first(
        &rx,
        std::time::Duration::from_millis(1200),
        std::time::Duration::from_secs(8),
    );
    child.kill().ok();

    assert_eq!(
        count_publishes(&frames, "watched_l_english.yml"),
        1,
        "the watched loc file itself should be validated and published once"
    );
    assert_eq!(
        count_publishes(&frames, "open_l_english.yml"),
        0,
        "a watched loc change with unchanged keys must not sweep open loc files"
    );
}

#[test]
fn test_watched_loc_new_keys_resolve_cross_file_with_one_sweep() {
    // New keys in watched (non-open) loc files must still reach cross-file
    // validation (via the watched-files overlay), and a batch of loc files
    // must produce ONE coalesced sweep of the open loc files, not one per file.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();
    let b_uri = write_loc_file(
        ws.path(),
        "localisation/b_l_english.yml",
        " B_KEY:0 \"b\"\n",
    );
    let c_uri = write_loc_file(
        ws.path(),
        "localisation/c_l_english.yml",
        " C_KEY:0 \"c\"\n",
    );
    // CW225 only fires for refs with lowercase letters (uppercase may be a
    // game variable), so the refs are spelled lowercase.
    let open_text = "\u{FEFF}l_english:\n OPEN_KEY:0 \"see $new_key$ and $new_key2$\"\n";
    let open_uri = write_loc_file(
        ws.path(),
        "localisation/open_l_english.yml",
        " OPEN_KEY:0 \"see $new_key$ and $new_key2$\"\n",
    );

    let (mut child, mut reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":open_uri,"languageId":"paradox","version":1,"text":open_text}}),
        ),
    )
    .unwrap();
    let codes = diags_for(&mut reader, "open_l_english.yml", 1).expect("didOpen publish");
    assert!(
        codes.contains(&"CW225".to_string()),
        "the open file's refs must be unresolved before the watched batch, got: {codes:?}"
    );
    let rx = spawn_frame_collector(reader);

    write_loc_file(
        ws.path(),
        "localisation/b_l_english.yml",
        " B_KEY:0 \"b\"\n NEW_KEY:0 \"n1\"\n",
    );
    write_loc_file(
        ws.path(),
        "localisation/c_l_english.yml",
        " C_KEY:0 \"c\"\n NEW_KEY2:0 \"n2\"\n",
    );
    write_frame(&mut child, &watched_changes(&[b_uri, c_uri])).unwrap();

    let frames = drain_after_first(
        &rx,
        std::time::Duration::from_millis(1200),
        std::time::Duration::from_secs(8),
    );
    child.kill().ok();

    let open_publishes: Vec<&serde_json::Value> = frames
        .iter()
        .filter(|v| {
            v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with("open_l_english.yml"))
        })
        .collect();
    assert_eq!(
        open_publishes.len(),
        1,
        "a batch of watched loc files must sweep the open loc files once, not per file"
    );
    let codes: Vec<String> = open_publishes[0]["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["code"].as_str().map(String::from))
        .collect();
    assert!(
        !codes.contains(&"CW225".to_string()),
        "keys added by watched loc files must resolve cross-file, got: {codes:?}"
    );
}

/// Codes of every publish for `suffix` in `frames`, in arrival order.
fn publish_codes_for(frames: &[serde_json::Value], suffix: &str) -> Vec<Vec<String>> {
    frames
        .iter()
        .filter(|v| {
            v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with(suffix))
        })
        .map(|v| {
            v["params"]["diagnostics"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|d| d["code"].as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn test_watched_loc_keys_survive_scan_index_install() {
    // A workspace scan REPLACES the loc index wholesale from its own disk
    // reads, which can predate a watched change (the read raced the write), so
    // keys held only by the scanned index would silently vanish. The watched
    // overlay must survive the install. Deterministic proxy for the race:
    // revert the watched file on disk with NO watched event, so the rescan
    // reads the OLD content while the overlay still holds the recorded key.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();
    let b_uri = write_loc_file(
        ws.path(),
        "localisation/b_l_english.yml",
        " B_KEY:0 \"b\"\n",
    );
    let open_text = "\u{FEFF}l_english:\n OPEN_KEY:0 \"see $new_key$\"\n";
    let open_uri = write_loc_file(
        ws.path(),
        "localisation/open_l_english.yml",
        " OPEN_KEY:0 \"see $new_key$\"\n",
    );

    let (mut child, mut reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":open_uri,"languageId":"paradox","version":1,"text":open_text}}),
        ),
    )
    .unwrap();
    let codes = diags_for(&mut reader, "open_l_english.yml", 1).expect("didOpen publish");
    assert!(codes.contains(&"CW225".to_string()), "got: {codes:?}");
    let rx = spawn_frame_collector(reader);

    // The watched change adds the key; the batch records it and sweeps.
    write_loc_file(
        ws.path(),
        "localisation/b_l_english.yml",
        " B_KEY:0 \"b\"\n NEW_KEY:0 \"n1\"\n",
    );
    write_frame(&mut child, &watched_changes(std::slice::from_ref(&b_uri))).unwrap();
    let frames = drain_after_first(
        &rx,
        std::time::Duration::from_millis(1200),
        std::time::Duration::from_secs(8),
    );
    let opens = publish_codes_for(&frames, "open_l_english.yml");
    assert!(
        opens
            .last()
            .is_some_and(|c| !c.contains(&"CW225".to_string())),
        "the recorded key must resolve before the rescan, got: {opens:?}"
    );

    // Revert on disk with no event, then rescan: the scan's loc walk reads the
    // pre-change content, standing in for a read that predated the change.
    write_loc_file(
        ws.path(),
        "localisation/b_l_english.yml",
        " B_KEY:0 \"b\"\n",
    );
    write_frame(
        &mut child,
        &jsonrpc_request(
            700,
            "workspace/executeCommand",
            serde_json::json!({ "command": "reindexWorkspace", "arguments": [] }),
        ),
    )
    .unwrap();
    let frames = drain_after_first(
        &rx,
        std::time::Duration::from_millis(1500),
        std::time::Duration::from_secs(15),
    );
    child.kill().ok();

    let opens = publish_codes_for(&frames, "open_l_english.yml");
    assert!(
        opens
            .last()
            .is_some_and(|c| !c.contains(&"CW225".to_string())),
        "a recorded watched key must survive the scan's index install, got: {opens:?}"
    );
}

#[test]
fn test_watched_loc_removed_key_stops_resolving() {
    // The watched overlay has per-file REPLACE semantics: a key added by one
    // watched change and removed by the next stops resolving, and the removal
    // triggers the batch sweep so a referencing open file gets its CW225 back.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();
    let b_uri = write_loc_file(
        ws.path(),
        "localisation/b_l_english.yml",
        " B_KEY:0 \"b\"\n",
    );
    let open_text = "\u{FEFF}l_english:\n OPEN_KEY:0 \"see $new_key$\"\n";
    let open_uri = write_loc_file(
        ws.path(),
        "localisation/open_l_english.yml",
        " OPEN_KEY:0 \"see $new_key$\"\n",
    );

    let (mut child, mut reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":open_uri,"languageId":"paradox","version":1,"text":open_text}}),
        ),
    )
    .unwrap();
    let codes = diags_for(&mut reader, "open_l_english.yml", 1).expect("didOpen publish");
    assert!(codes.contains(&"CW225".to_string()), "got: {codes:?}");
    let rx = spawn_frame_collector(reader);

    // Round 1: the key appears; the open file's ref resolves.
    write_loc_file(
        ws.path(),
        "localisation/b_l_english.yml",
        " B_KEY:0 \"b\"\n NEW_KEY:0 \"n1\"\n",
    );
    write_frame(&mut child, &watched_changes(std::slice::from_ref(&b_uri))).unwrap();
    let frames = drain_after_first(
        &rx,
        std::time::Duration::from_millis(1200),
        std::time::Duration::from_secs(8),
    );
    let opens = publish_codes_for(&frames, "open_l_english.yml");
    assert!(
        opens
            .last()
            .is_some_and(|c| !c.contains(&"CW225".to_string())),
        "the added key must resolve after round 1, got: {opens:?}"
    );

    // Round 2: the key is removed again; the ref must go back to unresolved.
    write_loc_file(
        ws.path(),
        "localisation/b_l_english.yml",
        " B_KEY:0 \"b\"\n",
    );
    write_frame(&mut child, &watched_changes(std::slice::from_ref(&b_uri))).unwrap();
    let frames = drain_after_first(
        &rx,
        std::time::Duration::from_millis(1200),
        std::time::Duration::from_secs(8),
    );
    child.kill().ok();

    let opens = publish_codes_for(&frames, "open_l_english.yml");
    assert_eq!(opens.len(), 1, "the removal must trigger one sweep");
    assert!(
        opens[0].contains(&"CW225".to_string()),
        "a key removed from a watched loc file must stop resolving, got: {opens:?}"
    );
}

#[test]
fn test_config_no_op_skips_revalidate_then_real_change_runs() {
    // Identical didChangeConfiguration payloads must not trigger a revalidate on
    // the second send; a genuinely changed ignoredErrorCodes must trigger
    // exactly one `(configChange)` pass over the single open document.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let (mut child, mut reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());

    // Open one game file so `revalidate_all_open_docs` has exactly one doc to
    // validate — one `(configChange)` log per real pass.
    let rel = "common/decisions/cfg.txt";
    let path = ws.path().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, STORM_FILE).unwrap();
    let uri = path_uri(&path);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":uri,"languageId":"hoi4","version":1,"text":STORM_FILE}}),
        ),
    )
    .unwrap();
    // Drain the didOpen publish before starting the config observation window.
    let _ = diags_for(&mut reader, "cfg.txt", 1);
    let rx = spawn_frame_collector(reader);

    let cfg = |codes: &[&str]| {
        jsonrpc_notification(
            "workspace/didChangeConfiguration",
            serde_json::json!({ "settings": { "ignoredErrorCodes": codes } }),
        )
    };
    let quiet = std::time::Duration::from_millis(700);
    let budget = std::time::Duration::from_secs(5);

    // Counts are cumulative over the non-draining profiling buffer, so each
    // window asserts the running total of `(configChange)` validations.

    // First send changes the (empty) live codes → one configChange pass.
    write_frame(&mut child, &cfg(&["CW999"])).unwrap();
    drain_after_first(&rx, quiet, budget);
    let log1 = fetch_profiling_log(&mut child, &rx, 1101);
    assert_eq!(
        count_validate_log(&log1, "configChange"),
        1,
        "first (changed) config should revalidate the open doc once"
    );
    // The open doc's AST is current (opened, never edited), so the revalidate
    // must reuse the stored AST via the prebuilt path — no fresh parse.
    assert!(
        log1.contains("(configChange)") && log1.contains("(prebuilt, no reparse)"),
        "an unedited open doc should revalidate from its stored AST, not re-parse: {log1}"
    );

    // Identical payload → no-op guard skips the revalidate (total stays 1).
    write_frame(&mut child, &cfg(&["CW999"])).unwrap();
    drain_until_quiet(&rx, quiet, budget);
    let log2 = fetch_profiling_log(&mut child, &rx, 1102);
    assert_eq!(
        count_validate_log(&log2, "configChange"),
        1,
        "an identical config re-send must not revalidate"
    );

    // A real change → one more configChange pass (total 2).
    write_frame(&mut child, &cfg(&["CW998"])).unwrap();
    drain_after_first(&rx, quiet, budget);
    let log3 = fetch_profiling_log(&mut child, &rx, 1103);
    child.kill().ok();
    assert_eq!(
        count_validate_log(&log3, "configChange"),
        2,
        "a genuinely changed config should revalidate once"
    );
}

#[test]
fn test_config_idle_only_change_passes_noop_guard() {
    // A didChangeConfiguration mutating ONLY backgroundReindexIdleSeconds must
    // count as a change (the no-op guard compares every field the handler
    // writes); an identical re-send must then hit the guard.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let (mut child, mut reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());

    let rel = "common/decisions/cfg.txt";
    let path = ws.path().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, STORM_FILE).unwrap();
    let uri = path_uri(&path);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":uri,"languageId":"hoi4","version":1,"text":STORM_FILE}}),
        ),
    )
    .unwrap();
    let _ = diags_for(&mut reader, "cfg.txt", 1);
    let rx = spawn_frame_collector(reader);

    let cfg = |secs: u64| {
        jsonrpc_notification(
            "workspace/didChangeConfiguration",
            serde_json::json!({ "settings": { "backgroundReindexIdleSeconds": secs } }),
        )
    };
    let quiet = std::time::Duration::from_millis(700);
    let budget = std::time::Duration::from_secs(5);

    // Cumulative `(configChange)` counts over the non-draining profiling buffer.

    // 5 differs from the 15s default → one configChange pass.
    write_frame(&mut child, &cfg(5)).unwrap();
    drain_after_first(&rx, quiet, budget);
    let log1 = fetch_profiling_log(&mut child, &rx, 1201);
    assert_eq!(
        count_validate_log(&log1, "configChange"),
        1,
        "an idle-only config change must not be swallowed by the no-op guard"
    );

    // Identical payload → no-op guard skips the revalidate (total stays 1).
    write_frame(&mut child, &cfg(5)).unwrap();
    drain_until_quiet(&rx, quiet, budget);
    let log2 = fetch_profiling_log(&mut child, &rx, 1202);
    child.kill().ok();
    assert_eq!(
        count_validate_log(&log2, "configChange"),
        1,
        "an identical idle re-send must not revalidate"
    );
}

#[test]
fn test_config_partial_payload_keeps_ignore_lists() {
    // A didChangeConfiguration carrying ONLY backgroundReindexIdleSeconds must
    // leave the previously-set ignore lists intact (absent-means-keep in this
    // handler), not wipe them to empty.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let (mut child, mut reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());

    let rel = "common/decisions/cfg.txt";
    let path = ws.path().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, STORM_FILE).unwrap();
    let uri = path_uri(&path);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":uri,"languageId":"hoi4","version":1,"text":STORM_FILE}}),
        ),
    )
    .unwrap();
    let _ = diags_for(&mut reader, "cfg.txt", 1);
    let rx = spawn_frame_collector(reader);

    let quiet = std::time::Duration::from_millis(700);
    let budget = std::time::Duration::from_secs(5);
    let baseline_log = fetch_profiling_log(&mut child, &rx, 1300);
    let baseline_scans = baseline_log.matches("workspace_scan_start").count();

    // Establish non-empty ignore lists. Discovery changed, so this needs a full
    // scan to rebuild both the script and localisation indexes.
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "workspace/didChangeConfiguration",
            serde_json::json!({ "settings": {
                "ignoredErrorCodes": ["CW999"],
                "ignoreFilePatterns": ["**/skip.txt"],
            }}),
        ),
    )
    .unwrap();
    drain_after_first(&rx, quiet, budget);
    let log1 = fetch_profiling_log(&mut child, &rx, 1301);
    assert_eq!(
        log1.matches("workspace_scan_start").count(),
        baseline_scans + 1,
        "setting the ignore lists should run one full scan"
    );

    // Idle-only partial payload → revalidates (idle changed) but must keep
    // the ignore lists.
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "workspace/didChangeConfiguration",
            serde_json::json!({ "settings": { "backgroundReindexIdleSeconds": 5 } }),
        ),
    )
    .unwrap();
    drain_after_first(&rx, quiet, budget);
    let log2 = fetch_profiling_log(&mut child, &rx, 1302);
    assert_eq!(
        count_validate_log(&log2, "configChange"),
        1,
        "an idle-only change should revalidate the open document once"
    );
    assert_eq!(
        log2.matches("workspace_scan_start").count(),
        baseline_scans + 1,
        "an idle-only change must not run another full scan"
    );

    // Re-send the full payload (plus the now-current idle value). If the
    // partial payload had wiped the lists, this would differ and revalidate;
    // absent-means-keep makes it a no-op.
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "workspace/didChangeConfiguration",
            serde_json::json!({ "settings": {
                "ignoredErrorCodes": ["CW999"],
                "ignoreFilePatterns": ["**/skip.txt"],
                "backgroundReindexIdleSeconds": 5,
            }}),
        ),
    )
    .unwrap();
    drain_until_quiet(&rx, quiet, budget);
    let log3 = fetch_profiling_log(&mut child, &rx, 1303);
    child.kill().ok();
    assert_eq!(
        count_validate_log(&log3, "configChange"),
        1,
        "the partial payload must not have wiped the ignore lists"
    );
    assert_eq!(
        log3.matches("workspace_scan_start").count(),
        baseline_scans + 1,
        "the identical full payload must not run another full scan"
    );
}

#[test]
fn test_live_ignore_clears_closed_localisation_diagnostics() {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();
    write_disk_file(ws.path(), "common/decisions/scan.txt", STORM_FILE);
    let ignored_uri = write_loc_file(
        ws.path(),
        "localisation/ignored_l_english.yml",
        " broken:0 \"unterminated\n",
    );

    let (mut child, reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());
    let rx = spawn_frame_collector(reader);
    reindex_until_scan_starts(&mut child, &rx);
    recv_frame_until(
        &rx,
        std::time::Duration::from_secs(10),
        "the localisation diagnostic before it is ignored",
        |_| {},
        |v| {
            v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"] == ignored_uri
                && publish_has_code(v, "CW268")
        },
    );

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "workspace/didChangeConfiguration",
            serde_json::json!({ "settings": {
                "ignoreFilePatterns": ["**/ignored_l_english.yml"],
            }}),
        ),
    )
    .unwrap();
    recv_frame_until(
        &rx,
        std::time::Duration::from_secs(10),
        "an empty publish clearing the ignored localisation diagnostic",
        |_| {},
        |v| {
            v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"] == ignored_uri
                && v["params"]["diagnostics"]
                    .as_array()
                    .is_some_and(Vec::is_empty)
        },
    );
    child.kill().ok();
}

#[test]
fn test_init_ignore_pattern_caps_cut_hostile_payload() {
    // #169: initializationOptions carrying a 1 MB glob and over-count lists
    // must be cut at the boundary. The startup summary (window/logMessage)
    // reports the capped counts: the 1 MB glob is dropped, the dir and code
    // lists truncated. A reverted cap would report 300 dirs and 250 codes
    // and feed the 1 MB pattern to the matcher for every walked entry.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();
    for rel in [
        "common/decisions/a.txt",
        "common/decisions/b.txt",
        "events/e.txt",
    ] {
        let path = ws.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, STORM_FILE).unwrap();
    }

    let ws_uri = path_uri(ws.path());
    let mut cmd = cwtools_server_cmd();
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let huge = "?".repeat(1024 * 1024);
    let over_count_dirs: Vec<String> = (0..300).map(|i| format!("dir{i}")).collect();
    let over_count_codes: Vec<String> = (0..250).map(|i| format!("CW{i}")).collect();
    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "vanilla": vanilla.path().to_string_lossy(),
                "ignoreFilePatterns": ["**/skip.txt", huge],
                "ignoreDirectories": over_count_dirs,
                "ignoredErrorCodes": over_count_codes,
            }
        }),
    );
    write_frame(&mut child, &init).unwrap();

    // The summary is logged during initialize, before the response. Reading
    // the response first would skip it, so scan frames in order.
    let mut summary: Option<String> = None;
    for _ in 0..2000 {
        let raw = read_frame(&mut reader).unwrap_or_default();
        if raw.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        if v["method"] == "window/logMessage"
            && let Some(m) = v["params"]["message"].as_str()
            && m.contains("ignore patterns:")
        {
            summary = Some(m.to_string());
            break;
        }
    }
    let summary = summary.expect("startup summary logMessage");
    assert!(
        summary.contains("1 files, 200 dirs, 200 suppressed codes"),
        "caps must truncate the hostile payload; got: {summary}"
    );

    // The startup scan must still complete with the capped patterns in place.
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);
    child.kill().ok();
}

#[test]
fn test_get_file_types_answers_during_watched_flood() {
    // The direct #90 regression: a large watched flood must not starve a cheap
    // getFileTypes request. With validation off the message future, the request
    // answers well under 2s.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let (mut child, reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());
    let uris: Vec<String> = (0..80)
        .map(|i| write_disk_file(ws.path(), &format!("common/decisions/g{i}.txt"), STORM_FILE))
        .collect();
    let rx = spawn_frame_collector(reader);

    // Fire the flood, then immediately ask for file types.
    write_frame(&mut child, &watched_changes(&uris)).unwrap();
    let sent = std::time::Instant::now();
    write_frame(
        &mut child,
        &jsonrpc_request(
            777,
            "workspace/executeCommand",
            serde_json::json!({ "command": "getFileTypes", "arguments": [uris[0]] }),
        ),
    )
    .unwrap();

    let mut elapsed = None;
    let deadline = std::time::Duration::from_secs(5);
    while sent.elapsed() < deadline {
        match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(v) => {
                if v["id"] == 777 {
                    elapsed = Some(sent.elapsed());
                    break;
                }
            }
            Err(_) => continue,
        }
    }
    child.kill().ok();

    let elapsed = elapsed.expect("getFileTypes never responded");
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "getFileTypes took {elapsed:?} during a watched flood (should be < 2s)"
    );
}

#[test]
fn test_unknown_execute_command_returns_error() {
    // An unrecognized workspace/executeCommand must surface a JSON-RPC error
    // naming the command — a silent `null` success masks client/engine
    // version drift (a newer client invoking a command this server lacks).
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let (mut child, reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());
    let rx = spawn_frame_collector(reader);

    write_frame(
        &mut child,
        &jsonrpc_request(
            901,
            "workspace/executeCommand",
            serde_json::json!({ "command": "notARealCommand", "arguments": [] }),
        ),
    )
    .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut response = None;
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(v) if v["id"] == 901 => {
                response = Some(v);
                break;
            }
            _ => continue,
        }
    }
    child.kill().ok();

    let v = response.expect("no response to the unknown command");
    assert!(
        !v["error"].is_null(),
        "unknown command must produce an error response, got: {v}"
    );
    assert!(
        v["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("notARealCommand")),
        "the error should name the offending command, got: {}",
        v["error"]
    );
}

#[test]
fn test_validate_workspace_command_returns_summary() {
    // `validateWorkspace` runs a full scan and returns aggregate counts.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    // One malformed closed file is enough to produce a non-zero error count.
    let bad = ws.path().join("common/decisions/bad.txt");
    std::fs::create_dir_all(bad.parent().unwrap()).unwrap();
    std::fs::write(&bad, "bad_dec = {\n").unwrap();

    let (mut child, mut reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());
    write_frame(
        &mut child,
        &jsonrpc_request(
            902,
            "workspace/executeCommand",
            serde_json::json!({ "command": "validateWorkspace", "arguments": [] }),
        ),
    )
    .unwrap();
    let raw = read_response(&mut reader).expect("no response to validateWorkspace");
    child.kill().ok();

    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let result = v["result"]
        .as_object()
        .expect("validateWorkspace returned an object");
    assert_eq!(
        result["totalFiles"].as_u64(),
        Some(1),
        "summary must count the one workspace file, got: {result:?}"
    );
    assert_eq!(
        result["validatedFiles"].as_u64(),
        Some(1),
        "summary must count the one validated file, got: {result:?}"
    );
    assert_eq!(
        result["filesWithErrors"].as_u64(),
        Some(1),
        "the malformed file must carry an error, got: {result:?}"
    );
    assert!(
        result["totalErrors"].as_u64().unwrap_or(0) > 0,
        "summary must report a positive error count, got: {result:?}"
    );
}

#[test]
fn test_did_open_burst_stops_at_the_document_count_limit() {
    const MAX_OPEN_DOCUMENTS: usize = 128;

    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();
    let (mut child, reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());
    let rx = spawn_frame_collector(reader);

    for i in 0..=MAX_OPEN_DOCUMENTS {
        let uri = path_uri(ws.path().join(format!("common/decisions/{i}.txt")));
        write_frame(
            &mut child,
            &jsonrpc_notification(
                "textDocument/didOpen",
                serde_json::json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": "hoi4",
                        "version": 1,
                        "text": "x = 1\n"
                    }
                }),
            ),
        )
        .unwrap();
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut validations = 0;
    let mut request_id = 2000;
    while std::time::Instant::now() < deadline {
        let log = fetch_profiling_log(&mut child, &rx, request_id);
        validations = count_validate_log(&log, "didOpen");
        if validations >= MAX_OPEN_DOCUMENTS {
            break;
        }
        request_id += 1;
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    child.kill().ok();
    child.wait().ok();

    assert_eq!(validations, MAX_OPEN_DOCUMENTS);
}

#[test]
fn test_did_open_validates_deferred() {
    // did_open now offloads validation off the message future; the file must
    // still get validated and its diagnostics published.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let (mut child, reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());
    let rel = "common/decisions/o.txt";
    let path = ws.path().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let uri = path_uri(&path);
    let rx = spawn_frame_collector(reader);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":uri,"languageId":"hoi4","version":1,"text":STORM_FILE}}),
        ),
    )
    .unwrap();

    let frames = drain_after_first(
        &rx,
        std::time::Duration::from_millis(800),
        std::time::Duration::from_secs(6),
    );
    let log = fetch_profiling_log(&mut child, &rx, 1004);
    child.kill().ok();

    assert_eq!(
        count_validate_log(&log, "didOpen"),
        1,
        "did_open should validate the file once, off the message future"
    );
    assert_eq!(
        count_publishes(&frames, "o.txt"),
        1,
        "did_open should publish diagnostics for the opened file"
    );
}

#[test]
fn test_did_open_then_immediate_close_ends_empty() {
    // Opening then immediately closing must leave the empty publish as the final
    // state — no stale late diagnostics from the deferred open validate racing
    // did_close.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let (mut child, reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());
    let rel = "common/decisions/c.txt";
    let path = ws.path().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let uri = path_uri(&path);
    let rx = spawn_frame_collector(reader);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":uri,"languageId":"hoi4","version":1,"text":STORM_FILE}}),
        ),
    )
    .unwrap();
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didClose",
            serde_json::json!({"textDocument":{"uri":uri}}),
        ),
    )
    .unwrap();

    let frames = drain_after_first(
        &rx,
        std::time::Duration::from_millis(1000),
        std::time::Duration::from_secs(6),
    );
    child.kill().ok();

    let last_publish = frames.iter().rev().find(|v| {
        v["method"] == "textDocument/publishDiagnostics"
            && v["params"]["uri"]
                .as_str()
                .is_some_and(|u| u.ends_with("c.txt"))
    });
    let last = last_publish.expect("expected at least the did_close empty publish for c.txt");
    let diags = last["params"]["diagnostics"].as_array().unwrap();
    assert!(
        diags.is_empty(),
        "final publish for a closed file must be empty, got: {:?}",
        diags
    );
}

#[test]
fn test_did_save_revalidates_the_open_document() {
    // did_save takes its own path: no text arrives with the notification
    // (`include_text` is false), nothing is written to the document store, and
    // the edit generation is read rather than bumped — it re-validates the
    // buffer the server already holds. Nothing pinned that it validates at all.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let (mut child, reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());
    let uri = write_disk_file(ws.path(), "common/decisions/s.txt", STORM_FILE);
    let rx = spawn_frame_collector(reader);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":uri,"languageId":"hoi4","version":1,"text":STORM_FILE}}),
        ),
    )
    .unwrap();
    let opened = drain_after_first(
        &rx,
        std::time::Duration::from_millis(800),
        std::time::Duration::from_secs(6),
    );
    assert_eq!(
        count_publishes(&opened, "s.txt"),
        1,
        "the open must publish before the save window opens"
    );

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didSave",
            serde_json::json!({"textDocument":{"uri":uri}}),
        ),
    )
    .unwrap();
    let saved = drain_after_first(
        &rx,
        std::time::Duration::from_millis(800),
        std::time::Duration::from_secs(6),
    );
    let log = fetch_profiling_log(&mut child, &rx, 1020);
    child.kill().ok();

    assert_eq!(
        count_validate_log(&log, "didSave"),
        1,
        "a save must validate the document once, tagged as a save"
    );
    assert_eq!(
        count_publishes(&saved, "s.txt"),
        1,
        "a save must republish the saved document's diagnostics"
    );
}

#[test]
fn test_did_save_for_a_document_that_was_never_opened_is_a_no_op() {
    // Unlike did_change, which reports a rejection, did_save returns silently
    // when the URI is not an open document: it needs the stored version, and
    // there is none. A client that saves a file it never opened (or one the
    // retention cap evicted) must not make the server read or validate it.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let (mut child, reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());
    let uri = write_disk_file(ws.path(), "common/decisions/u.txt", STORM_FILE);
    let rx = spawn_frame_collector(reader);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didSave",
            serde_json::json!({"textDocument":{"uri":uri}}),
        ),
    )
    .unwrap();

    let frames = drain_until_quiet(
        &rx,
        std::time::Duration::from_millis(1200),
        std::time::Duration::from_secs(6),
    );
    let log = fetch_profiling_log(&mut child, &rx, 1021);
    child.kill().ok();

    assert_eq!(
        count_validate_log(&log, "didSave"),
        0,
        "a save for a document that was never opened must not validate anything"
    );
    assert_eq!(
        count_publishes(&frames, "u.txt"),
        0,
        "and must not publish diagnostics for it"
    );
}

#[test]
fn test_did_close_restores_disk_definition() {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let definition_rel = "common/national_focus/f.txt";
    let usage_rel = "common/decisions/d.txt";
    let definition_uri = write_disk_file(ws.path(), definition_rel, "DISK_FOCUS = { x = yes }\n");
    let usage = "my_dec = {\n    has_focus = DISK_FOCUS\n}\n";
    let usage_uri = write_disk_file(ws.path(), usage_rel, usage);
    let (mut child, mut reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":usage_uri,"languageId":"hoi4","version":1,"text":usage}}),
        ),
    )
    .unwrap();
    wait_for_diagnostics(&mut reader, usage_rel);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":definition_uri,"languageId":"hoi4","version":1,"text":"LIVE_FOCUS = { x = yes }\n"}}),
        ),
    )
    .unwrap();
    wait_for_diagnostics(&mut reader, definition_rel);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didClose",
            serde_json::json!({"textDocument":{"uri":definition_uri}}),
        ),
    )
    .unwrap();
    wait_for_diagnostics(&mut reader, definition_rel);

    write_frame(
        &mut child,
        &jsonrpc_request(
            902,
            "textDocument/definition",
            serde_json::json!({
                "textDocument": {"uri": usage_uri},
                "position": {"line": 1, "character": 20},
            }),
        ),
    )
    .unwrap();
    let raw = read_response(&mut reader).expect("no definition response after didClose");
    child.kill().ok();
    let response: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let result = &response["result"];
    assert!(
        result.as_array().is_some_and(|locations| locations
            .iter()
            .any(|location| location["uri"] == definition_uri))
            || result["uri"] == definition_uri,
        "closing the live buffer should restore the disk definition, got: {result:?}"
    );
}

// ── did_open indexes its own subtype membership (architecture review L2) ─────

/// Fixture adapted from `crates/validation/tests/subtype_membership.rs`: a
/// `naval_equip` subtype is discriminated by `archetype = <equipment.naval_equip>`,
/// which only resolves once the archetype's own subtype-qualified membership
/// (`equipment.naval_equip`) is in the type index. Without that merge, the
/// variant never activates `naval_equip` and `model` becomes an unrecognized
/// field (CW263).
const SUBTYPE_RULES: &str = r#"
types = {
    type[equipment] = {
        skip_root_key = equipments
        path = "game/common/units/equipment"
        subtype[archetype_equip] = {
            ## cardinality = 0..1
            is_archetype = yes
        }
        subtype[naval_equip] = {
            ## cardinality = 0..1
            type = enum[ship_units]
            ## cardinality = 0..1
            archetype = <equipment.naval_equip>
        }
    }
}

equipment = {
    ## cardinality = 0..1
    is_archetype = bool
    ## cardinality = 0..1
    archetype = <equipment>
    ## cardinality = 0..1
    type = enum[ship_units]
    alias_name[unit_stat] = alias_match_left[unit_stat]
    subtype[naval_equip] = {
        ## cardinality = 0..1
        model = scalar
    }
}

alias[unit_stat:build_cost_ic] = float

enums = {
    enum[ship_units] = {
        submarine
        destroyer
    }
}
"#;

const SUBTYPE_SCRIPT: &str = r#"
equipments = {
    ship_hull_submarine = {
        is_archetype = yes
        type = submarine
        model = base_sub_model
    }
    ship_hull_cruiser_submarine = {
        archetype = ship_hull_submarine
        model = cruiser_sub_model
    }
}
"#;

#[test]
fn did_open_indexes_own_subtype_membership() {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    // An explicit empty `vanilla` dir, or `ensure_vanilla_index` auto-discovers
    // a real game install on the host (e.g. a Steam copy of HOI4) and merges its
    // equipment archetypes into `equipment.naval_equip`, masking the bug this
    // test guards against.
    let vanilla_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("test_rules.cwt"), SUBTYPE_RULES).unwrap();

    let rel_path = "common/units/equipment/ships.txt";
    let file_path = ws.path().join(rel_path);
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    // Empty on disk: the initial workspace scan (which indexes correctly via
    // `index_parsed_file`, unaffected by this bug) must never see the
    // archetype. It's introduced only by the live edit below, so any
    // `equipment.naval_equip` membership the variant resolves against can only
    // have come from `parse_and_validate`'s own indexing of that edit.
    std::fs::write(&file_path, "").unwrap();

    let ws_uri = path_uri(ws.path());
    let doc_uri = path_uri(&file_path);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "vanilla": vanilla_dir.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    // Diagnostics are gated (published as an empty set) until the initial scan
    // finishes and flips `index_ready`; wait for it so the assertions below
    // see the server's real validation output, not the gate's placeholder.
    wait_for_scan_done(&mut reader);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": doc_uri,
                    "languageId": "hoi4",
                    "version": 1,
                    "text": "",
                }
            }),
        ),
    )
    .unwrap();
    wait_for_diagnostics(&mut reader, rel_path);

    // Edit in the archetype and its naval variant together: the variant's
    // `archetype = <equipment.naval_equip>` discriminator only activates once
    // this file's own archetype is merged into `equipment.naval_equip` by the
    // same edit's indexing pass (did_change also runs through
    // `parse_and_validate`).
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": doc_uri, "version": 2 },
                "contentChanges": [{ "text": SUBTYPE_SCRIPT }],
            }),
        ),
    )
    .unwrap();

    let codes = diags_for(&mut reader, rel_path, 1).expect("diagnostics for ships.txt after edit");
    child.kill().ok();

    assert!(
        !codes.contains(&"CW263".to_string()),
        "an edit that adds both the archetype and its naval variant in one \
         didChange must index the archetype's own subtype membership so the \
         variant activates naval_equip and `model` is a recognized field, \
         got: {:?}",
        codes
    );
}

// ── Keystroke path runs CW100 too (open-doc missing-loc flicker) ─────────────

const MISSING_LOC_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        localisation = {
            ## required
            name = "$"
            ## required
            desc = "$_desc"
        }
    }
}
thing = { x = scalar }
"#;

#[test]
fn test_keystroke_edit_reports_missing_loc_once_loc_index_is_built() {
    // CW100 (missing localisation) is gated on the loc index being non-empty
    // (`validate.rs::append_missing_loc_errors`). Before this fix the gate was
    // applied only in the scan/dependent-sweep path
    // (`validate_parsed_with_indexes`), not in the keystroke path
    // (`parse_and_validate`) — so an open doc's CW100 would appear after a scan
    // or dependent revalidation and then vanish on the very next edit, until the
    // next scan. This is the flicker's kill test: once the loc index is built,
    // a didChange (the keystroke path) must still report CW100, not clear it.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    // Explicit empty vanilla dir so a real host game install (if any) can't
    // merge in loc data that would mask the missing `my_thing_desc` key.
    let vanilla_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("test_rules.cwt"), MISSING_LOC_RULES).unwrap();

    // Non-empty loc union (the CW100 gate), but missing `my_thing_desc`.
    let loc_dir = ws.path().join("localisation");
    std::fs::create_dir_all(&loc_dir).unwrap();
    let mut loc_bytes: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
    loc_bytes.extend_from_slice(b"l_english:\n my_thing:0 \"My Thing\"\n");
    std::fs::write(loc_dir.join("test_l_english.yml"), &loc_bytes).unwrap();

    let rel_path = "common/things/test.txt";
    let file_path = ws.path().join(rel_path);
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    // Empty on disk: the initial scan must not see `my_thing`, so the CW100
    // this test checks for can only have come from the didChange below (the
    // keystroke path), not a diagnostic left over from the scan.
    std::fs::write(&file_path, "").unwrap();

    let ws_uri = path_uri(ws.path());
    let doc_uri = path_uri(&file_path);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "vanilla": vanilla_dir.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    // The loc index (built from `localisation/` above) is complete by the time
    // the scan flips `index_ready` and the loading bar closes.
    wait_for_scan_done(&mut reader);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": doc_uri,
                    "languageId": "hoi4",
                    "version": 1,
                    "text": "",
                }
            }),
        ),
    )
    .unwrap();
    wait_for_diagnostics(&mut reader, rel_path);

    // The keystroke path: didChange runs through `parse_and_validate`, not the
    // scan/dependent-sweep's `validate_parsed_with_indexes`.
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": doc_uri, "version": 2 },
                "contentChanges": [{ "text": "my_thing = { x = 1 }\n" }],
            }),
        ),
    )
    .unwrap();

    let codes = diags_for(&mut reader, rel_path, 1).expect("diagnostics after keystroke edit");
    child.kill().ok();

    assert!(
        codes.contains(&"CW100".to_string()),
        "a didChange (keystroke) edit must report CW100 for my_thing's missing \
         required `my_thing_desc` loc key once the loc index is non-empty, \
         got: {:?}",
        codes
    );
}

// ── Code actions (quick-fixes from SuggestedFix payloads) ────────────────────

/// Read frames until a publishDiagnostics for `suffix` arrives carrying a
/// diagnostic whose `code` matches, returning that full diagnostic JSON object
/// (range + data included). None on timeout.
fn wait_for_diag_object(
    reader: &mut BufReader<std::process::ChildStdout>,
    suffix: &str,
    code: &str,
) -> Option<serde_json::Value> {
    for _ in 0..2000 {
        let raw = read_frame(reader).ok()?;
        if raw.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        if v["method"] == "textDocument/publishDiagnostics"
            && v["params"]["uri"]
                .as_str()
                .is_some_and(|u| u.ends_with(suffix))
            && let Some(d) = v["params"]["diagnostics"]
                .as_array()
                .and_then(|a| a.iter().find(|d| d["code"] == code))
        {
            return Some(d.clone());
        }
    }
    None
}

#[test]
fn test_code_action_quickfix_from_diagnostic() {
    // End-to-end: an empty `limit = { }` produces a CW281 diagnostic carrying a
    // fix payload in `data`. Round-tripping that diagnostic back through
    // textDocument/codeAction must yield one QUICKFIX whose edit, applied to the
    // source, deletes the empty limit — the same result as the CLI `fix`.
    const RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap(); // empty dir → index marked complete
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    let rel_path = "common/decisions/test.txt";
    let text = "x = { limit = { } }\n";
    let file_path = ws.path().join(rel_path);
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, text).unwrap();

    let ws_uri = path_uri(ws.path());
    let doc_uri = path_uri(&file_path);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "vanilla": vanilla.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &init).unwrap();
    let init_resp_str = read_response(&mut reader).expect("no init response");
    let init_resp: serde_json::Value = serde_json::from_str(&init_resp_str).unwrap();
    // Capability advertised: quickfix code actions, no resolve step.
    let ca = &init_resp["result"]["capabilities"]["codeActionProvider"];
    assert_eq!(ca["codeActionKinds"][0], "quickfix", "got: {ca}");
    assert_eq!(ca["resolveProvider"], false, "got: {ca}");

    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":doc_uri,"languageId":"hoi4","version":1,"text":text}}),
        ),
    )
    .unwrap();

    let diag = wait_for_diag_object(&mut reader, rel_path, "CW281")
        .expect("CW281 diagnostic published for the empty limit");
    assert!(
        diag.get("data").is_some(),
        "the diagnostic must carry a fix payload in `data`: {diag}"
    );

    let ca_req = jsonrpc_request(
        2,
        "textDocument/codeAction",
        serde_json::json!({
            "textDocument": { "uri": doc_uri },
            "range": diag["range"],
            "context": { "diagnostics": [diag] },
        }),
    );
    write_frame(&mut child, &ca_req).unwrap();
    let resp_str = read_response(&mut reader).expect("no codeAction response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(resp["id"], 2, "got: {resp_str}");
    let all = resp["result"].as_array().expect("array result");
    // An unfiltered request also carries the `source.fixAll` and
    // ignore-in-workspace actions; this test is about the per-diagnostic
    // quickfix.
    let actions: Vec<&serde_json::Value> = all.iter().filter(|a| a["kind"] == "quickfix").collect();
    assert_eq!(
        actions.len(),
        2,
        "the fix plus the ignore action: {resp_str}"
    );
    let action = actions[0];
    assert_eq!(action["kind"], "quickfix");
    assert_eq!(action["title"], "Remove empty limit");

    // The edit must reproduce the CLI `fix` output when applied.
    let changes = action["edit"]["changes"]
        .as_object()
        .expect("edit carries a changes map");
    let edits = changes
        .values()
        .next()
        .and_then(|v| v.as_array())
        .expect("edits for the document");
    assert_eq!(edits.len(), 1, "one text edit: {resp_str}");
    let e = &edits[0];
    let sc = e["range"]["start"]["character"].as_u64().unwrap() as usize;
    let ec = e["range"]["end"]["character"].as_u64().unwrap() as usize;
    let new_text = e["newText"].as_str().unwrap();
    // The fix is confined to line 0; splice the char range on that line.
    let nl = text.find('\n').unwrap();
    let chars: Vec<char> = text[..nl].chars().collect();
    let mut fixed: String = chars[..sc].iter().collect();
    fixed.push_str(new_text);
    fixed.extend(chars[ec..].iter());
    fixed.push_str(&text[nl..]);
    assert_eq!(
        fixed, "x = { }\n",
        "applied edit must delete the empty limit"
    );
}

/// Whether a publish notification's diagnostics include `code`.
fn publish_has_code(v: &serde_json::Value, code: &str) -> bool {
    v["params"]["diagnostics"]
        .as_array()
        .is_some_and(|a| a.iter().any(|d| d["code"] == code))
}

/// Drain server frames until the next `publishDiagnostics` whose URI ends with
/// `rel_path`, returning the notification. A server exit (EOF) fails the test
/// rather than blocking: `read_frame` returns an empty frame at end of stream,
/// which would otherwise loop forever.
fn wait_for_publish_to(
    reader: &mut BufReader<std::process::ChildStdout>,
    rel_path: &str,
) -> serde_json::Value {
    for _ in 0..400 {
        let raw = match read_frame(reader) {
            Ok(r) => r,
            Err(_) => panic!("server exited while waiting for a publish of {rel_path}"),
        };
        if raw.is_empty() {
            panic!("server exited while waiting for a publish of {rel_path}");
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw)
            && v["method"] == "textDocument/publishDiagnostics"
            && v["params"]["uri"]
                .as_str()
                .is_some_and(|u| u.ends_with(rel_path))
        {
            return v;
        }
    }
    panic!("no publishDiagnostics for {rel_path} in 400 frames")
}

#[test]
fn test_inline_ignore_directive_suppresses_a_keystroke_diagnostic() {
    // The same file opened twice, once with `# cwtools-ignore CW281` trailing
    // the offending line. The plain copy proves the setup fires CW281; the
    // directive'd copy must publish it never.
    const RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap(); // empty dir → index marked complete
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    let clean_rel = "common/decisions/clean.txt";
    let clean_text = "x = { limit = { } }\n";
    let clean_path = ws.path().join(clean_rel);
    std::fs::create_dir_all(clean_path.parent().unwrap()).unwrap();
    std::fs::write(&clean_path, clean_text).unwrap();

    let ignored_rel = "common/decisions/ignored.txt";
    let ignored_text = "x = { limit = { } } # cwtools-ignore CW281\n";
    let ignored_path = ws.path().join(ignored_rel);
    std::fs::write(&ignored_path, ignored_text).unwrap();

    let ws_uri = path_uri(ws.path());
    let clean_uri = path_uri(&clean_path);
    let ignored_uri = path_uri(&ignored_path);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "vanilla": vanilla.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &init).unwrap();
    let init_resp_str = read_response(&mut reader).expect("no init response");
    assert!(
        serde_json::from_str::<serde_json::Value>(&init_resp_str).unwrap()["result"]["capabilities"]
            ["codeActionProvider"]
            .is_object()
    );
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    // The plain copy proves the setup fires CW281.
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({ "textDocument": { "uri": clean_uri, "languageId": "hoi4", "version": 1, "text": clean_text } }),
        ),
    )
    .unwrap();
    let diag = wait_for_diag_object(&mut reader, clean_rel, "CW281")
        .expect("CW281 diagnostic published for the clean file");
    assert!(diag.get("data").is_some());

    // The directive'd copy must not publish it. The first publish for the
    // file after did_open is the keystroke validation result; a later one
    // (e.g. from a dependent sweep) would be filtered the same way, but the
    // first is enough to prove the directive is honored.
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({ "textDocument": { "uri": ignored_uri, "languageId": "hoi4", "version": 1, "text": ignored_text } }),
        ),
    )
    .unwrap();
    let publish = wait_for_publish_to(&mut reader, ignored_rel);
    assert!(
        !publish_has_code(&publish, "CW281"),
        "directive must suppress CW281: {publish}"
    );
    child.kill().ok();
}

#[test]
fn test_inline_ignore_directive_suppresses_a_scan_diagnostic() {
    // Closed files: the workspace scan validates them from disk, and the
    // directive must filter its publish the same way the keystroke path does.
    // The plain copy proves the scan publishes CW281 at all.
    const RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap(); // empty dir → index marked complete
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    let clean_rel = "common/decisions/clean.txt";
    let clean_path = ws.path().join(clean_rel);
    std::fs::create_dir_all(clean_path.parent().unwrap()).unwrap();
    std::fs::write(&clean_path, "x = { limit = { } }\n").unwrap();

    let ignored_rel = "common/decisions/ignored.txt";
    let ignored_path = ws.path().join(ignored_rel);
    std::fs::write(
        &ignored_path,
        "x = { limit = { } } # cwtools-ignore CW281\n",
    )
    .unwrap();

    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "vanilla": vanilla.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &init).unwrap();
    let init_resp_str = read_response(&mut reader).expect("no init response");
    assert!(!init_resp_str.is_empty());
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();

    // Capture every publish the startup scan sends, up to its bar-off.
    let rx = spawn_frame_collector(reader);
    let mut clean_published = false;
    let mut ignored_bad: Vec<serde_json::Value> = Vec::new();
    recv_frame_until(
        &rx,
        std::time::Duration::from_secs(60),
        "the startup scan to finish",
        |v| {
            if v["method"] != "textDocument/publishDiagnostics" {
                return;
            }
            let Some(uri) = v["params"]["uri"].as_str() else {
                return;
            };
            if uri.ends_with(clean_rel) && publish_has_code(v, "CW281") {
                clean_published = true;
            }
            if uri.ends_with(ignored_rel) && publish_has_code(v, "CW281") {
                ignored_bad.push(v.clone());
            }
        },
        |v| v["method"] == "loadingBar" && v["params"]["enable"] == serde_json::Value::Bool(false),
    );
    child.kill().ok();

    assert!(
        clean_published,
        "the scan must publish CW281 for the plain file, or the test proves nothing"
    );
    assert!(
        ignored_bad.is_empty(),
        "directive must suppress CW281 in the scan publish: {ignored_bad:?}"
    );
}

#[test]
fn test_ignore_code_action_edits_the_workspace_settings() {
    // Round-trip: a CW281 diagnostic's codeAction response carries an
    // "Ignore CW281 in this workspace" action whose edit adds the code to
    // `cwtools.errors.ignore` in the workspace's .vscode/settings.json.
    const RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap(); // empty dir → index marked complete
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    let rel_path = "common/decisions/test.txt";
    let text = "x = { limit = { } }\n";
    let file_path = ws.path().join(rel_path);
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, text).unwrap();

    let ws_uri = path_uri(ws.path());
    let doc_uri = path_uri(&file_path);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "vanilla": vanilla.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &init).unwrap();
    let init_resp_str = read_response(&mut reader).expect("no init response");
    assert!(!init_resp_str.is_empty());
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({ "textDocument": { "uri": doc_uri, "languageId": "hoi4", "version": 1, "text": text } }),
        ),
    )
    .unwrap();

    let diag =
        wait_for_diag_object(&mut reader, rel_path, "CW281").expect("CW281 diagnostic published");

    let ca_req = jsonrpc_request(
        2,
        "textDocument/codeAction",
        serde_json::json!({
            "textDocument": { "uri": doc_uri },
            "range": diag["range"],
            "context": { "diagnostics": [diag] },
        }),
    );
    write_frame(&mut child, &ca_req).unwrap();
    let resp_str = read_response(&mut reader).expect("no codeAction response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(resp["id"], 2, "got: {resp_str}");
    let all = resp["result"].as_array().expect("array result");
    let action = all
        .iter()
        .find(|a| a["title"] == "Ignore CW281 in this workspace")
        .unwrap_or_else(|| panic!("no ignore action in the response: {resp_str}"));
    assert_eq!(action["kind"], "quickfix");
    assert_eq!(action["diagnostics"].as_array().unwrap().len(), 1);

    let changes = action["edit"]["changes"]
        .as_object()
        .expect("edit carries a changes map");
    assert_eq!(changes.len(), 1, "only the settings file is edited");
    let (uri, edits) = changes.iter().next().expect("one file edited");
    assert!(
        uri.ends_with(".vscode/settings.json"),
        "the edit must target the workspace settings file: {uri}"
    );
    let new_text = edits[0]["newText"].as_str().expect("whole-file replace");
    let parsed: serde_json::Value =
        serde_json::from_str(new_text).expect("settings stay valid json");
    assert_eq!(parsed["cwtools"]["errors"]["ignore"][0], "CW281");
}

#[test]
fn test_cw240_quickfix_replaces_only_the_enum_value() {
    const RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}

enums = {
    enum[mode] = { historic fantasy sandbox }
}

decision = {
    label = scalar
    mode = enum[mode]
}
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    let rel_path = "common/decisions/test.txt";
    let text = "decision = { label = \"😀\" mode = histroic } # typo\n";
    let file_path = ws.path().join(rel_path);
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, text).unwrap();

    let ws_uri = path_uri(ws.path());
    let doc_uri = path_uri(&file_path);
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": ws_uri,
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "vanilla": vanilla.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":doc_uri,"languageId":"hoi4","version":1,"text":text}}),
        ),
    )
    .unwrap();
    let diag = wait_for_diag_object(&mut reader, rel_path, "CW240")
        .expect("CW240 diagnostic published for the misspelled enum value");

    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "textDocument/codeAction",
            serde_json::json!({
                "textDocument": { "uri": doc_uri },
                "range": diag["range"],
                "context": { "diagnostics": [diag], "only": ["quickfix"] },
            }),
        ),
    )
    .unwrap();
    let response = read_response(&mut reader).expect("no codeAction response");
    child.kill().ok();

    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    let actions = response["result"].as_array().expect("array result");
    assert_eq!(
        actions.len(),
        2,
        "the fix plus the ignore action: {response}"
    );
    assert_eq!(actions[0]["title"], "Did you mean 'historic'?");
    let edits = actions[0]["edit"]["changes"]
        .get(&doc_uri)
        .and_then(serde_json::Value::as_array)
        .expect("edits for the document");
    assert_eq!(edits.len(), 1, "one text edit expected: {response}");
    let value_start = text[..text.find("histroic").unwrap()]
        .encode_utf16()
        .count() as u64;
    assert_eq!(edits[0]["range"]["start"]["line"], 0);
    assert_eq!(edits[0]["range"]["start"]["character"], value_start);
    assert_eq!(edits[0]["range"]["end"]["line"], 0);
    assert_eq!(edits[0]["range"]["end"]["character"], value_start + 8);
    assert_eq!(edits[0]["newText"], "\"historic\"");

    let units: Vec<u16> = text[..text.find('\n').unwrap()].encode_utf16().collect();
    let start = edits[0]["range"]["start"]["character"].as_u64().unwrap() as usize;
    let end = edits[0]["range"]["end"]["character"].as_u64().unwrap() as usize;
    let mut fixed = String::from_utf16(&units[..start]).expect("start is on a char boundary");
    fixed.push_str(edits[0]["newText"].as_str().unwrap());
    fixed.push_str(&String::from_utf16(&units[end..]).expect("end is on a char boundary"));
    fixed.push('\n');
    assert_eq!(
        fixed, "decision = { label = \"😀\" mode = \"historic\" } # typo\n",
        "the UTF-16 edit must replace only the enum value"
    );
}

#[test]
fn test_cw122_quickfix_removes_only_the_quotes() {
    const RULES: &str = r#"
types = {
    type[thing] = { path = "game/common/things" }
}

thing = {
    label = scalar
    iname = localisation_inline
}
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();
    let loc_dir = ws.path().join("localisation");
    std::fs::create_dir_all(&loc_dir).unwrap();
    std::fs::write(
        loc_dir.join("test_l_english.yml"),
        b"\xEF\xBB\xBFl_english:\n my_key: \"My key\"\n",
    )
    .unwrap();

    let rel_path = "common/things/test.txt";
    let text = "thing = { label = \"😀\" iname = \"my_key\" } # unnecessary\n";
    let file_path = ws.path().join(rel_path);
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, text).unwrap();

    let ws_uri = path_uri(ws.path());
    let doc_uri = path_uri(&file_path);
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": ws_uri,
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "vanilla": vanilla.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":doc_uri,"languageId":"hoi4","version":1,"text":text}}),
        ),
    )
    .unwrap();
    let diag = wait_for_diag_object(&mut reader, rel_path, "CW122")
        .expect("CW122 diagnostic published for the quoted inline key");

    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "textDocument/codeAction",
            serde_json::json!({
                "textDocument": { "uri": doc_uri },
                "range": diag["range"],
                "context": { "diagnostics": [diag], "only": ["quickfix"] },
            }),
        ),
    )
    .unwrap();
    let response = read_response(&mut reader).expect("no codeAction response");
    child.kill().ok();

    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    let actions = response["result"].as_array().expect("array result");
    assert_eq!(
        actions.len(),
        2,
        "the fix plus the ignore action: {response}"
    );
    assert_eq!(actions[0]["title"], "Remove unnecessary quotes");
    let edits = actions[0]["edit"]["changes"]
        .get(&doc_uri)
        .and_then(serde_json::Value::as_array)
        .expect("edits for the document");
    assert_eq!(edits.len(), 1, "one text edit expected: {response}");
    let value_start = text[..text.find("\"my_key\"").unwrap()]
        .encode_utf16()
        .count() as u64;
    assert_eq!(edits[0]["range"]["start"]["line"], 0);
    assert_eq!(edits[0]["range"]["start"]["character"], value_start);
    assert_eq!(edits[0]["range"]["end"]["line"], 0);
    assert_eq!(edits[0]["range"]["end"]["character"], value_start + 8);
    assert_eq!(edits[0]["newText"], "my_key");

    let units: Vec<u16> = text[..text.find('\n').unwrap()].encode_utf16().collect();
    let start = edits[0]["range"]["start"]["character"].as_u64().unwrap() as usize;
    let end = edits[0]["range"]["end"]["character"].as_u64().unwrap() as usize;
    let mut fixed = String::from_utf16(&units[..start]).expect("start is on a char boundary");
    fixed.push_str(edits[0]["newText"].as_str().unwrap());
    fixed.push_str(&String::from_utf16(&units[end..]).expect("end is on a char boundary"));
    fixed.push('\n');
    assert_eq!(
        fixed, "thing = { label = \"😀\" iname = my_key } # unnecessary\n",
        "the UTF-16 edit must remove only the quotes"
    );
}

#[test]
fn test_code_action_and_diagnostic_agree_on_non_bmp_line() {
    // The published diagnostic and the quick fix hanging off it must speak the
    // same position encoding. A non-BMP char earlier on the line makes the
    // parser's char column and the client's UTF-16 column differ, so publishing
    // the raw column applied the fix one column short — corrupting the file.
    const RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap(); // empty dir → index marked complete
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    let rel_path = "common/decisions/test.txt";
    let text = "x = { name = \"😀\" limit = { } }\n";
    let file_path = ws.path().join(rel_path);
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, text).unwrap();

    let ws_uri = path_uri(ws.path());
    let doc_uri = path_uri(&file_path);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    // No `general.positionEncodings` in the capabilities → UTF-16, as VS Code.
    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "vanilla": vanilla.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &init).unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":doc_uri,"languageId":"hoi4","version":1,"text":text}}),
        ),
    )
    .unwrap();

    let diag = wait_for_diag_object(&mut reader, rel_path, "CW281")
        .expect("CW281 diagnostic published for the empty limit");
    // The squiggle starts at `limit` in UTF-16 units (18), not chars (17).
    let limit_utf16 = text[..text.find("limit").unwrap()].encode_utf16().count() as u64;
    assert_eq!(
        diag["range"]["start"]["character"], limit_utf16,
        "diagnostic must use the negotiated encoding: {diag}"
    );

    let ca_req = jsonrpc_request(
        2,
        "textDocument/codeAction",
        serde_json::json!({
            "textDocument": { "uri": doc_uri },
            "range": diag["range"],
            "context": { "diagnostics": [diag] },
        }),
    );
    write_frame(&mut child, &ca_req).unwrap();
    let resp_str = read_response(&mut reader).expect("no codeAction response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let e = resp["result"][0]["edit"]["changes"]
        .as_object()
        .and_then(|c| c.values().next())
        .and_then(|v| v.get(0))
        .cloned()
        .expect("one text edit");
    assert_eq!(
        e["range"]["start"], diag["range"]["start"],
        "the fix must start where the squiggle does: {resp_str}"
    );

    // Apply the edit the way a UTF-16 client would: splice by code units.
    let sc = e["range"]["start"]["character"].as_u64().unwrap() as usize;
    let ec = e["range"]["end"]["character"].as_u64().unwrap() as usize;
    let nl = text.find('\n').unwrap();
    let units: Vec<u16> = text[..nl].encode_utf16().collect();
    let mut fixed = String::from_utf16(&units[..sc]).expect("start is on a char boundary");
    fixed.push_str(e["newText"].as_str().unwrap());
    fixed.push_str(&String::from_utf16(&units[ec..]).expect("end is on a char boundary"));
    fixed.push_str(&text[nl..]);
    assert_eq!(
        fixed, "x = { name = \"😀\" }\n",
        "applied edit must delete exactly the empty limit"
    );
}

#[test]
fn test_yml_outside_localisation_dir_is_not_validated_as_loc() {
    // `.yml` alone doesn't make a file localisation: the loc walker only loads
    // what lives under a `localisation` dir. Opening a CI workflow used to route
    // it into loc validation (CW255/CW256 on a GitHub Actions file), and must
    // not fall through to the game-script validator either.
    const RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap(); // empty dir → index marked complete
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    let rel_path = ".github/workflows/ci.yml";
    let text = "name: CI\non:\n  push:\n    branches: [main]\n";
    let file_path = ws.path().join(rel_path);
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, text).unwrap();

    let ws_uri = path_uri(ws.path());
    let doc_uri = path_uri(&file_path);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": ws_uri,
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "vanilla": vanilla.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":doc_uri,"languageId":"yaml","version":1,"text":text}}),
        ),
    )
    .unwrap();

    let codes = diags_for(&mut reader, "ci.yml", 1).expect("diagnostics published for the yml");
    assert!(
        codes.is_empty(),
        "a yml outside any localisation dir must report nothing, got: {codes:?}"
    );

    // Nor may it fall through to the game-script validator: the same yaml
    // sitting inside a rule-matched game dir must still report nothing rather
    // than being parsed (and indexed) as Paradox script.
    let in_tree_rel = "common/decisions/notes.yml";
    let in_tree_path = ws.path().join(in_tree_rel);
    std::fs::create_dir_all(in_tree_path.parent().unwrap()).unwrap();
    std::fs::write(&in_tree_path, text).unwrap();
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":path_uri(&in_tree_path),"languageId":"yaml","version":1,"text":text}}),
        ),
    )
    .unwrap();
    let in_tree_codes =
        diags_for(&mut reader, in_tree_rel, 1).expect("diagnostics published for the in-tree yml");
    child.kill().ok();
    assert!(
        in_tree_codes.is_empty(),
        "a yml in a game dir is still not game script, got: {in_tree_codes:?}"
    );
}

// ── Inlay hints: loc titles ──────────────────────────────────────────────────

#[test]
fn test_inlay_hint_shows_loc_title_after_known_id() {
    // End-to-end: the server advertises the inlayHint capability, and a request
    // over a script that references a known decision id (with a localised title)
    // returns one hint carrying that title, positioned just past the id.
    const RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap(); // empty dir → index marked complete
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    // The definition (indexed as a `decision` instance named `my_decision`).
    let defs = ws.path().join("common/decisions/defs.txt");
    std::fs::create_dir_all(defs.parent().unwrap()).unwrap();
    std::fs::write(&defs, "my_decision = { cost = 1 }\n").unwrap();

    // Its localised title.
    let loc_dir = ws.path().join("localisation");
    std::fs::create_dir_all(&loc_dir).unwrap();
    let mut bytes: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice("l_english:\n my_decision:0 \"My Decision\"\n".as_bytes());
    std::fs::write(loc_dir.join("test_l_english.yml"), &bytes).unwrap();

    // The opened script referencing the id as a value on line 1.
    let rel_path = "common/decisions/use.txt";
    let text = "wrapper = {\n    ref = my_decision\n}\n";
    let use_path = ws.path().join(rel_path);
    std::fs::write(&use_path, text).unwrap();

    let ws_uri = path_uri(ws.path());
    let doc_uri = path_uri(&use_path);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "vanilla": vanilla.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &init).unwrap();
    let init_resp_str = read_response(&mut reader).expect("no init response");
    let init_resp: serde_json::Value = serde_json::from_str(&init_resp_str).unwrap();
    // Capability advertised.
    assert_eq!(
        init_resp["result"]["capabilities"]["inlayHintProvider"], true,
        "inlayHint capability must be advertised: {init_resp_str}"
    );

    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":doc_uri,"languageId":"hoi4","version":1,"text":text}}),
        ),
    )
    .unwrap();

    // Poll the inlay request until the loc map + type index are populated.
    let mut hint = serde_json::Value::Null;
    for attempt in 0..30 {
        let req = jsonrpc_request(
            2 + attempt,
            "textDocument/inlayHint",
            serde_json::json!({
                "textDocument": { "uri": doc_uri },
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 100, "character": 0 },
                },
            }),
        );
        write_frame(&mut child, &req).unwrap();
        let resp_str = read_response(&mut reader).expect("no inlayHint response");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        if let Some(arr) = resp["result"].as_array()
            && !arr.is_empty()
        {
            hint = arr[0].clone();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    child.kill().ok();

    assert_eq!(hint["label"], "My Decision", "got: {hint}");
    // Anchored just past `my_decision` on line 1: "    ref = my_decision" ends at col 21.
    assert_eq!(hint["position"]["line"], 1, "got: {hint}");
    assert_eq!(hint["position"]["character"], 21, "got: {hint}");
    assert_eq!(hint["paddingLeft"], true, "got: {hint}");
}

// ── Inlay hints: resolved scopes ─────────────────────────────────────────────

#[test]
fn test_inlay_hint_shows_rule_aware_scope() {
    const RULES: &str = r#"
types = {
    type[decision] = { path = "common/decisions" }
}
scopes = {
    Country = { aliases = { country } }
    Character = { aliases = { character } }
}
decision = {
    ## push_scope = character
    custom = {
        add = int
    }
}
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("test_rules.cwt"), RULES).unwrap();
    let rel_path = "common/decisions/scope.txt";
    let text = "my_decision = {\n    custom = {\n        add = 1\n    }\n}\n";
    let path = ws.path().join(rel_path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    let ws_uri = path_uri(ws.path());
    let doc_uri = path_uri(&path);
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": ws_uri,
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "inlayHintsScopes": true,
                    "inlayHintsLocTitles": false,
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).unwrap();
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {"uri": doc_uri, "languageId": "hoi4", "version": 1, "text": text}
            }),
        ),
    )
    .unwrap();
    wait_for_diagnostics(&mut reader, rel_path);
    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "textDocument/inlayHint",
            serde_json::json!({
                "textDocument": {"uri": doc_uri},
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 10, "character": 0}}
            }),
        ),
    )
    .unwrap();
    let response = read_response(&mut reader).unwrap();
    child.kill().ok();
    let value: serde_json::Value = serde_json::from_str(&response).unwrap();
    let hints = value["result"]
        .as_array()
        .unwrap_or_else(|| panic!("inlay result array: {value}"));
    assert_eq!(hints.len(), 1, "got: {value}");
    assert_eq!(hints[0]["label"], "→ character", "got: {value}");
    assert_eq!(hints[0]["position"]["line"], 1, "got: {value}");
    assert_eq!(hints[0]["position"]["character"], 13, "got: {value}");
    assert_eq!(hints[0]["tooltip"], serde_json::Value::Null, "got: {value}");
    assert_eq!(
        hints[0]["textEdits"],
        serde_json::Value::Null,
        "got: {value}"
    );
}

// ── getGraphData ─────────────────────────────────────────────────────────────

/// A focus type whose instances reference each other, plus a decision type that
/// references a focus. Both reference keys sit at depth 1 of a type rule
/// (`relative_position_id = <focus>` is copied from the real HOI4
/// national_focus.cwt): that is the shape `is_type_ref_leaf` /
/// `build_type_ref_keys` classify, so it is what the workspace reference index
/// — and therefore the graph — can see.
const GRAPH_RULES: &str = r#"
types = {
    type[focus] = {
        path = "game/common/national_focus"
        name_field = "id"
    }
    type[decision] = { path = "game/common/decisions" }
}
focus = {
    id = scalar
    ## cardinality = 0..1
    cost = int
    ## cardinality = 0..1
    relative_position_id = <focus>
}
decision = {
    ## cardinality = 0..1
    has_focus = <focus>
}
"#;

const GRAPH_FILES: &[(&str, &str)] = &[
    (
        "common/national_focus/tree.txt",
        "focus = {\n\tid = FOCUS_A\n\tcost = 1\n}\n\
         focus = {\n\tid = FOCUS_B\n\tcost = 1\n\trelative_position_id = FOCUS_A\n}\n\
         focus = {\n\tid = FOCUS_C\n\tcost = 1\n\trelative_position_id = FOCUS_B\n}\n",
    ),
    (
        "common/decisions/d.txt",
        "my_decision = {\n\thas_focus = FOCUS_C\n}\n",
    ),
];

/// Assert `value` is exactly the `GraphData` the client declares in
/// cwtools-vscode `client/common/graphTypes.ts`: an array of `GraphNode`, every
/// documented member typed as declared, every required member present, and no
/// member the client's interfaces don't know about.
fn assert_graph_data_shape(value: &serde_json::Value) {
    let nodes = value.as_array().expect("GraphData must be an array");
    assert!(!nodes.is_empty(), "expected at least one node");
    for node in nodes {
        let obj = node.as_object().expect("GraphNode must be an object");
        for (key, member) in obj {
            let ok = match key.as_str() {
                "id" | "entityType" | "name" | "entityTypeDisplayName" | "abbreviation" => {
                    member.is_string()
                }
                "isPrimary" => member.is_boolean(),
                "references" => member
                    .as_array()
                    .is_some_and(|refs| refs.iter().all(is_graph_reference)),
                "location" => is_graph_location(member),
                "details" => member
                    .as_array()
                    .is_some_and(|rows| rows.iter().all(is_graph_node_detail)),
                _ => false,
            };
            assert!(
                ok,
                "GraphNode member `{key}` = {member} is not graphTypes.ts"
            );
        }
        for required in ["id", "references", "isPrimary", "entityType"] {
            assert!(
                obj.contains_key(required),
                "GraphNode is missing the required member `{required}`: {node}"
            );
        }
    }
}

fn is_graph_reference(value: &serde_json::Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.contains_key("key")
        && obj.contains_key("isOutgoing")
        && obj.iter().all(|(k, v)| match k.as_str() {
            "key" | "label" => v.is_string(),
            "isOutgoing" => v.is_boolean(),
            _ => false,
        })
}

fn is_graph_location(value: &serde_json::Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.len() == 3
        && obj
            .get("filename")
            .is_some_and(serde_json::Value::is_string)
        && obj.get("line").is_some_and(serde_json::Value::is_u64)
        && obj.get("column").is_some_and(serde_json::Value::is_u64)
}

fn is_graph_node_detail(value: &serde_json::Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.len() == 2
        && obj.get("key").is_some_and(serde_json::Value::is_string)
        && obj
            .get("values")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|vs| vs.iter().all(serde_json::Value::is_string))
}

/// Every `(source, target)` edge the webview would build from the payload.
fn graph_edges(nodes: &[serde_json::Value]) -> Vec<(String, String)> {
    let mut edges = Vec::new();
    for node in nodes {
        let id = node["id"].as_str().unwrap().to_string();
        for r in node["references"].as_array().unwrap() {
            let key = r["key"].as_str().unwrap().to_string();
            if r["isOutgoing"].as_bool().unwrap() {
                edges.push((id.clone(), key));
            } else {
                edges.push((key, id.clone()));
            }
        }
    }
    edges.sort();
    edges
}

/// Spawn a server over a workspace of `files`, then run `getGraphData` with
/// `arguments`, polling until the workspace scan has populated the index.
fn graph_data_response(files: &[(&str, &str)], arguments: serde_json::Value) -> serde_json::Value {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GRAPH_RULES).unwrap();
    for (rel, content) in files {
        let p = ws.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
    }

    let (mut child, mut reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());
    let mut response = serde_json::Value::Null;
    for attempt in 0..20 {
        write_frame(
            &mut child,
            &jsonrpc_request(
                600 + attempt,
                "workspace/executeCommand",
                serde_json::json!({ "command": "getGraphData", "arguments": arguments }),
            ),
        )
        .unwrap();
        let raw = read_response(&mut reader).expect("no getGraphData response");
        response = serde_json::from_str(&raw).unwrap();
        // The reference index lands with the async workspace scan; a graph that
        // is still only seeds means the scan hasn't finished writing it.
        let settled = response["result"].as_array().is_none_or(|nodes| {
            nodes
                .iter()
                .any(|n| !n["references"].as_array().unwrap().is_empty())
        });
        if settled {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    child.kill().ok();
    response
}

#[test]
fn test_get_graph_data_is_advertised_in_capabilities() {
    // graphAvailability.ts greys the whole graph feature out unless this exact
    // name is in executeCommandProvider.commands.
    let tmp = tempfile::tempdir().unwrap();
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(tmp.path()),
                "capabilities": {}
            }),
        ),
    )
    .unwrap();
    let resp_str = read_response(&mut reader).expect("no init response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let commands = resp["result"]["capabilities"]["executeCommandProvider"]["commands"]
        .as_array()
        .expect("executeCommandProvider.commands");
    assert!(
        commands.contains(&serde_json::Value::String("getGraphData".to_string())),
        "getGraphData not advertised: {commands:?}"
    );
}

#[test]
fn test_get_graph_data_returns_client_graph_shape() {
    let response = graph_data_response(GRAPH_FILES, serde_json::json!(["focus", 3]));
    assert!(
        response["error"].is_null(),
        "getGraphData failed: {}",
        response["error"]
    );
    assert_graph_data_shape(&response["result"]);

    let nodes = response["result"].as_array().unwrap();
    let mut ids: Vec<&str> = nodes.iter().map(|n| n["id"].as_str().unwrap()).collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        ["FOCUS_A", "FOCUS_B", "FOCUS_C", "my_decision"],
        "got: {}",
        response["result"]
    );

    // The prerequisite chain, plus the decision that reaches it at depth 1.
    assert_eq!(
        graph_edges(nodes),
        [
            ("FOCUS_B".to_string(), "FOCUS_A".to_string()),
            ("FOCUS_C".to_string(), "FOCUS_B".to_string()),
            ("my_decision".to_string(), "FOCUS_C".to_string()),
        ],
        "got: {}",
        response["result"]
    );

    let focus_b = nodes.iter().find(|n| n["id"] == "FOCUS_B").unwrap();
    assert_eq!(focus_b["isPrimary"], true);
    assert_eq!(focus_b["entityType"], "focus");
    assert_eq!(focus_b["entityTypeDisplayName"], "Focus");
    // 1-based line/column pointing at the `focus = {` that opens FOCUS_B.
    assert_eq!(focus_b["location"]["line"], 5, "got: {focus_b}");
    assert_eq!(focus_b["location"]["column"], 1, "got: {focus_b}");
    assert!(
        focus_b["location"]["filename"]
            .as_str()
            .unwrap()
            .ends_with("common/national_focus/tree.txt"),
        "got: {focus_b}"
    );

    // A related type joins the graph but is not primary, and its crossing edge
    // is labelled with the type it points at.
    let decision = nodes.iter().find(|n| n["id"] == "my_decision").unwrap();
    assert_eq!(decision["isPrimary"], false);
    assert_eq!(decision["entityType"], "decision");
    assert_eq!(decision["references"][0]["label"], "focus");

    // Nothing was dropped, so no node carries a truncation notice.
    assert!(
        nodes.iter().all(|n| n["details"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["key"] != "truncated")),
        "unexpected truncation: {}",
        response["result"]
    );
}

#[test]
fn test_get_graph_data_depth_bounds_the_walk() {
    // Depth 1 reaches the decision that references FOCUS_C directly. The focus
    // instances are all seeds, so what depth prunes here is the referring type.
    let response = graph_data_response(GRAPH_FILES, serde_json::json!(["focus", 1]));
    let nodes = response["result"].as_array().unwrap();
    assert_eq!(nodes.len(), 4, "got: {}", response["result"]);

    // A decision is only reachable through a focus, so seeding on `decision`
    // with depth 1 never walks back out to the focuses.
    let response = graph_data_response(GRAPH_FILES, serde_json::json!(["decision", 1]));
    let nodes = response["result"].as_array().unwrap();
    let ids: Vec<&str> = nodes.iter().map(|n| n["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["my_decision"], "got: {}", response["result"]);
}

#[test]
fn test_get_graph_data_rejects_bad_requests() {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GRAPH_RULES).unwrap();
    for (rel, content) in GRAPH_FILES {
        let p = ws.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
    }
    let (mut child, mut reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());

    // (arguments, a phrase the error message must name)
    let cases: [(serde_json::Value, &str); 6] = [
        (serde_json::json!([]), "entity type"),
        (serde_json::json!([42, 3]), "entity type"),
        (serde_json::json!(["focus"]), "missing depth"),
        (serde_json::json!(["focus", 0]), "at least 1"),
        (serde_json::json!(["focus", -2]), "at least 1"),
        (serde_json::json!(["not_a_type", 3]), "unknown entity type"),
    ];
    for (i, (arguments, phrase)) in cases.iter().enumerate() {
        write_frame(
            &mut child,
            &jsonrpc_request(
                700 + i as i64,
                "workspace/executeCommand",
                serde_json::json!({ "command": "getGraphData", "arguments": arguments }),
            ),
        )
        .unwrap();
        let raw = read_response(&mut reader).expect("no getGraphData response");
        let resp: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(
            resp["result"].is_null(),
            "{arguments} should not have produced a result: {resp}"
        );
        let message = resp["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(phrase),
            "{arguments}: error should name `{phrase}`, got {resp}"
        );
        assert_eq!(resp["error"]["code"], -32602, "{arguments}: got {resp}");
    }
    child.kill().ok();
}

#[test]
fn test_get_graph_data_on_empty_workspace_reports_not_ready() {
    // No script files at all: the command must name the problem rather than
    // hand the webview an empty array.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GRAPH_RULES).unwrap();
    let (mut child, mut reader) = storm_server(ws.path(), rules_dir.path(), vanilla.path());

    write_frame(
        &mut child,
        &jsonrpc_request(
            720,
            "workspace/executeCommand",
            serde_json::json!({ "command": "getGraphData", "arguments": ["focus", 3] }),
        ),
    )
    .unwrap();
    let raw = read_response(&mut reader).expect("no getGraphData response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(resp["result"].is_null(), "got: {resp}");
    let message = resp["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("index is empty") || message.contains("no instances"),
        "got: {resp}"
    );
}

// ── Semantic tokens, document colours, source.fixAll, $/progress ─────────────

/// Rules for the editor-capability round-trips below: an entity with a colour
/// block in both conventions, a typed reference, an enum, a localisation field
/// and a trigger alias.
const EDITOR_RULES: &str = r#"
types = {
    type[focus] = {
        path = "game/common/national_focus"
    }
    type[ideology] = {
        path = "game/common/ideologies"
    }
}
focus = {
    id = scalar
    cost = int
    text = localisation
    mood = enum[mood]
    prerequisite = <focus>
    available = {
        alias_name[trigger] = alias_match_left[trigger]
    }
}
ideology = {
    color = {
        ## cardinality = 3..3
        int
    }
    color = {
        ## cardinality = 3..3
        float
    }
    dynamic_faction_names = {
        ## cardinality = 3..3
        int
    }
}
alias[trigger:has_completed_focus] = <focus>
alias[trigger:always] = bool
enums = {
    enum[mood] = {
        calm
        angry
    }
}
"#;

/// Boot a server on a temp workspace with `EDITOR_RULES`, open every file, and
/// hand back the live child plus its reader so the caller can issue requests.
fn editor_server(
    files: &[(&str, &str)],
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    std::process::Child,
    BufReader<std::process::ChildStdout>,
) {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("editor_rules.cwt"), EDITOR_RULES).unwrap();
    for (rel, content) in files {
        let p = ws.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
    }

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": path_uri(ws.path()),
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
            }
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");

    for (rel, content) in files {
        let body = jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": path_uri(ws.path().join(rel)),
                    "languageId": "hoi4",
                    "version": 1,
                    "text": content,
                }
            }),
        );
        write_frame(&mut child, &body).unwrap();
        wait_for_diagnostics(&mut reader, rel);
    }
    (ws, rules_dir, child, reader)
}

/// Drain frames until the `publishDiagnostics` for `rel_path` arrives, returning
/// the whole diagnostics array (the code-action tests need the payloads, not
/// just the readiness signal `wait_for_diagnostics` gives).
fn wait_for_diags(
    reader: &mut BufReader<std::process::ChildStdout>,
    rel_path: &str,
) -> Option<Vec<serde_json::Value>> {
    for _ in 0..2000 {
        let raw = read_frame(reader).ok()?;
        if raw.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        if v["method"] == "textDocument/publishDiagnostics"
            && v["params"]["uri"]
                .as_str()
                .is_some_and(|u| u.ends_with(rel_path))
        {
            return v["params"]["diagnostics"].as_array().cloned();
        }
    }
    None
}

/// Decode the delta-encoded `data` array back into absolute
/// `(line, start, length, type, modifiers)` tuples — exactly what a client does.
fn decode_semantic_tokens(data: &[serde_json::Value]) -> Vec<(u32, u32, u32, u32, u32)> {
    let nums: Vec<u32> = data.iter().map(|v| v.as_u64().unwrap() as u32).collect();
    assert_eq!(nums.len() % 5, 0, "the stream must be quintuples");
    let mut out = Vec::new();
    let (mut line, mut start) = (0u32, 0u32);
    for c in nums.chunks(5) {
        line += c[0];
        start = if c[0] == 0 { start + c[1] } else { c[1] };
        out.push((line, start, c[2], c[3], c[4]));
    }
    out
}

#[test]
fn test_semantic_tokens_full_returns_a_decodable_classified_stream() {
    let text = "\
# a focus tree
my_focus = {
    id = my_focus
    cost = 10
    mood = calm
    text = focus_title_key
    available = {
        has_completed_focus = other_focus
        always = yes
    }
}
";
    let rel = "common/national_focus/tree.txt";
    let (ws, _rules, mut child, mut reader) = editor_server(&[(rel, text)]);

    let body = jsonrpc_request(
        2,
        "textDocument/semanticTokens/full",
        serde_json::json!({
            "textDocument": { "uri": path_uri(ws.path().join(rel)) },
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let raw = read_response(&mut reader).expect("no semanticTokens response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(resp["id"], 2, "got: {raw}");
    let data = resp["result"]["data"].as_array().expect("token data");
    let tokens = decode_semantic_tokens(data);
    println!("semanticTokens/full data = {}", resp["result"]["data"]);
    println!("decoded (line, start, len, type, mods) = {tokens:#?}");

    // Legend indices, from `semantic::TOKEN_TYPES`.
    const COMMENT: u32 = 0;
    const PROPERTY: u32 = 1;
    const OPERATOR: u32 = 2;
    const NUMBER: u32 = 3;
    const STRING: u32 = 4;
    const KEYWORD: u32 = 5;
    const TYPE: u32 = 6;
    const ENUM_MEMBER: u32 = 7;
    const FUNCTION: u32 = 10;

    let find = |line: u32, start: u32| {
        tokens
            .iter()
            .find(|t| t.0 == line && t.1 == start)
            .copied()
            .unwrap_or_else(|| panic!("no token at {line}:{start} in {tokens:#?}"))
    };

    // Every token must be within its line and non-empty — the drift symptom.
    let lines: Vec<&str> = text.lines().collect();
    for (line, start, length, ..) in &tokens {
        let width = lines[*line as usize].chars().count() as u32;
        assert!(
            *length > 0 && start + length <= width,
            "token ({line},{start},{length}) runs past line {line} ({width} chars)"
        );
    }

    assert_eq!(find(0, 0).3, COMMENT, "the leading comment");
    assert_eq!(find(0, 0).2, 14, "comment length");
    assert_eq!(find(1, 0).3, PROPERTY, "the root key");
    assert_eq!(find(1, 9).3, OPERATOR);
    assert_eq!(find(3, 11).3, NUMBER, "cost = 10");
    assert_eq!(find(4, 11).3, ENUM_MEMBER, "mood = calm resolves the enum");
    assert_eq!(
        find(5, 11).3,
        STRING,
        "text = <localisation> stays a string"
    );
    assert_eq!(
        find(7, 8).3,
        FUNCTION,
        "has_completed_focus resolves through alias[trigger:…]"
    );
    assert_eq!(
        find(7, 30).3,
        TYPE,
        "its value resolves to a <focus> instance"
    );
    assert_eq!(find(8, 17).3, KEYWORD, "always = yes");
}

#[test]
fn test_semantic_tokens_range_returns_only_the_requested_lines() {
    // Two entities; the request covers the second one's body only.
    let text = "\
first_focus = {
    id = first_focus
    cost = 10
}
second_focus = {
    id = second_focus
    cost = 20
}
";
    let rel = "common/national_focus/tree.txt";
    let (ws, _rules, mut child, mut reader) = editor_server(&[(rel, text)]);

    let body = jsonrpc_request(
        2,
        "textDocument/semanticTokens/range",
        serde_json::json!({
            "textDocument": { "uri": path_uri(ws.path().join(rel)) },
            // Exclusive end on column 0: lines 5 and 6, not 7.
            "range": {
                "start": { "line": 5, "character": 0 },
                "end": { "line": 7, "character": 0 },
            },
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let raw = read_response(&mut reader).expect("no semanticTokens/range response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(resp["id"], 2, "got: {raw}");
    let data = resp["result"]["data"].as_array().expect("token data");
    let tokens = decode_semantic_tokens(data);
    println!("decoded (line, start, len, type, mods) = {tokens:#?}");

    assert!(!tokens.is_empty(), "range should still classify: {raw}");
    assert!(
        tokens.iter().all(|t| (5..=6).contains(&t.0)),
        "tokens outside the requested range: {tokens:#?}"
    );
    // `id`, `=`, `second_focus`, `cost`, `=`, `20`.
    assert_eq!(tokens.len(), 6, "{tokens:#?}");
}

#[test]
fn test_semantic_tokens_range_end_column_zero_is_exclusive() {
    // `first = 1` on line 0, `second = 2` on line 1. A range ending at 1:0
    // covers line 0 only.
    let text = "first = 1\nsecond = 2\n";
    let rel = "common/national_focus/tree.txt";
    let (ws, _rules, mut child, mut reader) = editor_server(&[(rel, text)]);

    let body = jsonrpc_request(
        2,
        "textDocument/semanticTokens/range",
        serde_json::json!({
            "textDocument": { "uri": path_uri(ws.path().join(rel)) },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 1, "character": 0 },
            },
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let raw = read_response(&mut reader).expect("no semanticTokens/range response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let data = resp["result"]["data"].as_array().expect("token data");
    let tokens = decode_semantic_tokens(data);
    assert!(
        tokens.iter().all(|t| t.0 == 0),
        "line 1 is past the exclusive end: {tokens:#?}"
    );
    assert_eq!(tokens.len(), 3, "{tokens:#?}");
}

#[test]
fn test_semantic_tokens_skip_loc_and_cwt_files() {
    let files = [
        (
            "localisation/english/x_l_english.yml",
            "l_english:\n key:0 \"v\"\n",
        ),
        (
            "common/national_focus/tree.txt",
            "my_focus = {\n    id = my_focus\n}\n",
        ),
    ];
    let (ws, _rules, mut child, mut reader) = editor_server(&files);
    for (i, rel) in ["localisation/english/x_l_english.yml"].iter().enumerate() {
        let body = jsonrpc_request(
            30 + i as i64,
            "textDocument/semanticTokens/full",
            serde_json::json!({ "textDocument": { "uri": path_uri(ws.path().join(rel)) } }),
        );
        write_frame(&mut child, &body).unwrap();
        let raw = read_response(&mut reader).expect("no response");
        let resp: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(
            resp["result"].is_null(),
            "{rel} should get no tokens: {raw}"
        );
    }
    child.kill().ok();
}

#[test]
fn test_document_color_and_presentation_round_trip_both_conventions() {
    let text = "\
communism = {
    color = { 51 102 153 }
    dynamic_faction_names = { 0.2 0.4 0.6 }
}
";
    let rel = "common/ideologies/00_ideologies.txt";
    let (ws, _rules, mut child, mut reader) = editor_server(&[(rel, text)]);
    let doc_uri = path_uri(ws.path().join(rel));

    let body = jsonrpc_request(
        2,
        "textDocument/documentColor",
        serde_json::json!({ "textDocument": { "uri": doc_uri } }),
    );
    write_frame(&mut child, &body).unwrap();
    let raw = read_response(&mut reader).expect("no documentColor response");
    let resp: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(resp["id"], 2, "got: {raw}");
    println!("documentColor result = {}", resp["result"]);
    let colours = resp["result"].as_array().expect("colour array").clone();
    assert_eq!(colours.len(), 2, "two colour leaves: {}", resp["result"]);

    // Swatches agree: `{ 51 102 153 }` and `{ 0.2 0.4 0.6 }` are the same colour.
    for c in &colours {
        let (r, g, b) = (
            c["color"]["red"].as_f64().unwrap(),
            c["color"]["green"].as_f64().unwrap(),
            c["color"]["blue"].as_f64().unwrap(),
        );
        assert!((r - 0.2).abs() < 0.01, "red {r} in {c}");
        assert!((g - 0.4).abs() < 0.01, "green {g} in {c}");
        assert!((b - 0.6).abs() < 0.01, "blue {b} in {c}");
        assert_eq!(c["color"]["alpha"], 1.0);
    }

    // The ranges must cover exactly the literal, not the key.
    let lines: Vec<&str> = text.lines().collect();
    let slice = |c: &serde_json::Value| {
        let line = c["range"]["start"]["line"].as_u64().unwrap() as usize;
        let s = c["range"]["start"]["character"].as_u64().unwrap() as usize;
        let e = c["range"]["end"]["character"].as_u64().unwrap() as usize;
        lines[line].chars().skip(s).take(e - s).collect::<String>()
    };
    let mut spans: Vec<String> = colours.iter().map(slice).collect();
    spans.sort();
    assert_eq!(spans, vec!["{ 0.2 0.4 0.6 }", "{ 51 102 153 }"]);

    // colorPresentation must write back the convention it read. Ask each range
    // for the SAME colour it already holds and check the spelling survives.
    for (i, c) in colours.iter().enumerate() {
        let body = jsonrpc_request(
            10 + i as i64,
            "textDocument/colorPresentation",
            serde_json::json!({
                "textDocument": { "uri": doc_uri },
                "color": c["color"],
                "range": c["range"],
            }),
        );
        write_frame(&mut child, &body).unwrap();
        let raw = read_response(&mut reader).expect("no colorPresentation response");
        let resp: serde_json::Value = serde_json::from_str(&raw).unwrap();
        println!("colorPresentation for {} = {}", slice(c), resp["result"]);
        let label = resp["result"][0]["label"].as_str().expect("a presentation");
        let edit = resp["result"][0]["textEdit"]["newText"]
            .as_str()
            .expect("a text edit");
        assert_eq!(label, edit);
        assert_eq!(resp["result"][0]["textEdit"]["range"], c["range"]);
        if slice(c).contains('.') {
            assert_eq!(edit, "{ 0.200 0.400 0.600 }", "floats stay floats");
        } else {
            assert_eq!(edit, "{ 51 102 153 }", "bytes stay bytes");
        }
    }
    child.kill().ok();
}

#[test]
fn test_color_presentation_writes_a_new_pick_in_the_source_convention() {
    let text = "communism = {\n    color = { 51 102 153 }\n}\n";
    let rel = "common/ideologies/00_ideologies.txt";
    let (ws, _rules, mut child, mut reader) = editor_server(&[(rel, text)]);
    let doc_uri = path_uri(ws.path().join(rel));

    // Pure orange picked in the editor over an int-convention literal.
    let body = jsonrpc_request(
        5,
        "textDocument/colorPresentation",
        serde_json::json!({
            "textDocument": { "uri": doc_uri },
            "color": { "red": 1.0, "green": 0.5, "blue": 0.0, "alpha": 1.0 },
            "range": {
                "start": { "line": 1, "character": 12 },
                "end": { "line": 1, "character": 26 },
            },
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let raw = read_response(&mut reader).expect("no colorPresentation response");
    child.kill().ok();
    let resp: serde_json::Value = serde_json::from_str(&raw).unwrap();
    println!("colorPresentation (new pick) = {}", resp["result"]);
    assert_eq!(
        resp["result"][0]["textEdit"]["newText"], "{ 255 128 0 }",
        "an int-convention literal must not be rewritten as floats: {raw}"
    );
}

#[test]
fn test_initialize_advertises_the_new_capabilities() {
    let tmp = tempfile::tempdir().unwrap();
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(tmp.path()),
                "capabilities": { "window": { "workDoneProgress": true } },
            }),
        ),
    )
    .unwrap();
    let raw = read_response(&mut reader).expect("no init response");
    child.kill().ok();
    let resp: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let caps = &resp["result"]["capabilities"];
    println!(
        "semanticTokensProvider = {}",
        caps["semanticTokensProvider"]
    );
    println!("colorProvider = {}", caps["colorProvider"]);
    println!("codeActionProvider = {}", caps["codeActionProvider"]);
    println!("workspace = {}", caps["workspace"]);

    assert_eq!(caps["semanticTokensProvider"]["full"]["delta"], true);
    let types = caps["semanticTokensProvider"]["legend"]["tokenTypes"]
        .as_array()
        .expect("a token legend");
    assert_eq!(
        types[0], "comment",
        "legend order is the wire encoding; index 0 must stay `comment`"
    );
    assert_eq!(types.len(), 11);
    assert_eq!(
        caps["semanticTokensProvider"]["legend"]["tokenModifiers"][0],
        "declaration"
    );
    assert_eq!(caps["colorProvider"], true);
    let kinds = caps["codeActionProvider"]["codeActionKinds"]
        .as_array()
        .expect("code action kinds");
    assert!(
        kinds.iter().any(|k| k == "source.fixAll"),
        "editor.codeActionsOnSave needs source.fixAll advertised: {kinds:?}"
    );
    assert_eq!(caps["workspace"]["workspaceFolders"]["supported"], true);
    assert_eq!(
        caps["workspace"]["workspaceFolders"]["changeNotifications"],
        true
    );
}

#[test]
fn test_semantic_tokens_refresh_waits_for_advertised_client_support() {
    let (ws, _) = boundary_workspace();
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let reader = BufReader::new(child.stdout.take().unwrap());
    let stdin = child.stdin.take().unwrap();
    let workspace = path_uri(ws.path());

    let result = run_with_deadline(stdin, reader, 5, move |stdin, reader| {
        write_frame_to(
            stdin,
            &jsonrpc_request(
                1,
                "initialize",
                serde_json::json!({
                    "processId": std::process::id(),
                    "rootUri": workspace,
                    "capabilities": {
                        "workspace": { "semanticTokens": { "refreshSupport": true } }
                    },
                    "initializationOptions": { "language": "hoi4" }
                }),
            ),
        )
        .expect("initialize request");
        read_response(reader).expect("initialize response");
        write_frame_to(
            stdin,
            &jsonrpc_notification("initialized", serde_json::json!({})),
        )
        .expect("initialized notification");

        let mut refreshed = false;
        for _ in 0..5000 {
            let raw = read_frame(reader).expect("server closed during scan");
            let frame: serde_json::Value = serde_json::from_str(&raw).expect("LSP frame");
            if frame["method"] == "workspace/semanticTokens/refresh" {
                write_frame_to(
                    stdin,
                    &serde_json::json!({ "jsonrpc": "2.0", "id": frame["id"], "result": null })
                        .to_string(),
                )
                .expect("semantic refresh response");
                refreshed = true;
            }
            if frame["method"] == "loadingBar"
                && frame["params"]["enable"] == serde_json::Value::Bool(false)
            {
                return refreshed;
            }
        }
        false
    });
    child.kill().ok();
    child.wait().ok();

    assert_eq!(result, Some(true), "scan did not request semantic refresh");
}

#[test]
fn test_source_fix_all_applies_every_fixable_diagnostic_at_once() {
    // Two empty `limit = { }` blocks -> two CW281 diagnostics, each carrying a
    // fix. `source.fixAll` must return ONE action holding both edits, and
    // applying them must give the same file the CLI `fix --apply` would.
    const RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    let rel = "common/decisions/test.txt";
    let text = "a = { limit = { } }\nb = { limit = { } }\n";
    let p = ws.path().join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, text).unwrap();
    let doc_uri = path_uri(&p);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(ws.path()),
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "vanilla": vanilla.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":doc_uri,"languageId":"hoi4","version":1,"text":text}}),
        ),
    )
    .unwrap();

    // Capture the published diagnostics: the client round-trips them back on the
    // codeAction request, and the fix payloads ride along in `data`.
    let diagnostics = wait_for_diags(&mut reader, rel).expect("diagnostics for the document");
    let fixable: Vec<&serde_json::Value> = diagnostics
        .iter()
        .filter(|d| d["data"].is_object())
        .collect();
    println!("fixable diagnostics = {}", fixable.len());
    assert_eq!(
        fixable.len(),
        2,
        "two empty limits -> two fixable diagnostics: {diagnostics:#?}"
    );

    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "textDocument/codeAction",
            serde_json::json!({
                "textDocument": { "uri": doc_uri },
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 2, "character": 0 },
                },
                "context": { "diagnostics": diagnostics, "only": ["source.fixAll"] },
            }),
        ),
    )
    .unwrap();
    let raw = read_response(&mut reader).expect("no codeAction response");
    child.kill().ok();
    let resp: serde_json::Value = serde_json::from_str(&raw).unwrap();
    println!("source.fixAll action = {}", resp["result"]);
    let actions = resp["result"].as_array().expect("actions");
    assert_eq!(actions.len(), 1, "only source.fixAll was requested: {raw}");
    assert_eq!(actions[0]["kind"], "source.fixAll");
    assert_eq!(actions[0]["title"], "Fix all (2 auto-fixable)");
    assert_eq!(
        actions[0]["diagnostics"].as_array().map(Vec::len),
        Some(2),
        "the action claims both diagnostics"
    );
    let edits = actions[0]["edit"]["changes"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(edits.len(), 2, "one edit per fixable diagnostic");

    // Apply the edits (line-descending, so earlier columns stay valid) and check
    // the result: both empty limits removed.
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut resolved: Vec<(usize, usize, usize, String)> = edits
        .iter()
        .map(|e| {
            (
                e["range"]["start"]["line"].as_u64().unwrap() as usize,
                e["range"]["start"]["character"].as_u64().unwrap() as usize,
                e["range"]["end"]["character"].as_u64().unwrap() as usize,
                e["newText"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    resolved.sort_by(|a, b| b.cmp(a));
    for (line, sc, ec, new_text) in resolved {
        let chars: Vec<char> = lines[line].chars().collect();
        let mut out: String = chars[..sc].iter().collect();
        out.push_str(&new_text);
        out.extend(chars[ec..].iter());
        lines[line] = out;
    }
    let fixed = format!("{}\n", lines.join("\n"));
    println!("fixed source = {fixed:?}");
    assert_eq!(fixed, "a = { }\nb = { }\n");
}

// ── Create missing localisation key (CW100) / fixAllWorkspace ────────────────

/// Like [`read_response`], but answers any `workspace/applyEdit` request the
/// server sends before returning the next real response — `fixAllWorkspace`
/// is the server's first server-initiated request, so the harness has never
/// had to act as an LSP *client* before. A request (has both `method` and
/// `id`) is answered on `stdin` and drained; a response (`id` alone) is
/// returned. Also returns the edit from the first `workspace/applyEdit`
/// request seen, if any, so the caller can assert on its contents.
fn read_response_answering_apply_edit(
    child: &mut std::process::Child,
    reader: &mut BufReader<std::process::ChildStdout>,
) -> std::io::Result<(String, Option<serde_json::Value>)> {
    let mut captured = None;
    loop {
        let raw = read_frame(reader)?;
        if raw.is_empty() {
            return Ok((raw, captured));
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return Ok((raw, captured));
        };
        if v.get("method").and_then(|m| m.as_str()) == Some("workspace/applyEdit") {
            captured = Some(v["params"]["edit"].clone());
            let reply = serde_json::json!({
                "jsonrpc": "2.0",
                "id": v["id"],
                "result": { "applied": true },
            });
            write_frame(child, &reply.to_string())?;
            continue;
        }
        if v.get("id").is_some() {
            return Ok((raw, captured));
        }
    }
}

#[test]
fn test_create_loc_key_code_action_inserts_after_sibling_key() {
    // A type declaring two `## required` name-derived loc keys; the mod's own
    // loc file already has one of them (`my_thing`). CW100 fires for the
    // other (`my_thing_desc`), and its code action must insert the missing
    // stub right after the sibling's definition — the cross-file
    // `WorkspaceEdit` this feature exists for.
    const RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        localisation = {
            ## required
            name = "$"
            ## required
            desc = "$_desc"
        }
    }
}
thing = { x = scalar }
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    let rel_path = "common/things/test.txt";
    let text = "my_thing = { x = yes }\n";
    let file_path = ws.path().join(rel_path);
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, text).unwrap();

    let loc_path = ws.path().join("localisation/things_l_english.yml");
    std::fs::create_dir_all(loc_path.parent().unwrap()).unwrap();
    let loc_text = "l_english:\n my_thing:0 \"My Thing\"\n";
    std::fs::write(&loc_path, loc_text).unwrap();

    let ws_uri = path_uri(ws.path());
    let doc_uri = path_uri(&file_path);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": ws_uri,
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "vanilla": vanilla.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":doc_uri,"languageId":"hoi4","version":1,"text":text}}),
        ),
    )
    .unwrap();

    let diag = wait_for_diag_object(&mut reader, rel_path, "CW100")
        .expect("CW100 diagnostic published for my_thing_desc");
    assert!(
        diag["message"]
            .as_str()
            .is_some_and(|m| m.contains("my_thing_desc")),
        "got: {diag}"
    );

    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "textDocument/codeAction",
            serde_json::json!({
                "textDocument": { "uri": doc_uri },
                "range": diag["range"],
                "context": { "diagnostics": [diag], "only": ["quickfix"] },
            }),
        ),
    )
    .unwrap();
    let resp_str = read_response(&mut reader).expect("no codeAction response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let actions = resp["result"].as_array().expect("array result");
    assert_eq!(
        actions.len(),
        2,
        "the create-loc-key action plus the ignore action: {resp_str}"
    );
    let action = &actions[0];
    assert_eq!(action["title"], "Create localisation key my_thing_desc");
    assert_eq!(action["kind"], "quickfix");

    let ops = action["edit"]["documentChanges"]
        .as_array()
        .expect("documentChanges carries the cross-file edit");
    assert_eq!(ops.len(), 1, "no sibling file needed creating: {resp_str}");
    let op = &ops[0];
    assert!(
        op["textDocument"]["uri"]
            .as_str()
            .is_some_and(|u| u.ends_with("things_l_english.yml")),
        "got: {op}"
    );
    let edits = op["edits"].as_array().expect("edits array");
    assert_eq!(edits.len(), 1);
    let e = &edits[0];
    // Inserted right after the sibling's line (`l_english:` is line 0,
    // ` my_thing:0 "My Thing"` is line 1), at the start of the next line.
    assert_eq!(e["range"]["start"]["line"], 2);
    assert_eq!(e["range"]["start"]["character"], 0);
    assert_eq!(
        e["range"]["start"], e["range"]["end"],
        "an empty (insertion) range"
    );
    assert_eq!(e["newText"], " my_thing_desc:0 \"TODO\"\n");

    // Applying it reproduces the expected file.
    let mut lines: Vec<&str> = loc_text.lines().collect();
    lines.insert(2, " my_thing_desc:0 \"TODO\"");
    let fixed = format!("{}\n", lines.join("\n"));
    assert_eq!(
        fixed,
        "l_english:\n my_thing:0 \"My Thing\"\n my_thing_desc:0 \"TODO\"\n"
    );
}

#[test]
fn test_create_loc_key_code_action_creates_a_new_loc_file_when_the_workspace_has_none() {
    // The third resolution tier (`LocInsertTarget::NewFile`): the workspace
    // has no loc file at all, so the create-loc-key action must add a
    // `Create` resource op alongside the insertion edit, and the inserted
    // text must open with the BOM the game requires for a loc file to load —
    // nothing else in the suite pins that byte.
    //
    // CW100 is gated on the merged loc index being non-empty
    // (`append_missing_loc_errors`), so an empty workspace with no loc
    // anywhere would suppress it entirely. The base-game (vanilla) dir is
    // given a loc file instead: it merges into the union (index non-empty,
    // CW100 fires) but sits outside `editable_roots`, so the edit boundary
    // refuses it for the sibling-site and existing-file tiers alike and
    // resolution falls through to NewFile.
    const RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        localisation = {
            ## required
            name = "$"
            ## required
            desc = "$_desc"
        }
    }
}
thing = { x = scalar }
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    let rel_path = "common/things/test.txt";
    let text = "my_thing = { x = yes }\n";
    let file_path = ws.path().join(rel_path);
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, text).unwrap();

    // Only vanilla defines `my_thing`'s name loc — the workspace has no
    // `localisation/` directory at all.
    let vanilla_loc_path = vanilla.path().join("localisation/things_l_english.yml");
    std::fs::create_dir_all(vanilla_loc_path.parent().unwrap()).unwrap();
    std::fs::write(&vanilla_loc_path, "l_english:\n my_thing:0 \"My Thing\"\n").unwrap();

    let ws_uri = path_uri(ws.path());
    let doc_uri = path_uri(&file_path);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": ws_uri,
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "vanilla": vanilla.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":doc_uri,"languageId":"hoi4","version":1,"text":text}}),
        ),
    )
    .unwrap();

    let diag = wait_for_diag_object(&mut reader, rel_path, "CW100")
        .expect("CW100 diagnostic published for my_thing_desc");
    assert!(
        diag["message"]
            .as_str()
            .is_some_and(|m| m.contains("my_thing_desc")),
        "got: {diag}"
    );

    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "textDocument/codeAction",
            serde_json::json!({
                "textDocument": { "uri": doc_uri },
                "range": diag["range"],
                "context": { "diagnostics": [diag], "only": ["quickfix"] },
            }),
        ),
    )
    .unwrap();
    let resp_str = read_response(&mut reader).expect("no codeAction response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let actions = resp["result"].as_array().expect("array result");
    assert_eq!(
        actions.len(),
        2,
        "the create-loc-key action plus the ignore action: {resp_str}"
    );
    let action = &actions[0];
    assert_eq!(action["title"], "Create localisation key my_thing_desc");
    assert_eq!(action["kind"], "quickfix");

    let ops = action["edit"]["documentChanges"]
        .as_array()
        .expect("documentChanges carries the cross-file edit");
    assert_eq!(
        ops.len(),
        2,
        "a Create op plus the insertion edit: {resp_str}"
    );

    let create_op = &ops[0];
    assert_eq!(create_op["kind"], "create");
    assert!(
        create_op["uri"]
            .as_str()
            .is_some_and(|u| u.ends_with("localisation/cwtools_generated_l_english.yml")),
        "got: {create_op}"
    );
    assert_eq!(create_op["options"]["overwrite"], false);
    assert_eq!(create_op["options"]["ignoreIfExists"], true);

    let edit_op = &ops[1];
    assert_eq!(edit_op["textDocument"]["uri"], create_op["uri"]);
    let edits = edit_op["edits"].as_array().expect("edits array");
    assert_eq!(edits.len(), 1);
    let e = &edits[0];
    assert_eq!(e["range"]["start"]["line"], 0);
    assert_eq!(e["range"]["start"]["character"], 0);
    assert_eq!(
        e["range"]["start"], e["range"]["end"],
        "an empty (insertion) range"
    );
    assert_eq!(
        e["newText"], "\u{FEFF}l_english:\n my_thing_desc:0 \"TODO\"\n",
        "the new file's content must open with the BOM the game requires"
    );
}

#[test]
fn test_create_loc_key_code_action_batch_dedupes_a_shared_new_file() {
    // #142: `name` and `desc` are BOTH missing (unlike the two tests above,
    // where one sibling already has a definition), and no loc file for the
    // language exists anywhere the edit boundary would accept — so both
    // diagnostics resolve to the identical generated `NewFile` target. One
    // codeAction request returning both diagnostics at once (an "instance
    // missing both title and desc" batch, no diagnostics round trip between
    // them) must not offer two independent actions that would each insert
    // their own BOM + header: applying the response must produce the header
    // exactly once, with both stubs.
    const RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        localisation = {
            ## required
            name = "$"
            ## required
            desc = "$_desc"
        }
    }
}
thing = { x = scalar }
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    let rel_path = "common/things/test.txt";
    let text = "my_thing = { x = yes }\n";
    let file_path = ws.path().join(rel_path);
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, text).unwrap();

    // An unrelated vanilla key only: the merged loc index is non-empty (CW100's
    // gate) but neither `my_thing` nor `my_thing_desc` resolves anywhere.
    let vanilla_loc_path = vanilla.path().join("localisation/other_l_english.yml");
    std::fs::create_dir_all(vanilla_loc_path.parent().unwrap()).unwrap();
    std::fs::write(
        &vanilla_loc_path,
        "l_english:\n unrelated_key:0 \"Unrelated\"\n",
    )
    .unwrap();

    let ws_uri = path_uri(ws.path());
    let doc_uri = path_uri(&file_path);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": ws_uri,
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "vanilla": vanilla.path().to_string_lossy(),
                    "cacheDir": cache_dir.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":doc_uri,"languageId":"hoi4","version":1,"text":text}}),
        ),
    )
    .unwrap();

    let diagnostics = wait_for_diags(&mut reader, rel_path).expect("diagnostics for the document");
    let cw100: Vec<serde_json::Value> = diagnostics
        .into_iter()
        .filter(|d| d["code"] == "CW100")
        .collect();
    assert_eq!(
        cw100.len(),
        2,
        "both name and desc must be missing: {cw100:#?}"
    );

    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "textDocument/codeAction",
            serde_json::json!({
                "textDocument": { "uri": doc_uri },
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 1, "character": 0 },
                },
                "context": { "diagnostics": cw100, "only": ["quickfix"] },
            }),
        ),
    )
    .unwrap();
    let resp_str = read_response(&mut reader).expect("no codeAction response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let actions = resp["result"].as_array().expect("array result");
    assert_eq!(
        actions.len(),
        2,
        "the second key must be absorbed into the first's action, not offered \
         as a second independent NewFile action — only the ignore action joins: {resp_str}"
    );

    let ops = actions[0]["edit"]["documentChanges"]
        .as_array()
        .expect("documentChanges carries the cross-file edit");
    assert_eq!(
        ops.len(),
        2,
        "one Create plus a SINGLE TextDocumentEdit for the file, not one per key: {resp_str}"
    );
    assert_eq!(ops[0]["kind"], "create");

    // Both inserts share the (0,0) start position within the one
    // TextDocumentEdit: per the LSP spec, array order (not sequential
    // mutation) decides the order they appear in the resulting text, so
    // concatenating `newText` in array order reproduces what a client
    // applying this edit would see.
    let edits = ops[1]["edits"].as_array().expect("edits array");
    assert_eq!(
        edits.len(),
        2,
        "the header edit and the folded stub, as two edits in one TextDocumentEdit: {resp_str}"
    );
    let applied: String = edits
        .iter()
        .map(|e| e["newText"].as_str().unwrap())
        .collect();
    assert_eq!(
        applied.matches("l_english:").count(),
        1,
        "the header must appear exactly once: {applied:?}"
    );
    assert!(applied.contains(" my_thing:0 "), "got: {applied:?}");
    assert!(applied.contains(" my_thing_desc:0 "), "got: {applied:?}");
}

#[test]
fn test_create_loc_key_code_action_reresolves_once_the_new_file_exists() {
    // Step 4 of #142: the SAFE one-at-a-time flow. Applying the first action
    // creates the generated file; a FRESH codeAction request afterward (a
    // real diagnostics round trip, unlike the in-batch case above) must
    // resolve via `ExistingFileAppend`, not `NewFile` again — no second
    // header, even for the exact same diagnostic re-requested.
    const RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        localisation = {
            ## required
            name = "$"
            ## required
            desc = "$_desc"
        }
    }
}
thing = { x = scalar }
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    let rel_path = "common/things/test.txt";
    let text = "my_thing = { x = yes }\n";
    let file_path = ws.path().join(rel_path);
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, text).unwrap();

    let vanilla_loc_path = vanilla.path().join("localisation/things_l_english.yml");
    std::fs::create_dir_all(vanilla_loc_path.parent().unwrap()).unwrap();
    std::fs::write(&vanilla_loc_path, "l_english:\n my_thing:0 \"My Thing\"\n").unwrap();

    let ws_uri = path_uri(ws.path());
    let doc_uri = path_uri(&file_path);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": ws_uri,
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "vanilla": vanilla.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":doc_uri,"languageId":"hoi4","version":1,"text":text}}),
        ),
    )
    .unwrap();

    let diag = wait_for_diag_object(&mut reader, rel_path, "CW100")
        .expect("CW100 diagnostic published for my_thing_desc");

    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "textDocument/codeAction",
            serde_json::json!({
                "textDocument": { "uri": doc_uri },
                "range": diag["range"],
                "context": { "diagnostics": [diag.clone()], "only": ["quickfix"] },
            }),
        ),
    )
    .unwrap();
    let first_resp_str = read_response(&mut reader).expect("no codeAction response");
    let first_resp: serde_json::Value = serde_json::from_str(&first_resp_str).unwrap();
    let first_actions = first_resp["result"].as_array().expect("array result");
    assert_eq!(first_actions.len(), 2);
    let first_ops = first_actions[0]["edit"]["documentChanges"]
        .as_array()
        .expect("documentChanges");
    assert_eq!(
        first_ops.len(),
        2,
        "Create + header insert: {first_resp_str}"
    );
    let generated_uri = first_ops[0]["uri"]
        .as_str()
        .expect("create uri")
        .to_string();
    let generated_new_text = first_ops[1]["edits"][0]["newText"]
        .as_str()
        .expect("insert text")
        .to_string();

    // Simulate the client applying that first action: write the file exactly
    // as the edit describes.
    let generated_path = tower_lsp::lsp_types::Url::parse(&generated_uri)
        .unwrap()
        .to_file_path()
        .unwrap();
    std::fs::create_dir_all(generated_path.parent().unwrap()).unwrap();
    std::fs::write(&generated_path, &generated_new_text).unwrap();

    // A real client's file watcher (which covers loc `*.yml`) fires a CREATED
    // event when the action's file lands on disk; that invalidates the
    // loc-discovery cache so the next request re-walks and sees the file.
    write_frame(
        &mut child,
        &watched_created(std::slice::from_ref(&generated_uri)),
    )
    .unwrap();

    write_frame(
        &mut child,
        &jsonrpc_request(
            3,
            "textDocument/codeAction",
            serde_json::json!({
                "textDocument": { "uri": doc_uri },
                "range": diag["range"],
                "context": { "diagnostics": [diag], "only": ["quickfix"] },
            }),
        ),
    )
    .unwrap();
    let second_resp_str = read_response(&mut reader).expect("no second codeAction response");
    child.kill().ok();

    let second_resp: serde_json::Value = serde_json::from_str(&second_resp_str).unwrap();
    let second_actions = second_resp["result"].as_array().expect("array result");
    assert_eq!(second_actions.len(), 2);
    let second_ops = second_actions[0]["edit"]["documentChanges"]
        .as_array()
        .expect("documentChanges");
    assert_eq!(
        second_ops.len(),
        1,
        "the file exists now, so no Create op and no second header: {second_resp_str}"
    );
    let second_edit = &second_ops[0]["edits"][0];
    assert!(
        !second_edit["newText"]
            .as_str()
            .unwrap()
            .contains("l_english:"),
        "re-resolving after the file exists must not re-emit the header: {second_resp_str}"
    );
}

#[test]
fn test_fix_all_workspace_applies_every_fixable_diagnostic() {
    // Two empty `limit = { }` blocks -> two CW281 diagnostics, each carrying a
    // fix. `fixAllWorkspace` must send one `workspace/applyEdit` covering both,
    // then report how many fixes/files it applied — the workspace-wide
    // counterpart of `source.fixAll` (and of `cwtools fix --apply`).
    const RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    let rel = "common/decisions/test.txt";
    let text = "a = { limit = { } }\nb = { limit = { } }\n";
    let p = ws.path().join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, text).unwrap();
    let doc_uri = path_uri(&p);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(ws.path()),
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "vanilla": vanilla.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":doc_uri,"languageId":"hoi4","version":1,"text":text}}),
        ),
    )
    .unwrap();
    wait_for_diags(&mut reader, rel).expect("diagnostics published for the document");

    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "workspace/executeCommand",
            serde_json::json!({ "command": "fixAllWorkspace", "arguments": [] }),
        ),
    )
    .unwrap();
    let (resp_str, applied_edit) =
        read_response_answering_apply_edit(&mut child, &mut reader).expect("no command response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(
        resp["result"].as_str(),
        Some("Applied 2 fix(es) across 1 file(s)"),
        "got: {resp_str}"
    );

    let edit = applied_edit.expect("the server must call workspace/applyEdit");
    let changes = edit["changes"].as_object().expect("changes map");
    assert_eq!(changes.len(), 1, "one file touched: {edit}");
    let edits = changes.values().next().unwrap().as_array().unwrap();
    assert_eq!(edits.len(), 2, "one edit per fixable diagnostic");

    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut resolved: Vec<(usize, usize, usize, String)> = edits
        .iter()
        .map(|e| {
            (
                e["range"]["start"]["line"].as_u64().unwrap() as usize,
                e["range"]["start"]["character"].as_u64().unwrap() as usize,
                e["range"]["end"]["character"].as_u64().unwrap() as usize,
                e["newText"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    resolved.sort_by(|a, b| b.cmp(a));
    for (line, sc, ec, new_text) in resolved {
        let chars: Vec<char> = lines[line].chars().collect();
        let mut out: String = chars[..sc].iter().collect();
        out.push_str(&new_text);
        out.extend(chars[ec..].iter());
        lines[line] = out;
    }
    let fixed = format!("{}\n", lines.join("\n"));
    assert_eq!(fixed, "a = { }\nb = { }\n");
}

#[test]
fn test_fix_all_workspace_skips_stale_closed_file_edits() {
    const RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    let rel = "common/decisions/test.txt";
    let path = ws.path().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "a = { limit = { } }\n").unwrap();

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(ws.path()),
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "vanilla": vanilla.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    // Change a closed file after its diagnostic was published. The old fix range
    // must not be applied to the new contents.
    std::fs::write(&path, "a = { limit = { x = 1 } }\n").unwrap();
    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "workspace/executeCommand",
            serde_json::json!({ "command": "fixAllWorkspace", "arguments": [] }),
        ),
    )
    .unwrap();
    let (resp_str, applied_edit) =
        read_response_answering_apply_edit(&mut child, &mut reader).expect("no command response");
    child.kill().ok();

    assert!(applied_edit.is_none(), "stale fixes must not be applied");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(
        resp["result"].as_str(),
        Some("Applied 0 fix(es) across 0 file(s); 1 skipped (stale)"),
        "got: {resp_str}"
    );
}

/// A workspace whose one decision file carries a fixable CW281, initialized and
/// scanned. Returns the temp dirs (the caller keeps them alive), the child, the
/// reader, and the file's path. The scan's diagnostics for the file are asserted
/// on the way past: they are what puts it in the `fixAllWorkspace` store, so a
/// test that never saw them would pass on an empty store for the wrong reason.
/// They are read before `wait_for_scan_done` because the scan publishes them
/// before closing its loading bar, and that drain would swallow them.
#[allow(clippy::type_complexity)]
fn spawn_fixable_workspace() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    tempfile::TempDir,
    std::process::Child,
    BufReader<std::process::ChildStdout>,
    std::path::PathBuf,
) {
    const RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    let path = ws.path().join("common/decisions/test.txt");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "a = { limit = { } }\n").unwrap();

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(ws.path()),
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "vanilla": vanilla.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    let scanned = wait_for_diags(&mut reader, "test.txt").expect("scan diagnostics");
    assert!(
        scanned.iter().any(|d| d["code"] == "CW281"),
        "the fixture must publish a fixable CW281, got: {scanned:?}"
    );
    wait_for_scan_done(&mut reader);
    (ws, rules_dir, vanilla, child, reader, path)
}

/// The `fixAllWorkspace` result for a server whose only fixable file is gone
/// from the Problems panel: the store must be empty, not merely stale.
const NOTHING_FIXABLE: &str = "No auto-fixable problems in the workspace.";

#[test]
fn test_deleting_a_watched_file_drops_its_fixable_edits() {
    // The DELETE batch publishes empty diagnostics for the file. That publish
    // owns the `fixAllWorkspace` store too, or the deleted file's fixes outlive
    // the diagnostics they came from (#133).
    let (ws, _rules, _vanilla, mut child, mut reader, path) = spawn_fixable_workspace();
    let uri = path_uri(&path);

    std::fs::remove_file(&path).unwrap();
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "workspace/didChangeWatchedFiles",
            serde_json::json!({ "changes": [{ "uri": uri, "type": 3 }] }),
        ),
    )
    .unwrap();
    let cleared = wait_for_diags(&mut reader, "test.txt").expect("empty publish for the delete");
    assert!(cleared.is_empty(), "the delete must clear the panel");

    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "workspace/executeCommand",
            serde_json::json!({ "command": "fixAllWorkspace", "arguments": [] }),
        ),
    )
    .unwrap();
    let (resp_str, applied_edit) =
        read_response_answering_apply_edit(&mut child, &mut reader).expect("no command response");
    child.kill().ok();
    drop(ws);

    assert!(applied_edit.is_none(), "a deleted file has nothing to fix");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(
        resp["result"].as_str(),
        Some(NOTHING_FIXABLE),
        "got: {resp_str}"
    );
}

#[test]
fn test_closing_a_document_drops_its_fixable_edits() {
    // did_close's empty publish must take the store entry with it. The entry is
    // keyed by the open buffer's version, so leaving it behind survives until
    // some later reopen matches that version against different content (#133).
    let (ws, _rules, _vanilla, mut child, mut reader, path) = spawn_fixable_workspace();
    let uri = path_uri(&path);

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":uri,"languageId":"hoi4","version":7,
                "text": "a = { limit = { } }\n"}}),
        ),
    )
    .unwrap();
    let open_diags = wait_for_diags(&mut reader, "test.txt").expect("didOpen diagnostics");
    assert!(
        open_diags.iter().any(|d| d["code"] == "CW281"),
        "expected a fixable CW281 while open, got: {open_diags:?}"
    );

    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didClose",
            serde_json::json!({"textDocument":{"uri":uri}}),
        ),
    )
    .unwrap();
    let cleared = wait_for_diags(&mut reader, "test.txt").expect("empty publish for the close");
    assert!(cleared.is_empty(), "the close must clear the panel");

    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "workspace/executeCommand",
            serde_json::json!({ "command": "fixAllWorkspace", "arguments": [] }),
        ),
    )
    .unwrap();
    let (resp_str, applied_edit) =
        read_response_answering_apply_edit(&mut child, &mut reader).expect("no command response");
    child.kill().ok();
    drop(ws);

    assert!(
        applied_edit.is_none(),
        "a closed file's diagnostics were cleared, so nothing may be applied"
    );
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(
        resp["result"].as_str(),
        Some(NOTHING_FIXABLE),
        "got: {resp_str}"
    );
}

#[test]
fn test_fix_all_workspace_reports_nothing_to_fix() {
    // No diagnostics ever published -> the store is empty -> the command must
    // say so without sending a `workspace/applyEdit` at all.
    let ws = tempfile::tempdir().unwrap();
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(ws.path()),
                "capabilities": {},
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "workspace/executeCommand",
            serde_json::json!({ "command": "fixAllWorkspace", "arguments": [] }),
        ),
    )
    .unwrap();
    let (resp_str, applied_edit) =
        read_response_answering_apply_edit(&mut child, &mut reader).expect("no command response");
    child.kill().ok();
    assert!(
        applied_edit.is_none(),
        "nothing to fix -> no applyEdit call"
    );
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(
        resp["result"].as_str(),
        Some("No auto-fixable problems in the workspace."),
        "got: {resp_str}"
    );
}

#[test]
fn test_fix_all_workspace_finds_nothing_once_the_applied_fix_is_reopened_clean() {
    // Full lifecycle: fixAllWorkspace applies a fix, the client applies it (a
    // didChange with the fixed text on the still-open document), the server
    // republishes without the diagnostic — which also drops the entry from
    // the `fixable_edits` store, since `publish_filtered` updates the store
    // before it publishes (validate.rs) — and a second fixAllWorkspace call
    // must then report nothing left to fix rather than resending the
    // already-applied edit.
    const RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    let rel = "common/decisions/test.txt";
    let text = "a = { limit = { } }\n";
    let p = ws.path().join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, text).unwrap();
    let doc_uri = path_uri(&p);

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(ws.path()),
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "vanilla": vanilla.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":doc_uri,"languageId":"hoi4","version":1,"text":text}}),
        ),
    )
    .unwrap();
    let before = wait_for_diags(&mut reader, rel).expect("diagnostics published for the document");
    assert!(
        before.iter().any(|d| d["code"] == "CW281"),
        "expected CW281 before the fix, got: {before:?}"
    );

    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "workspace/executeCommand",
            serde_json::json!({ "command": "fixAllWorkspace", "arguments": [] }),
        ),
    )
    .unwrap();
    let (resp_str, applied_edit) =
        read_response_answering_apply_edit(&mut child, &mut reader).expect("no command response");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&resp_str).unwrap()["result"].as_str(),
        Some("Applied 1 fix(es) across 1 file(s)"),
        "got: {resp_str}"
    );
    assert!(
        applied_edit.is_some(),
        "the server must call workspace/applyEdit"
    );

    // Simulate the client having applied that edit itself.
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": doc_uri, "version": 2 },
                "contentChanges": [{ "text": "a = { }\n" }],
            }),
        ),
    )
    .unwrap();
    let after = diags_for(&mut reader, rel, 1).expect("republish after the didChange");
    assert!(
        !after.contains(&"CW281".to_string()),
        "the fixed text must not still report CW281, got: {:?}",
        after
    );

    write_frame(
        &mut child,
        &jsonrpc_request(
            3,
            "workspace/executeCommand",
            serde_json::json!({ "command": "fixAllWorkspace", "arguments": [] }),
        ),
    )
    .unwrap();
    let (resp_str2, applied_edit2) = read_response_answering_apply_edit(&mut child, &mut reader)
        .expect("no second command response");
    child.kill().ok();
    assert!(
        applied_edit2.is_none(),
        "nothing left to fix -> no second applyEdit call"
    );
    let resp2: serde_json::Value = serde_json::from_str(&resp_str2).unwrap();
    assert_eq!(
        resp2["result"].as_str(),
        Some("No auto-fixable problems in the workspace."),
        "got: {resp_str2}"
    );
}

/// A hostile workspace ships `localisation/things_l_english.yml` as a symlink to
/// a file outside it. The loc walk rejects the symlink (#161), so the sibling
/// key it defines is never indexed and CW100 fires for both `name` and `desc`
/// — and the create-key action must not offer to write through the link
/// (#160). It falls through to the new-file tier instead, which lands inside
/// the workspace.
#[cfg(unix)]
#[test]
fn test_create_loc_key_action_refuses_a_symlinked_loc_file() {
    const RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        localisation = {
            ## required
            name = "$"
            ## required
            desc = "$_desc"
        }
    }
}
thing = { x = scalar }
"#;
    let ws = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    let rel_path = "common/things/test.txt";
    let text = "my_thing = { x = yes }\n";
    let file_path = ws.path().join(rel_path);
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, text).unwrap();

    // The real loc file lives outside the workspace; only a link to it is in.
    let target = outside.path().join("things_l_english.yml");
    std::fs::write(&target, "l_english:\n my_thing:0 \"My Thing\"\n").unwrap();
    let link = ws.path().join("localisation/things_l_english.yml");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    // A real vanilla loc file keeps the loc index non-empty so CW100's gate
    // fires; the symlinked file is rejected and contributes nothing.
    let vanilla_loc = vanilla.path().join("localisation/other_l_english.yml");
    std::fs::create_dir_all(vanilla_loc.parent().unwrap()).unwrap();
    std::fs::write(&vanilla_loc, "l_english:\n unrelated_key:0 \"Unrelated\"\n").unwrap();

    let doc_uri = path_uri(&file_path);
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(ws.path()),
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "vanilla": vanilla.path().to_string_lossy(),
                    "cacheDir": cache_dir.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument":{"uri":doc_uri,"languageId":"hoi4","version":1,"text":text}}),
        ),
    )
    .unwrap();

    // The symlinked loc file is rejected by the scan (#161), so neither
    // `my_thing` nor `my_thing_desc` resolves — both are missing. Without that
    // this test would pass vacuously on a workspace where nothing was indexed.
    let diags = wait_for_diags(&mut reader, rel_path).expect("diagnostics for the document");
    let cw100: Vec<serde_json::Value> =
        diags.into_iter().filter(|d| d["code"] == "CW100").collect();
    assert_eq!(
        cw100.len(),
        2,
        "both name and desc must be missing (the symlinked loc file is not indexed): {cw100:#?}"
    );
    let diag = cw100
        .iter()
        .find(|d| {
            d["message"]
                .as_str()
                .is_some_and(|m| m.contains("my_thing_desc"))
        })
        .cloned()
        .expect("CW100 for my_thing_desc");

    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "textDocument/codeAction",
            serde_json::json!({
                "textDocument": { "uri": doc_uri },
                "range": diag["range"],
                "context": { "diagnostics": [diag], "only": ["quickfix"] },
            }),
        ),
    )
    .unwrap();
    let resp_str = read_response(&mut reader).expect("no codeAction response");
    child.kill().ok();

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let actions = resp["result"].as_array().expect("array result");
    assert_eq!(
        actions.len(),
        2,
        "the create-loc-key action plus the ignore action: {resp_str}"
    );
    let ops = actions[0]["edit"]["documentChanges"]
        .as_array()
        .expect("documentChanges carries the cross-file edit");
    let targets: Vec<&str> = ops
        .iter()
        .filter_map(|op| {
            op["uri"]
                .as_str()
                .or_else(|| op["textDocument"]["uri"].as_str())
        })
        .collect();
    assert!(
        !targets.iter().any(|u| u.ends_with("things_l_english.yml")),
        "must not edit through the symlink, got: {targets:?}"
    );
    let outside_uri = path_uri(outside.path());
    assert!(
        !targets.iter().any(|u| u.starts_with(&outside_uri)),
        "must not edit outside the workspace, got: {targets:?}"
    );
    assert!(
        targets
            .iter()
            .all(|u| u.ends_with("localisation/cwtools_generated_l_english.yml")),
        "falls through to a new in-workspace file, got: {targets:?}"
    );
}

/// Which of `rel_paths` published a diagnostic with `code` during the initial
/// scan, in the order seen. The scan publishes before it clears the loading
/// bar, so this replaces [`wait_for_scan_done`] rather than following it —
/// which also means a file the scan never validated shows up as a missing
/// entry when the bar goes off, instead of blocking on a frame that never
/// arrives.
#[cfg(unix)]
fn scan_diag_paths(
    reader: &mut BufReader<std::process::ChildStdout>,
    rel_paths: &[&str],
    code: &str,
) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for _ in 0..5000 {
        let Ok(raw) = read_frame(reader) else {
            return seen;
        };
        if raw.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        if v["method"] == "textDocument/publishDiagnostics" {
            let uri = v["params"]["uri"].as_str().unwrap_or("");
            let carries_code = v["params"]["diagnostics"]
                .as_array()
                .is_some_and(|ds| ds.iter().any(|d| d["code"] == code));
            if carries_code
                && let Some(rel) = rel_paths.iter().find(|rel| uri.ends_with(**rel))
                && !seen.iter().any(|s| s == rel)
            {
                seen.push((*rel).to_string());
            }
        }
        if v["method"] == "loadingBar" && v["params"]["enable"] == serde_json::Value::Bool(false) {
            return seen;
        }
    }
    seen
}

/// `fixAllWorkspace` collects every URI that published a fixable diagnostic.
/// The workspace scan rejects symlinks (#161), so a linked file is never
/// validated and never enters the fixAllWorkspace store; the generated
/// `applyEdit` must only touch the real in-workspace file.
///
/// The link points into the base-game install on purpose. That is the case the
/// read boundary (#163) cannot catch: the install is a legitimate place to read
/// from, so the file's text resolves and the edit would be planned. Only a
/// separate, workspace-only *edit* boundary refuses it.
#[cfg(unix)]
#[test]
fn test_fix_all_workspace_skips_a_symlinked_file() {
    const RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
"#;
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), RULES).unwrap();

    let real_rel = "common/decisions/real.txt";
    let real = ws.path().join(real_rel);
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::write(&real, "a = { limit = { } }\n").unwrap();

    // A file in the base-game install, reachable only through a link inside the
    // workspace — readable, and so plannable, but never writable.
    let linked_rel = "common/decisions/linked.txt";
    let target = vanilla.path().join("linked.txt");
    std::fs::write(&target, "b = { limit = { } }\n").unwrap();
    std::os::unix::fs::symlink(&target, ws.path().join(linked_rel)).unwrap();

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(ws.path()),
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "vanilla": vanilla.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();

    // The scan rejects the symlink (#161), so the linked file is never
    // validated and never enters the fixAllWorkspace store. The real file is
    // still fixable.
    let validated = scan_diag_paths(&mut reader, &[real_rel, linked_rel], "CW281");
    assert!(
        validated.iter().any(|p| p == real_rel),
        "the real file must be fixable too, saw: {validated:?}"
    );
    assert!(
        !validated.iter().any(|p| p == linked_rel),
        "the scan must reject the symlink and not publish a diagnostic for it, saw: {validated:?}"
    );

    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "workspace/executeCommand",
            serde_json::json!({ "command": "fixAllWorkspace", "arguments": [] }),
        ),
    )
    .unwrap();
    let (resp_str, applied_edit) =
        read_response_answering_apply_edit(&mut child, &mut reader).expect("no command response");
    child.kill().ok();

    let edit = applied_edit.expect("the server must call workspace/applyEdit");
    let changes = edit["changes"].as_object().expect("changes map");
    let touched: Vec<&String> = changes.keys().collect();
    assert!(
        touched.iter().all(|u| u.ends_with("real.txt")),
        "only the real in-workspace file may be edited, got: {touched:?}"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&resp_str).unwrap()["result"].as_str(),
        Some("Applied 1 fix(es) across 1 file(s)"),
        "got: {resp_str}"
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "b = { limit = { } }\n",
        "the base-game file is untouched"
    );
}

#[test]
fn test_scan_reports_standard_work_done_progress() {
    // A client that advertises window.workDoneProgress must see the standard
    // $/progress stream, not only the custom `loadingBar` notification.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("editor_rules.cwt"), EDITOR_RULES).unwrap();
    let p = ws.path().join("common/national_focus/tree.txt");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "my_focus = {\n    id = my_focus\n}\n").unwrap();

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(ws.path()),
                "capabilities": { "window": { "workDoneProgress": true } },
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();

    let stdin = child.stdin.take().unwrap();
    let collected = run_with_deadline(stdin, reader, 90, |stdin, reader| {
        let mut created = false;
        let mut kinds: Vec<String> = Vec::new();
        let mut saw_loading_bar = false;
        for _ in 0..2000 {
            let Ok(raw) = read_frame(reader) else { break };
            if raw.is_empty() {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            match v["method"].as_str() {
                Some("window/workDoneProgress/create") => {
                    println!("window/workDoneProgress/create = {}", v["params"]);
                    assert_eq!(v["params"]["token"], "cwtools/scan");
                    created = true;
                    // The server waits for this response before it begins.
                    write_frame_to(
                        stdin,
                        &serde_json::json!({ "jsonrpc": "2.0", "id": v["id"], "result": null })
                            .to_string(),
                    )
                    .unwrap();
                }
                Some("$/progress") => {
                    println!("$/progress = {}", v["params"]);
                    assert_eq!(v["params"]["token"], "cwtools/scan");
                    let kind = v["params"]["value"]["kind"].as_str().unwrap().to_string();
                    let done = kind == "end";
                    kinds.push(kind);
                    if done {
                        break;
                    }
                }
                Some("loadingBar") => saw_loading_bar = true,
                _ => {}
            }
        }
        (created, kinds, saw_loading_bar)
    });
    child.kill().ok();

    let (created, kinds, saw_loading_bar) =
        collected.expect("timed out waiting for the progress stream");
    assert!(created, "no window/workDoneProgress/create");
    assert_eq!(
        kinds.first().map(String::as_str),
        Some("begin"),
        "progress must open with begin: {kinds:?}"
    );
    assert_eq!(
        kinds.last().map(String::as_str),
        Some("end"),
        "progress must be closed: {kinds:?}"
    );
    assert!(
        saw_loading_bar,
        "the custom loadingBar notification must still fire for the VS Code client"
    );
}

/// #145: a command that carries a `workDoneToken` gets its progress reported
/// against *that* token — one bar for the operation, driven by the client's own
/// notification — rather than the server opening its `cwtools/scan` stream
/// alongside it.
#[test]
fn test_execute_command_reports_progress_against_the_client_token() {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("editor_rules.cwt"), EDITOR_RULES).unwrap();
    let p = ws.path().join("common/national_focus/tree.txt");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "my_focus = {\n    id = my_focus\n}\n").unwrap();

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(ws.path()),
                "capabilities": { "window": { "workDoneProgress": true } },
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let init = read_response(&mut reader).expect("no init response");
    let init: serde_json::Value = serde_json::from_str(&init).unwrap();
    assert_eq!(
        init["result"]["capabilities"]["executeCommandProvider"]["workDoneProgress"],
        serde_json::json!(true),
        "the client feature-detects command progress on this flag"
    );
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    // Everything from here runs inside the deadline closure, because it owns
    // stdin and the wait for the startup scan has to *answer*
    // `window/workDoneProgress/create`. This client advertises
    // `window.workDoneProgress`, and the server blocks on that request before
    // it opens its own stream — draining frames without replying wedges the
    // scan and the test with it.
    let stdin = child.stdin.take().unwrap();
    let collected = run_with_deadline(stdin, reader, 120, |stdin, reader| {
        let mut kinds: Vec<String> = Vec::new();
        let mut percentages: Vec<u64> = Vec::new();
        let mut foreign_during_command: Vec<String> = Vec::new();
        let mut created_for_command = false;
        let mut result: Option<serde_json::Value> = None;
        // `ScanGuard::finish` sends the bar-off *before* it releases the scan
        // flag, deliberately, so the next scan's `begin` can't be overtaken by
        // this one's `end`. A command fired the instant that notification
        // arrives therefore races the release and can lose the CAS — rarely
        // when the suite runs alone, reliably under a loaded `cargo test`. So
        // retry until one actually re-indexes, each attempt on its own id and
        // token so the frames stay attributable.
        let mut attempt = 0i64;
        let mut token = String::new();
        let send_attempt = |stdin: &mut std::process::ChildStdin, attempt: i64| -> String {
            let token = format!("cwtools/command/1/{attempt}");
            write_frame_to(
                stdin,
                &jsonrpc_request(
                    100 + attempt,
                    "workspace/executeCommand",
                    serde_json::json!({
                        "command": "reindexWorkspace",
                        "arguments": [],
                        "workDoneToken": token,
                    }),
                ),
            )
            .unwrap();
            token
        };
        for _ in 0..20_000 {
            let Ok(raw) = read_frame(reader) else { break };
            if raw.is_empty() {
                break; // EOF
            }
            let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            if v["id"] == serde_json::json!(100 + attempt) && v.get("result").is_some() {
                if v["result"] == serde_json::json!("Re-index already in progress.") {
                    // Lost the CAS to the tail of the startup scan. Drop what
                    // that attempt reported and try again.
                    attempt += 1;
                    kinds.clear();
                    percentages.clear();
                    foreign_during_command.clear();
                    created_for_command = false;
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    token = send_attempt(stdin, attempt);
                    continue;
                }
                // Recorded rather than returned: `end` is sent before the
                // handler returns, but tower-lsp merges notifications and
                // responses onto one output with no ordering between them, so
                // the response can arrive first. Keep reading for the `end`.
                result = Some(v["result"].clone());
                if kinds.last().map(String::as_str) == Some("end") {
                    break;
                }
                continue;
            }
            match v["method"].as_str() {
                Some("window/workDoneProgress/create") => {
                    if !token.is_empty() {
                        created_for_command = true;
                    }
                    write_frame_to(
                        stdin,
                        &serde_json::json!({ "jsonrpc": "2.0", "id": v["id"], "result": null })
                            .to_string(),
                    )
                    .unwrap();
                }
                Some("$/progress") => {
                    let seen = v["params"]["token"].as_str().unwrap_or_default();
                    if seen != token {
                        // Only from the command's own `begin` onward. The
                        // startup scan closes its `cwtools/scan` stream just
                        // *after* the `loadingBar(false)` that tells us it
                        // finished, and that trailing `end` is its own, not a
                        // second stream opened alongside the command.
                        if !kinds.is_empty() {
                            foreign_during_command.push(seen.to_string());
                        }
                        continue;
                    }
                    kinds.push(v["params"]["value"]["kind"].as_str().unwrap().to_string());
                    if let Some(pct) = v["params"]["value"]["percentage"].as_u64() {
                        percentages.push(pct);
                    }
                    if result.is_some() && kinds.last().map(String::as_str) == Some("end") {
                        break;
                    }
                }
                // The startup scan is done; run the command it was blocking.
                Some("loadingBar")
                    if token.is_empty()
                        && v["params"]["enable"] == serde_json::Value::Bool(false) =>
                {
                    token = send_attempt(stdin, attempt);
                }
                _ => {}
            }
        }
        (
            kinds,
            percentages,
            foreign_during_command,
            created_for_command,
            result,
        )
    });
    child.kill().ok();

    let (kinds, percentages, foreign_during_command, created_for_command, result) =
        collected.expect("timed out waiting for the command to finish");
    assert_eq!(
        result.and_then(|r| r.as_str().map(str::to_string)),
        Some("Workspace re-indexed.".to_string())
    );
    assert_eq!(
        kinds.first().map(String::as_str),
        Some("begin"),
        "progress must open with begin: {kinds:?}"
    );
    assert_eq!(
        kinds.last().map(String::as_str),
        Some("end"),
        "progress must be closed: {kinds:?}"
    );
    // The client advertised `window.workDoneProgress`, so the server *would*
    // have opened `cwtools/scan` for this scan had the command not owned the
    // indicator — which is what makes both of these meaningful rather than
    // vacuous.
    assert!(
        foreign_during_command.is_empty(),
        "a command with its own token must not also open the server's stream, saw {foreign_during_command:?}"
    );
    assert!(
        !created_for_command,
        "a client-supplied token is already registered; window/workDoneProgress/create is for server-initiated ones"
    );
    // Determinate, and never rewinding: the client turns these into increments.
    assert!(
        percentages.len() >= 2,
        "expected a moving percentage, got {percentages:?}"
    );
    assert!(
        percentages.windows(2).all(|w| w[0] <= w[1]),
        "percentage went backwards: {percentages:?}"
    );
    assert!(
        percentages.iter().all(|&p| p <= 100),
        "percentage out of range: {percentages:?}"
    );
}

/// #228: two commands in flight at once each keep their own `$/progress`
/// stream. `cacheVanilla` doesn't take the scan flag, so it can begin while a
/// `reindexWorkspace` scan is still running; the scan's later phases have to
/// stay on the re-index token instead of following whichever command began
/// last and then falling back to the server's stream when that one ends.
#[test]
fn test_concurrent_commands_keep_separate_progress_streams() {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    // Explicit and empty, so `cacheVanilla` re-indexes nothing instead of
    // whatever real game install auto-discovery finds on the host.
    let vanilla_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("editor_rules.cwt"), EDITOR_RULES).unwrap();
    let p = ws.path().join("common/national_focus/tree.txt");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "my_focus = {\n    id = my_focus\n}\n").unwrap();

    let mut child = cwtools_server_cmd()
        // Holds every scan at its first phase, so the re-index is reliably
        // still running when the second command opens and closes its stream —
        // rather than racing a workspace big enough to take a measurable time.
        .env("CWTOOLS_SCAN_HOLD_MS", "5000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(ws.path()),
                "capabilities": { "window": { "workDoneProgress": true } },
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                    "vanilla": vanilla_dir.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    // As in the token tests above, everything runs inside the deadline closure
    // because answering `window/workDoneProgress/create` is what lets the
    // startup scan finish.
    let stdin = child.stdin.take().unwrap();
    let collected = run_with_deadline(stdin, reader, 180, |stdin, reader| {
        // Every `$/progress` frame as `(token, kind)`, in arrival order — the
        // interleaving is the thing under test.
        let mut events: Vec<(String, String)> = Vec::new();
        let mut reindex_result: Option<serde_json::Value> = None;
        let mut reindex_closed = false;
        // Same scan-guard race as the tests above: the bar-off notification
        // precedes the flag release, so a command sent on it can lose the CAS.
        // Retry until one actually re-indexes, each attempt on its own ids and
        // tokens so the frames stay attributable.
        let mut attempt = 0i64;
        let mut reindex_token = String::new();
        let mut vanilla_token = String::new();
        let send_reindex = |stdin: &mut std::process::ChildStdin, attempt: i64| -> String {
            let token = format!("cwtools/command/228/reindex/{attempt}");
            write_frame_to(
                stdin,
                &jsonrpc_request(
                    100 + attempt,
                    "workspace/executeCommand",
                    serde_json::json!({
                        "command": "reindexWorkspace",
                        "arguments": [],
                        "workDoneToken": token,
                    }),
                ),
            )
            .unwrap();
            token
        };
        for _ in 0..20_000 {
            let Ok(raw) = read_frame(reader) else { break };
            if raw.is_empty() {
                break; // EOF
            }
            let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            if v["id"] == serde_json::json!(100 + attempt) && v.get("result").is_some() {
                if v["result"] == serde_json::json!("Re-index already in progress.") {
                    attempt += 1;
                    events.clear();
                    vanilla_token.clear();
                    reindex_closed = false;
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    reindex_token = send_reindex(stdin, attempt);
                    continue;
                }
                // Recorded rather than returned: the response and the `end` are
                // merged onto one output with no ordering between them, so
                // either can land first.
                reindex_result = Some(v["result"].clone());
                if reindex_closed {
                    break;
                }
                continue;
            }
            match v["method"].as_str() {
                Some("window/workDoneProgress/create") => write_frame_to(
                    stdin,
                    &serde_json::json!({ "jsonrpc": "2.0", "id": v["id"], "result": null })
                        .to_string(),
                )
                .unwrap(),
                Some("$/progress") => {
                    let token = v["params"]["token"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    let kind = v["params"]["value"]["kind"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    // The re-index owns the workspace and is held at its first
                    // phase; start the overlapping command now.
                    if token == reindex_token && kind == "begin" && vanilla_token.is_empty() {
                        vanilla_token = format!("cwtools/command/228/vanilla/{attempt}");
                        write_frame_to(
                            stdin,
                            &jsonrpc_request(
                                200 + attempt,
                                "workspace/executeCommand",
                                serde_json::json!({
                                    "command": "cacheVanilla",
                                    "arguments": [],
                                    "workDoneToken": vanilla_token,
                                }),
                            ),
                        )
                        .unwrap();
                    }
                    reindex_closed |= token == reindex_token && kind == "end";
                    events.push((token, kind));
                    if reindex_closed && reindex_result.is_some() {
                        break;
                    }
                }
                // The startup scan is done; run the command it was blocking.
                Some("loadingBar")
                    if reindex_token.is_empty()
                        && v["params"]["enable"] == serde_json::Value::Bool(false) =>
                {
                    reindex_token = send_reindex(stdin, attempt);
                }
                _ => {}
            }
        }
        (events, reindex_token, vanilla_token, reindex_result)
    });
    child.kill().ok();

    let (events, reindex_token, vanilla_token, reindex_result) =
        collected.expect("timed out waiting for the commands to finish");
    assert_eq!(
        reindex_result.and_then(|r| r.as_str().map(str::to_string)),
        Some("Workspace re-indexed.".to_string())
    );
    assert!(
        !vanilla_token.is_empty(),
        "the overlapping command was never sent: {events:?}"
    );
    let vanilla_end = events
        .iter()
        .position(|(token, kind)| token == &vanilla_token && kind == "end")
        .unwrap_or_else(|| panic!("the second command never closed its stream: {events:?}"));
    // The bug: the second command's `begin` took the indicator, so the first
    // command's remaining phases reported against the second token and then,
    // once that one ended, against the server's `cwtools/scan` stream instead.
    assert!(
        events[vanilla_end + 1..]
            .iter()
            .any(|(token, kind)| token == &reindex_token && kind == "report"),
        "the re-index's later phases must stay on its own token: {events:?}"
    );
    let mine: Vec<&str> = events
        .iter()
        .filter(|(token, _)| token == &reindex_token)
        .map(|(_, kind)| kind.as_str())
        .collect();
    assert_eq!(mine.first(), Some(&"begin"), "{events:?}");
    assert_eq!(mine.last(), Some(&"end"), "{events:?}");
}

/// #145: the Cancel button. `window/workDoneProgress/cancel` is a notification,
/// so the server handles it while the scan is still running — unlike
/// `$/cancelRequest`, which tower-lsp answers by dropping the handler and which
/// therefore cannot produce a reply at all. The command has to come back
/// promptly *and* say it was cancelled.
#[test]
fn test_work_done_progress_cancel_stops_a_command() {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("editor_rules.cwt"), EDITOR_RULES).unwrap();
    let p = ws.path().join("common/national_focus/tree.txt");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "my_focus = {\n    id = my_focus\n}\n").unwrap();

    let mut child = cwtools_server_cmd()
        // Holds every scan open at its start, which is how the cancel lands
        // mid-scan deterministically instead of racing a workspace big enough
        // to take a measurable time.
        .env("CWTOOLS_SCAN_HOLD_MS", "3000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(ws.path()),
                "capabilities": { "window": { "workDoneProgress": true } },
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    // As in the token test above, the whole exchange runs inside the deadline
    // closure so it can answer `window/workDoneProgress/create` while waiting
    // out the startup scan.
    let stdin = child.stdin.take().unwrap();
    let collected = run_with_deadline(stdin, reader, 120, |stdin, reader| {
        let mut saw_cancellable_begin = false;
        // Same scan-guard race as the token test above: the bar-off notification
        // precedes the flag release, so a command sent on it can lose the CAS.
        // Retry until one actually starts a scan to cancel.
        let mut attempt = 0i64;
        let mut token = String::new();
        let send_attempt = |stdin: &mut std::process::ChildStdin, attempt: i64| -> String {
            let token = format!("cwtools/command/7/{attempt}");
            write_frame_to(
                stdin,
                &jsonrpc_request(
                    100 + attempt,
                    "workspace/executeCommand",
                    serde_json::json!({
                        "command": "reindexWorkspace",
                        "arguments": [],
                        "workDoneToken": token,
                    }),
                ),
            )
            .unwrap();
            token
        };
        for _ in 0..20_000 {
            let Ok(raw) = read_frame(reader) else { break };
            if raw.is_empty() {
                break; // EOF
            }
            let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            if v["id"] == serde_json::json!(100 + attempt) && v.get("result").is_some() {
                if v["result"] == serde_json::json!("Re-index already in progress.") {
                    attempt += 1;
                    saw_cancellable_begin = false;
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    token = send_attempt(stdin, attempt);
                    continue;
                }
                return (saw_cancellable_begin, Some(v["result"].clone()));
            }
            match v["method"].as_str() {
                Some("window/workDoneProgress/create") => write_frame_to(
                    stdin,
                    &serde_json::json!({ "jsonrpc": "2.0", "id": v["id"], "result": null })
                        .to_string(),
                )
                .unwrap(),
                // The startup scan is done; run the command it was blocking.
                Some("loadingBar")
                    if token.is_empty()
                        && v["params"]["enable"] == serde_json::Value::Bool(false) =>
                {
                    token = send_attempt(stdin, attempt);
                }
                // Cancel only once `begin` has landed: the token isn't in the
                // server's registry until then, and a cancel naming a token it
                // doesn't know is dropped.
                Some("$/progress")
                    if v["params"]["token"] == token.as_str()
                        && v["params"]["value"]["kind"] == "begin" =>
                {
                    saw_cancellable_begin =
                        v["params"]["value"]["cancellable"] == serde_json::Value::Bool(true);
                    write_frame_to(
                        stdin,
                        &jsonrpc_notification(
                            "window/workDoneProgress/cancel",
                            serde_json::json!({ "token": token }),
                        ),
                    )
                    .unwrap();
                }
                _ => {}
            }
        }
        (saw_cancellable_begin, None)
    });
    child.kill().ok();

    let (saw_cancellable_begin, result) = collected.expect("timed out; the command never answered");
    assert!(
        saw_cancellable_begin,
        "a command bar must advertise itself as cancellable"
    );
    let result = result.expect("cancelled command never answered");
    assert_eq!(
        result.as_str(),
        Some("Re-index cancelled."),
        "a cancelled command must return, and say so"
    );
}

/// #204: the client cancels a long `workspace/executeCommand` while its scan is
/// running. `tower-lsp` answers `$/cancelRequest` by dropping the handler, so
/// nothing on the normal exit path runs — both progress channels have to be
/// closed by the scan guard's `Drop`, and the guard has to release the scan so
/// the next command can index.
#[test]
fn test_cancelling_a_scan_command_closes_progress_and_frees_the_scan() {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("editor_rules.cwt"), EDITOR_RULES).unwrap();
    let p = ws.path().join("common/national_focus/tree.txt");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "my_focus = {\n    id = my_focus\n}\n").unwrap();

    // Hold every scan open for 3s so the cancel reliably lands mid-scan.
    let mut child = cwtools_server_cmd()
        .env("CWTOOLS_SCAN_HOLD_MS", "3000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(ws.path()),
                "capabilities": { "window": { "workDoneProgress": true } },
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    let rx = spawn_frame_collector(reader);

    // The server blocks its `begin` on this response, so every wait below has to
    // answer it. Returns the first frame matching `want`.
    let pump = |child: &mut std::process::Child,
                secs: u64,
                want: &dyn Fn(&serde_json::Value) -> bool|
     -> Option<serde_json::Value> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            let Ok(v) = rx.recv_timeout(std::time::Duration::from_millis(200)) else {
                continue;
            };
            if v["method"] == "window/workDoneProgress/create" {
                write_frame(
                    child,
                    &serde_json::json!({ "jsonrpc": "2.0", "id": v["id"], "result": null })
                        .to_string(),
                )
                .unwrap();
                continue;
            }
            if want(&v) {
                return Some(v);
            }
        }
        None
    };
    let bar_off = |v: &serde_json::Value| {
        v["method"] == "loadingBar" && v["params"]["enable"] == serde_json::Value::Bool(false)
    };

    // Wait out the startup scan, which holds too.
    assert!(
        pump(&mut child, 30, &bar_off).is_some(),
        "the startup scan never closed its loading bar"
    );

    write_frame(
        &mut child,
        &jsonrpc_request(
            900,
            "workspace/executeCommand",
            serde_json::json!({ "command": "reindexWorkspace", "arguments": [] }),
        ),
    )
    .unwrap();
    // Cancel once the scan has both channels open — the `begin` is the last of
    // the two, so it also proves the create round-trip finished.
    assert!(
        pump(&mut child, 30, &|v| v["method"] == "$/progress"
            && v["params"]["value"]["kind"] == "begin")
        .is_some(),
        "no $/progress begin after reindexWorkspace"
    );

    write_frame(
        &mut child,
        &jsonrpc_notification("$/cancelRequest", serde_json::json!({ "id": 900 })),
    )
    .unwrap();

    let mut saw_bar_off = false;
    let mut saw_progress_end = false;
    let mut response = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline && !(saw_bar_off && saw_progress_end) {
        let Some(v) = pump(&mut child, 5, &|v| {
            v["id"] == 900
                || (v["method"] == "loadingBar"
                    && v["params"]["enable"] == serde_json::Value::Bool(false))
                || (v["method"] == "$/progress" && v["params"]["value"]["kind"] == "end")
        }) else {
            continue;
        };
        if v["id"] == 900 {
            response = Some(v);
        } else if v["method"] == "loadingBar" {
            assert!(
                !saw_bar_off,
                "duplicate loadingBar close after cancellation"
            );
            saw_bar_off = true;
        } else {
            assert!(
                !saw_progress_end,
                "duplicate $/progress end after cancellation"
            );
            saw_progress_end = true;
        }
    }
    assert!(
        saw_bar_off,
        "the cancelled scan left the loading bar spinning"
    );
    assert!(
        saw_progress_end,
        "the cancelled scan left its $/progress token open"
    );

    let response = response
        .or_else(|| pump(&mut child, 10, &|v| v["id"] == 900))
        .expect("the cancelled request never answered");
    assert_eq!(
        response["error"]["code"], -32800,
        "a cancelled request answers RequestCancelled: {response}"
    );

    // The guard releases the scan flag only once the close has gone out, so the
    // first retry can still lose the CAS — the point is that one of them wins.
    let mut reindexed = None;
    for id in 901..906 {
        write_frame(
            &mut child,
            &jsonrpc_request(
                id,
                "workspace/executeCommand",
                serde_json::json!({ "command": "reindexWorkspace", "arguments": [] }),
            ),
        )
        .unwrap();
        let answer = pump(&mut child, 30, &|v| v["id"] == id)
            .unwrap_or_else(|| panic!("reindexWorkspace {id} never answered"));
        if answer["result"] == "Workspace re-indexed." {
            reindexed = Some(answer);
            break;
        }
    }
    child.kill().ok();
    assert!(
        reindexed.is_some(),
        "no scan could start after the cancelled one released the guard"
    );
}

#[test]
fn test_did_change_workspace_folders_repoints_and_rescans() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("editor_rules.cwt"), EDITOR_RULES).unwrap();
    let old = first.path().join("events/old.txt");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, "old_event = { id = old_event }\n").unwrap();
    let p = second.path().join("common/national_focus/tree.txt");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "moved_focus = {\n    id = moved_focus\n}\n").unwrap();

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(first.path()),
                "workspaceFolders": [{ "uri": path_uri(first.path()), "name": "first" }],
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": path_uri(&old),
                    "languageId": "hoi4",
                    "version": 1,
                    "text": "old_event = { id = old_event }\n",
                }
            }),
        ),
    )
    .unwrap();

    // Swap the primary folder for one that holds a focus file.
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "workspace/didChangeWorkspaceFolders",
            serde_json::json!({
                "event": {
                    "added": [{ "uri": path_uri(second.path()), "name": "second" }],
                    "removed": [{ "uri": path_uri(first.path()), "name": "first" }],
                }
            }),
        ),
    )
    .unwrap();

    // The re-scan must index the new root: its file gets diagnostics published.
    let stdin = child.stdin.take().unwrap();
    let old_uri = path_uri(&old);
    let result = run_with_deadline(stdin, reader, 90, move |stdin, reader| {
        let mut requested_old_symbols = false;
        for _ in 0..2000 {
            let Ok(raw) = read_frame(reader) else { break };
            if raw.is_empty() {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            if v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with("common/national_focus/tree.txt"))
                && !requested_old_symbols
            {
                println!("rescan published diagnostics for {}", v["params"]["uri"]);
                write_frame_to(
                    stdin,
                    &jsonrpc_request(
                        99,
                        "textDocument/documentSymbol",
                        serde_json::json!({ "textDocument": { "uri": old_uri } }),
                    ),
                )
                .unwrap();
                requested_old_symbols = true;
            }
            if v["id"] == 99 {
                return requested_old_symbols && v["result"].is_null();
            }
        }
        false
    });
    child.kill().ok();
    assert_eq!(
        result,
        Some(true),
        "the new workspace was not scanned or the removed workspace document stayed open"
    );
}

// ── #98: rules-config errors reach the Problems panel ────────────────────────

/// The rules load runs inside `initialize`, where tower-lsp drops notifications,
/// so the diagnostics behind the "N rules-config error(s)" popup went nowhere.
#[test]
fn test_rules_config_errors_publish_after_initialized() {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        rules_dir.path().join("broken.cwt"),
        "types = {\n  type[foo] = { path = \"common/foo\" }\n}\nr = {\n  a = <undefined_thing>\n  b = <also_missing>\n}\n",
    )
    .unwrap();

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": path_uri(ws.path()),
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
            },
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");

    let body = jsonrpc_notification("initialized", serde_json::json!({}));
    write_frame(&mut child, &body).unwrap();

    let stdin = child.stdin.take().unwrap();
    let saw = run_with_deadline(stdin, reader, 30, |_stdin, reader| {
        for _ in 0..400 {
            let Ok(raw) = read_frame(reader) else {
                return false;
            };
            if raw.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
                continue;
            };
            if v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with("broken.cwt"))
                && v["params"]["diagnostics"]
                    .as_array()
                    .is_some_and(|d| !d.is_empty())
            {
                return true;
            }
        }
        false
    });
    child.kill().ok();
    assert_eq!(
        saw,
        Some(true),
        "no diagnostics published for the broken .cwt file"
    );
}

// ── #107: published diagnostic ranges do not bleed past the statement ─────────

/// Nothing asserted a published diagnostic's range end, which is how the parser's
/// whitespace-absorbing `SourceRange.end` reached users as a squiggle running a
/// line long. CW223 needs no rules to fire, so this is a pure range check.
#[test]
fn test_published_diagnostic_range_stays_on_its_own_line() {
    let ws = tempfile::tempdir().unwrap();
    let rel = "common/scripted_effects/e.txt";
    let text = "my_effect = {\n    NOT = { a = 1 b = 2 }\n    x = yes\n}\n";
    let p = ws.path().join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, text).unwrap();

    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        rules_dir.path().join("types.cwt"),
        "types = {\n    type[scripted_effect] = {\n        path = \"game/common/scripted_effects\"\n    }\n}\n",
    )
    .unwrap();

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": path_uri(ws.path()),
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
            },
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();

    let doc_uri = path_uri(&p);
    let body = jsonrpc_notification(
        "textDocument/didOpen",
        serde_json::json!({
            "textDocument": { "uri": doc_uri, "languageId": "hoi4", "version": 1, "text": text }
        }),
    );
    write_frame(&mut child, &body).unwrap();

    let stdin = child.stdin.take().unwrap();
    let found = run_with_deadline(stdin, reader, 60, |_stdin, reader| {
        // No fixed frame budget: the diagnostic can trail an arbitrary number of
        // other notifications (progress, other files' diagnostics), and under a
        // loaded CI box the server is slow to emit them. The outer deadline is
        // the only bound; `read_frame` errors still fail fast.
        loop {
            let Ok(raw) = read_frame(reader) else {
                return None;
            };
            if raw.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
                continue;
            };
            if v["method"] != "textDocument/publishDiagnostics"
                || !v["params"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with("e.txt"))
                // The workspace scan publishes without a `version` and with the
                // whole-line fallback range (no doc text at hand). The open-doc
                // publishers (didOpen re-validate, post-scan refresh) tag the
                // frame with the document version and publish precise columns.
                // Skip the scan's frame so the race can't hand this test the
                // imprecise range (#179).
                || v["params"]["version"].as_i64().is_none()
            {
                continue;
            }
            if let Some(d) = v["params"]["diagnostics"]
                .as_array()
                .and_then(|ds| ds.iter().find(|d| d["code"] == "CW223"))
            {
                return Some(d["range"].clone());
            }
        }
    });
    child.kill().ok();

    let range = found.flatten().expect("no CW223 diagnostic published");
    // `NOT` sits at line index 1, columns 4..7.
    assert_eq!(range["start"]["line"], 1, "range: {range}");
    assert_eq!(range["start"]["character"], 4, "range: {range}");
    assert_eq!(
        range["end"]["line"], 1,
        "must not spill onto a later line: {range}"
    );
    assert_eq!(range["end"]["character"], 7, "range: {range}");
}

/// The workspace scan and the didOpen re-validate both publish for a file
/// opened mid-scan, and the range test above depends on which frame it picks.
/// This pins both shapes deterministically: e.txt is left unopened until the
/// scan has already published it, so the scan's frame is guaranteed to arrive
/// first. The scan holds no document text, so its CW223 is a single-char span
/// at the raw parser column and the frame carries no `version`; the didOpen
/// re-validate then republishes the precise columns with the document version
/// tagged on the frame. The version-less scan frame is what the range test's
/// filter skips (#179).
#[test]
fn test_scan_publish_falls_back_and_did_open_republishes_precise_range() {
    let ws = tempfile::tempdir().unwrap();
    let rel = "common/scripted_effects/e.txt";
    let text = "my_effect = {\n    NOT = { a = 1 b = 2 }\n    x = yes\n}\n";
    let p = ws.path().join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, text).unwrap();

    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        rules_dir.path().join("types.cwt"),
        "types = {\n    type[scripted_effect] = {\n        path = \"game/common/scripted_effects\"\n    }\n}\n",
    )
    .unwrap();

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": path_uri(ws.path()),
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
            },
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();

    let stdin = child.stdin.take().unwrap();
    let found = run_with_deadline(stdin, reader, 60, move |stdin, reader| {
        // Phase 1: the scan's publish. e.txt is on disk and not open yet, so
        // the scan validates it with no document text and publishes the
        // version-less fallback range.
        let (scan_version, scan_range) = loop {
            let Ok(raw) = read_frame(reader) else {
                return None;
            };
            if raw.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
                continue;
            };
            if v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with("e.txt"))
                && let Some(d) = v["params"]["diagnostics"]
                    .as_array()
                    .and_then(|ds| ds.iter().find(|d| d["code"] == "CW223"))
            {
                break (v["params"]["version"].clone(), d["range"].clone());
            }
        };

        // Phase 2: open the doc, then wait for the open-doc re-validate's
        // publish. The scan's version-less frame is already in the pipe, so
        // skipping it is exactly what the range test's filter must do; the
        // first version-tagged CW223 is always the precise range.
        let doc_uri = path_uri(&p);
        let body = jsonrpc_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": { "uri": doc_uri, "languageId": "hoi4", "version": 1, "text": text }
            }),
        );
        write_frame_to(stdin, &body).ok()?;
        loop {
            let Ok(raw) = read_frame(reader) else {
                return None;
            };
            if raw.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
                continue;
            };
            if v["method"] != "textDocument/publishDiagnostics"
                || !v["params"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with("e.txt"))
                || v["params"]["version"].as_i64().is_none()
            {
                continue;
            }
            if let Some(d) = v["params"]["diagnostics"]
                .as_array()
                .and_then(|ds| ds.iter().find(|d| d["code"] == "CW223"))
            {
                return Some((
                    scan_version,
                    scan_range,
                    v["params"]["version"].clone(),
                    d["range"].clone(),
                ));
            }
        }
    });
    child.kill().ok();

    let (scan_version, scan_range, open_version, open_range) = found
        .flatten()
        .expect("the scan's CW223 frame or the open-doc republish never arrived");
    // The scan's fallback: no version tag, single-char span at the raw column.
    assert!(
        scan_version.is_null(),
        "the scan's publish must carry no version: {scan_version}"
    );
    assert_eq!(scan_range["start"]["line"], 1, "range: {scan_range}");
    assert_eq!(scan_range["start"]["character"], 4, "range: {scan_range}");
    assert_eq!(
        scan_range["end"]["line"], 1,
        "must not spill onto a later line: {scan_range}"
    );
    assert_eq!(
        scan_range["end"]["character"],
        scan_range["start"]["character"].as_i64().unwrap() + 1,
        "no document text means a one-character squiggle: {scan_range}"
    );
    // The open-doc re-validate: version-tagged, precise columns.
    assert_eq!(
        open_version.as_i64(),
        Some(1),
        "the open-doc publish must carry the document version: {open_version}"
    );
    assert_eq!(open_range["start"]["line"], 1, "range: {open_range}");
    assert_eq!(open_range["start"]["character"], 4, "range: {open_range}");
    assert_eq!(
        open_range["end"]["line"], 1,
        "must not spill onto a later line: {open_range}"
    );
    assert_eq!(open_range["end"]["character"], 7, "range: {open_range}");
}

/// A `.cwt` that parsed badly and then parses cleanly has to have its squiggle
/// taken back. Only files *with* errors are published, so without an explicit
/// clear the stale diagnostic sits in the Problems panel forever.
#[test]
fn test_rules_config_diagnostics_clear_after_a_clean_reload() {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let broken = rules_dir.path().join("broken.cwt");
    std::fs::write(
        &broken,
        "types = {\n  type[foo] = { path = \"common/foo\" }\n}\nr = {\n  a = <undefined_thing>\n}\n",
    )
    .unwrap();

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": path_uri(ws.path()),
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
            },
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();

    // Repair the rules on disk, then ask the server to re-read them.
    std::fs::write(
        &broken,
        "types = {\n  type[foo] = { path = \"common/foo\" }\n}\n",
    )
    .unwrap();
    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "workspace/executeCommand",
            serde_json::json!({"command": "reloadrulesconfig", "arguments": []}),
        ),
    )
    .unwrap();

    let stdin = child.stdin.take().unwrap();
    let cleared = run_with_deadline(stdin, reader, 30, |_stdin, reader| {
        for _ in 0..400 {
            let Ok(raw) = read_frame(reader) else {
                return false;
            };
            if raw.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
                continue;
            };
            if v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with("broken.cwt"))
                && v["params"]["diagnostics"]
                    .as_array()
                    .is_some_and(|d| d.is_empty())
            {
                return true;
            }
        }
        false
    });
    child.kill().ok();
    assert_eq!(
        cleared,
        Some(true),
        "the recovered .cwt never had its diagnostics cleared"
    );
}

/// #98: the rules-error toast fired inside `initialize`, where tower-lsp drops
/// notifications, and again from the boot-time `reloadrulesconfig` the client
/// sends after its rules clone. It must arrive exactly once, after the
/// handshake; an unchanged error set must not toast again; a changed one must —
/// even one that keeps the same count and the same first error, which the
/// displayed summary alone cannot distinguish.
#[test]
fn test_rules_config_toast_defers_to_initialized_and_dedupes() {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let broken = rules_dir.path().join("broken.cwt");
    std::fs::write(
        &broken,
        "types = {\n  type[foo] = { path = \"common/foo\" }\n}\nr = {\n  a = <undefined_thing>\n  b = <also_missing>\n}\n",
    )
    .unwrap();

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let body = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": path_uri(ws.path()),
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
            },
        }),
    );
    write_frame(&mut child, &body).unwrap();
    let _ = read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();

    let broken_path = broken.clone();
    let stdin = child.stdin.take().unwrap();
    let phases = run_with_deadline(stdin, reader, 60, move |stdin, reader| {
        let is_toast = |v: &serde_json::Value| v["method"] == "window/showMessage";
        // Toasts seen until a response with `id` arrives; None = frame cap hit.
        let toasts_until_response =
            |reader: &mut BufReader<std::process::ChildStdout>, id: i64| -> Option<usize> {
                let mut n = 0usize;
                for _ in 0..400 {
                    let raw = read_frame(reader).ok()?;
                    if raw.is_empty() {
                        continue;
                    }
                    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
                        continue;
                    };
                    if is_toast(&v) {
                        n += 1;
                    }
                    if v["id"] == id {
                        return Some(n);
                    }
                }
                None
            };

        // Phase 1: the deferred boot toast arrives with no further prompting.
        let mut boot_toast = false;
        for _ in 0..400 {
            let Ok(raw) = read_frame(reader) else {
                return None;
            };
            if raw.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw)
                && is_toast(&v)
            {
                boot_toast = true;
                break;
            }
        }

        // Phase 2: reloading the same broken rules must not toast again.
        write_frame_to(
            stdin,
            &jsonrpc_request(
                2,
                "workspace/executeCommand",
                serde_json::json!({"command": "reloadrulesconfig", "arguments": []}),
            ),
        )
        .ok()?;
        let same_reload = toasts_until_response(reader, 2)?;

        // Phase 3: a different error set toasts again — here one that keeps
        // the count (2) and the first error, so a summary-keyed dedupe would
        // wrongly swallow it.
        std::fs::write(
            &broken_path,
            "types = {\n  type[foo] = { path = \"common/foo\" }\n}\nr = {\n  a = <undefined_thing>\n  b = <other_thing>\n}\n",
        )
        .ok()?;
        write_frame_to(
            stdin,
            &jsonrpc_request(
                3,
                "workspace/executeCommand",
                serde_json::json!({"command": "reloadrulesconfig", "arguments": []}),
            ),
        )
        .ok()?;
        let changed_reload = toasts_until_response(reader, 3)?;

        Some((boot_toast, same_reload, changed_reload))
    });
    child.kill().ok();

    let (boot_toast, same_reload, changed_reload) =
        phases.flatten().expect("server went quiet mid-test");
    assert!(
        boot_toast,
        "the boot rules-error toast must arrive after `initialized`, not be dropped"
    );
    assert_eq!(
        same_reload, 0,
        "reloading an unchanged error set must not toast again"
    );
    assert_eq!(
        changed_reload, 1,
        "a changed error set must toast exactly once"
    );
}

/// #193: `reloadrulesconfig` arriving while another scan holds the scan guard
/// must not skip the revalidation. The client fires this command right after
/// the startup scan's loading bar ends, but the bar-off notification is sent
/// before the guard drops, so the reload races the tail of that scan — whose
/// diagnostics were produced with no rules loaded. The command must retry
/// until it wins the CAS and run one full revalidation before answering, and
/// the response must report that a revalidation actually ran.
#[test]
fn test_reloadrulesconfig_retries_until_it_wins_the_scan_guard() {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    // Hold every scan open for 4s so the reload reliably lands mid-scan. The
    // startup scan's own hold is waited out by storm_server_env.
    let (mut child, reader) = storm_server_env(
        ws.path(),
        rules_dir.path(),
        vanilla.path(),
        &[("CWTOOLS_SCAN_HOLD_MS", "4000")],
    );
    let rx = spawn_frame_collector(reader);

    // Start a competing scan and wait for the scan-started signal, so the hold
    // is definitely active when the reload arrives.
    reindex_until_scan_starts(&mut child, &rx);

    // Fire the reload while the scan holds the CAS: it must not answer until
    // the competing scan is gone and a revalidation has run.
    write_frame(
        &mut child,
        &jsonrpc_request(
            901,
            "workspace/executeCommand",
            serde_json::json!({ "command": "reloadrulesconfig", "arguments": [] }),
        ),
    )
    .unwrap();
    assert_no_response_within(
        &rx,
        901,
        std::time::Duration::from_secs(2),
        "reloadrulesconfig must not answer while the competing scan holds the guard",
    );

    // The competing scan releases after its hold, the retry wins the CAS, and
    // one full revalidation (a second loadingBar on→off cycle) must complete
    // before the success response.
    let (response, saw_revalidation) = wait_for_response_watching_scans(
        &rx,
        901,
        std::time::Duration::from_secs(30),
        "reloadrulesconfig to answer once it won the guard",
    );
    child.kill().ok();

    assert!(
        saw_revalidation,
        "no revalidation ran between the reload and its response"
    );
    let msg = response["result"].as_str().expect("string result");
    assert!(
        msg.contains("workspace re-validated"),
        "the reload must report the revalidation it ran: {msg}"
    );
}

/// #193: when the scan guard never gives way before the retry deadline, the
/// reload must answer honestly that re-validation is queued — it must not
/// claim a revalidation it skipped, and it must give up on time instead of
/// hanging the command behind the competing scan.
#[test]
fn test_reloadrulesconfig_reports_queued_revalidation_when_scan_never_releases() {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    // Hold every scan open for 10s, but give the reload only 1s to win the
    // guard: it must give up and report the pending state.
    let (mut child, reader) = storm_server_env(
        ws.path(),
        rules_dir.path(),
        vanilla.path(),
        &[
            ("CWTOOLS_SCAN_HOLD_MS", "10000"),
            ("CWTOOLS_RETRY_DEADLINE_MS", "1000"),
        ],
    );
    let rx = spawn_frame_collector(reader);

    // Start a competing scan and wait for the scan-started signal, so the hold
    // is definitely active for the reload's whole deadline.
    reindex_until_scan_starts(&mut child, &rx);

    // Fire the reload. Its 1s deadline expires while the 10s hold is still
    // active, so the answer must arrive promptly, report the pending state,
    // and no revalidation scan may have run.
    write_frame(
        &mut child,
        &jsonrpc_request(
            901,
            "workspace/executeCommand",
            serde_json::json!({ "command": "reloadrulesconfig", "arguments": [] }),
        ),
    )
    .unwrap();
    // The competing scan releases only after 10s, so any answer inside this
    // window must be the reload's own give-up response.
    let (response, saw_revalidation) = wait_for_response_watching_scans(
        &rx,
        901,
        std::time::Duration::from_secs(5),
        "reloadrulesconfig to give up on its 1s deadline",
    );
    child.kill().ok();

    assert!(
        !saw_revalidation,
        "a revalidation ran after the reload gave up"
    );
    let msg = response["result"].as_str().expect("string result");
    assert!(
        msg.contains("re-validation queued behind the running scan"),
        "the reload must report the queued revalidation: {msg}"
    );
}

/// #193: when the reload gives up on the guard before its deadline, the
/// revalidation must still land once the competing scan releases — the
/// give-up hands off to a bounded background retry instead of leaving the
/// stale no-rules diagnostics until the next edit.
#[test]
fn test_reloadrulesconfig_give_up_lands_queued_revalidation() {
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    let signals = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    // This test cares about both ends of the hold: the reload's 1s deadline has
    // to expire inside it, and the deferred retry has to run once it releases.
    // A fixed hold makes both a bet on wall-clock that parallel load can lose,
    // so the competing scan holds while `gate` exists and this test moves the
    // gate itself (#198). The file lives outside the workspace so creating it
    // isn't a watched-file change.
    let gate = signals.path().join("scan-hold");
    let gate_env = gate.to_string_lossy().into_owned();
    let (mut child, reader) = storm_server_env(
        ws.path(),
        rules_dir.path(),
        vanilla.path(),
        &[
            ("CWTOOLS_SCAN_HOLD_FILE", gate_env.as_str()),
            ("CWTOOLS_RETRY_DEADLINE_MS", "1000"),
        ],
    );
    let rx = spawn_frame_collector(reader);

    // Arm the hold (the startup scan is already done, so it isn't caught by
    // it), then start the competing scan that trips it.
    std::fs::write(&gate, "").unwrap();
    reindex_until_scan_starts(&mut child, &rx);

    // The give-up response arrives ~1s in, with the scan still held.
    write_frame(
        &mut child,
        &jsonrpc_request(
            901,
            "workspace/executeCommand",
            serde_json::json!({ "command": "reloadrulesconfig", "arguments": [] }),
        ),
    )
    .unwrap();
    let (response, _) = wait_for_response_watching_scans(
        &rx,
        901,
        std::time::Duration::from_secs(30),
        "reloadrulesconfig to give up on its 1s deadline",
    );
    let msg = response["result"].as_str().expect("string result");
    assert!(
        msg.contains("re-validation queued"),
        "the reload must report the queued revalidation: {msg}"
    );

    // Release the hold. The deferred retry then wins the CAS and runs a full
    // revalidation, a bar-on that can only come after the response.
    std::fs::remove_file(&gate).unwrap();
    wait_for_scan_started(
        &rx,
        std::time::Duration::from_secs(30),
        "the deferred revalidation to run after the reload gave up",
    );
    child.kill().ok();
}

// ── #162: the cache path the client names ────────────────────────────────────

/// `initializationOptions.vanillaCache` is a client-chosen path read inside
/// `initialize` itself. Pointed at a character device it read to EOF there, so
/// the handshake never came back and the window sat dead with no diagnostics.
/// The server must refuse the input and finish the handshake, then still be
/// answering afterwards rather than wedged behind the same read.
#[cfg(unix)]
#[test]
fn a_vanilla_cache_naming_a_character_device_does_not_stall_initialize() {
    let ws = tempfile::tempdir().unwrap();
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let reader = BufReader::new(child.stdout.take().unwrap());
    let stdin = child.stdin.take().unwrap();
    let ws_uri = path_uri(ws.path());
    let script = ws.path().join("probe.txt");
    std::fs::write(&script, "a = { b = 1 }\n").unwrap();
    let script_uri = path_uri(&script);

    let answered = run_with_deadline(stdin, reader, 30, move |stdin, reader| {
        let init = jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": ws_uri,
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "vanillaCache": "/dev/zero",
                }
            }),
        );
        write_frame_to(stdin, &init).ok()?;
        read_response(reader).ok()?;
        write_frame_to(
            stdin,
            &jsonrpc_notification("initialized", serde_json::json!({})),
        )
        .ok()?;
        write_frame_to(
            stdin,
            &jsonrpc_request(
                2,
                "textDocument/foldingRange",
                serde_json::json!({ "textDocument": { "uri": script_uri } }),
            ),
        )
        .ok()?;
        serde_json::from_str::<serde_json::Value>(&read_response(reader).ok()?).ok()
    });
    child.kill().ok();
    // Reap it: a server that blew its deadline is still reading the device.
    child.wait().ok();

    let response = answered
        .flatten()
        .expect("initialize never came back with vanillaCache = /dev/zero");
    assert!(
        response.get("error").is_none(),
        "the server stopped answering after refusing the cache: {response}"
    );
}

// ── #163: the URI access boundary ────────────────────────────────────────────
// `textDocument/foldingRange` is the cleanest probe: it needs nothing but the
// file's text, so its answer is a direct read-out of whether the server was
// willing to read the URI. Ranges = allowed, null = refused.

/// Assert `response` is a *refusal*: a successful JSON-RPC reply carrying an
/// empty result. A transport-level error reply also has a null `result`, so
/// checking that alone would let a crashed handler pass as a clean refusal.
fn assert_refused(response: &serde_json::Value, what: &str) {
    assert!(
        response.get("error").is_none(),
        "{what} must be refused with a successful empty reply, got a JSON-RPC error: {response}"
    );
    let result = &response["result"];
    let empty = result.is_null() || result.as_array().is_some_and(|a| a.is_empty());
    assert!(empty, "{what} must be refused, got: {result}");
}

/// The folding ranges from a successful reply.
fn expect_ranges(response: &serde_json::Value) -> &Vec<serde_json::Value> {
    assert!(
        response.get("error").is_none(),
        "expected a successful reply, got a JSON-RPC error: {response}"
    );
    response["result"].as_array().expect("folding ranges")
}

/// Boot a server rooted at `ws`, optionally open `open` (uri, text) as buffers,
/// then ask for `uri`'s folding ranges. Returns the whole JSON-RPC response so
/// callers can tell a refusal from an error reply.
fn folding_ranges_for(
    ws: &std::path::Path,
    open: &[(&str, &str)],
    uri: &str,
) -> Option<serde_json::Value> {
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let reader = BufReader::new(child.stdout.take().unwrap());
    let stdin = child.stdin.take().unwrap();
    let ws_uri = path_uri(ws);
    let open: Vec<(String, String)> = open
        .iter()
        .map(|(u, t)| ((*u).to_string(), (*t).to_string()))
        .collect();
    let uri = uri.to_string();

    // Deadline-bounded: before the boundary existed a `/dev/zero` request read
    // until it ran the machine out of memory rather than answering.
    let result = run_with_deadline(stdin, reader, 30, move |stdin, reader| {
        let init = jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": ws_uri,
                "capabilities": {},
                "initializationOptions": { "language": "hoi4" }
            }),
        );
        write_frame_to(stdin, &init).ok()?;
        read_response(reader).ok()?;
        write_frame_to(
            stdin,
            &jsonrpc_notification("initialized", serde_json::json!({})),
        )
        .ok()?;
        wait_for_scan_done(reader);
        for (u, text) in &open {
            write_frame_to(
                stdin,
                &jsonrpc_notification(
                    "textDocument/didOpen",
                    serde_json::json!({
                        "textDocument": {"uri": u, "languageId": "hoi4", "version": 1, "text": text}
                    }),
                ),
            )
            .ok()?;
        }
        // A didOpen and the request after it are dispatched concurrently, so
        // when a buffer is expected the request is retried until it lands.
        // Nothing to wait for otherwise: the disk read is immediate.
        let attempts = if open.is_empty() { 1 } else { 40 };
        let mut response = serde_json::Value::Null;
        for attempt in 0..attempts {
            write_frame_to(
                stdin,
                &jsonrpc_request(
                    2 + attempt,
                    "textDocument/foldingRange",
                    serde_json::json!({ "textDocument": { "uri": uri } }),
                ),
            )
            .ok()?;
            response = serde_json::from_str(&read_response(reader).ok()?).ok()?;
            if !response["result"].is_null() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Some(response)
    });
    child.kill().ok();
    // Reap it: a server that blew its deadline is still running, and on the
    // `/dev/zero` path it is still allocating.
    child.wait().ok();
    result.flatten()
}

/// A workspace tempdir holding one foldable script file, plus its URI.
fn boundary_workspace() -> (tempfile::TempDir, String) {
    let ws = tempfile::tempdir().unwrap();
    let file = ws.path().join("common/national_focus/f.txt");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "outer = {\n    inner = {\n        x = 1\n    }\n}\n").unwrap();
    let uri = path_uri(&file);
    (ws, uri)
}

#[test]
fn test_access_boundary_allows_a_closed_workspace_file() {
    // The no-regression half: the boundary must not cost the server its ability
    // to read files it was pointed at but that aren't open in a buffer.
    let (ws, uri) = boundary_workspace();
    let response = folding_ranges_for(ws.path(), &[], &uri).expect("server went quiet");
    let ranges = expect_ranges(&response);
    assert!(
        ranges
            .iter()
            .any(|r| r["startLine"] == 0 && r["endLine"] == 4),
        "expected the outer fold, got: {response}"
    );
}

#[test]
fn test_access_boundary_refuses_a_file_outside_the_workspace() {
    let (ws, _) = boundary_workspace();
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("f.txt");
    std::fs::write(&file, "outer = {\n    inner = {\n        x = 1\n    }\n}\n").unwrap();
    let response = folding_ranges_for(ws.path(), &[], &path_uri(&file)).expect("server went quiet");
    assert_refused(&response, "a readable file outside every root");
}

#[test]
fn test_access_boundary_refuses_an_open_buffer_outside_the_workspace() {
    let (ws, _) = boundary_workspace();
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("f.txt");
    let text = "outer = {\n    inner = {\n        x = 1\n    }\n}\n";
    std::fs::write(&file, text).unwrap();
    let uri = path_uri(&file);
    let response = folding_ranges_for(ws.path(), &[(&uri, text)], &uri).expect("server went quiet");
    assert_refused(&response, "an open buffer outside every workspace folder");
}

#[test]
fn test_access_boundary_refuses_a_non_file_uri() {
    let (ws, file_uri) = boundary_workspace();
    // The same in-workspace file under a non-`file` scheme. `Url::to_file_path`
    // ignores the scheme, so this used to be read exactly like the `file:` form
    // — pointing it at a file that IS allowed is what makes the scheme the only
    // thing under test.
    let http_uri = file_uri.replacen("file://", "http://localhost", 1);
    for uri in [http_uri.as_str(), "untitled:Untitled-1"] {
        let response = folding_ranges_for(ws.path(), &[], uri).expect("server went quiet");
        assert_refused(&response, uri);
    }
}

#[cfg(unix)]
#[test]
fn test_access_boundary_refuses_a_character_device() {
    // The reported crash: `file:///dev/zero` was converted to a path and read to
    // EOF, which never comes. The server must answer, and answer nothing.
    if !std::path::Path::new("/dev/zero").exists() {
        return;
    }
    let (ws, _) = boundary_workspace();
    let response =
        folding_ranges_for(ws.path(), &[], "file:///dev/zero").expect("server hung on /dev/zero");
    assert_refused(&response, "/dev/zero");
}

#[cfg(unix)]
#[test]
fn test_access_boundary_allows_an_auto_discovered_vanilla_install() {
    // A `vanilla` init option is optional: with none, the server probes the
    // usual Steam library paths under $HOME. That install is indexed, so goto
    // and hover land in it — and the boundary has to allow reading it, which it
    // only does if the resolved dir makes it back into the config.
    let home = tempfile::tempdir().unwrap();
    let vanilla = home
        .path()
        .join(".steam/steam/steamapps/common/Hearts of Iron IV");
    let vanilla_file = vanilla.join("common/national_focus/base.txt");
    std::fs::create_dir_all(vanilla_file.parent().unwrap()).unwrap();
    std::fs::write(
        &vanilla_file,
        "outer = {\n    inner = {\n        x = 1\n    }\n}\n",
    )
    .unwrap();

    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let mut child = cwtools_server_cmd()
        .env("HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    write_frame(
        &mut child,
        &jsonrpc_request(
            1,
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": path_uri(ws.path()),
                "capabilities": {},
                // No `vanilla`: auto-discovery is the thing under test.
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.path().to_string_lossy(),
                }
            }),
        ),
    )
    .unwrap();
    read_response(&mut reader).expect("no init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);

    write_frame(
        &mut child,
        &jsonrpc_request(
            2,
            "textDocument/foldingRange",
            serde_json::json!({ "textDocument": { "uri": path_uri(&vanilla_file) } }),
        ),
    )
    .unwrap();
    let response: serde_json::Value =
        serde_json::from_str(&read_response(&mut reader).expect("no response")).unwrap();
    child.kill().ok();
    child.wait().ok();

    let ranges = expect_ranges(&response);
    assert!(
        ranges
            .iter()
            .any(|r| r["startLine"] == 0 && r["endLine"] == 4),
        "an auto-discovered base-game file must be readable, got: {response}"
    );
}

/// Every game id `Game::from_str` accepts, with the Steam `steamapps/common`
/// folder `discover_vanilla_dir` has to map it to. ARCHITECTURE.md lists that
/// mapping as one of the nine sites a new game touches in lockstep, and as one
/// of the ones the compiler cannot catch: the match ends in `_ => None`, so a
/// missing or misspelled arm compiles and quietly discovers nothing forever.
/// `eu5` and `custom` are deliberately absent — neither ships on Steam under a
/// known folder, and both take the `None` arm today.
#[cfg(unix)]
const STEAM_INSTALL_FOLDERS: &[(&str, &str)] = &[
    ("hoi4", "Hearts of Iron IV"),
    ("stellaris", "Stellaris"),
    ("eu4", "Europa Universalis IV"),
    ("ck2", "Crusader Kings II"),
    ("ck3", "Crusader Kings III"),
    ("vic2", "Victoria 2"),
    ("vic3", "Victoria 3"),
    ("ir", "ImperatorRome"),
];

/// Boot a server for `game` against a `$HOME` holding a base-game file under
/// `folder`, and ask for that file's folding ranges. Ranges mean the install
/// was discovered — nothing else puts a path outside the workspace inside the
/// access boundary — and an empty result means it was not.
#[cfg(unix)]
fn auto_discovered_vanilla_response(game: &str, folder: &str) -> serde_json::Value {
    let home = tempfile::tempdir().unwrap();
    let vanilla_file = home
        .path()
        .join(".steam/steam/steamapps/common")
        .join(folder)
        .join("common/national_focus/base.txt");
    std::fs::create_dir_all(vanilla_file.parent().unwrap()).unwrap();
    std::fs::write(
        &vanilla_file,
        "outer = {\n    inner = {\n        x = 1\n    }\n}\n",
    )
    .unwrap();

    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();

    let mut child = cwtools_server_cmd()
        .env("HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn");
    let reader = BufReader::new(child.stdout.take().unwrap());
    let stdin = child.stdin.take().unwrap();
    let ws_uri = path_uri(ws.path());
    let rules = rules_dir.path().to_string_lossy().to_string();
    let file_uri = path_uri(&vanilla_file);
    let game = game.to_string();

    let response = run_with_deadline(stdin, reader, 60, move |stdin, reader| {
        write_frame_to(
            stdin,
            &jsonrpc_request(
                1,
                "initialize",
                serde_json::json!({
                    "processId": std::process::id(),
                    "rootUri": ws_uri,
                    "capabilities": {},
                    // No `vanilla`: auto-discovery is the thing under test.
                    "initializationOptions": { "language": game, "rulesCache": rules }
                }),
            ),
        )
        .ok()?;
        read_response(reader).ok()?;
        write_frame_to(
            stdin,
            &jsonrpc_notification("initialized", serde_json::json!({})),
        )
        .ok()?;
        wait_for_scan_done(reader);
        write_frame_to(
            stdin,
            &jsonrpc_request(
                2,
                "textDocument/foldingRange",
                serde_json::json!({ "textDocument": { "uri": file_uri } }),
            ),
        )
        .ok()?;
        serde_json::from_str::<serde_json::Value>(&read_response(reader).ok()?).ok()
    });
    child.kill().ok();
    child.wait().ok();
    response.flatten().expect("the server never answered")
}

#[cfg(unix)]
#[test]
fn test_steam_folder_is_mapped_for_every_supported_game() {
    for (game, folder) in STEAM_INSTALL_FOLDERS {
        let response = auto_discovered_vanilla_response(game, folder);
        assert!(
            !response["result"].is_null(),
            "{game} must auto-discover its install under {folder:?}; an empty \
             result means the file was refused as outside the boundary, so the \
             mapping produced no install: {response}"
        );
        let ranges = expect_ranges(&response);
        assert!(
            ranges
                .iter()
                .any(|r| r["startLine"] == 0 && r["endLine"] == 4),
            "{game} must auto-discover its install under {folder:?}, got: {response}"
        );
    }
}

#[cfg(unix)]
#[test]
fn test_steam_folder_mapping_is_per_game_not_a_wildcard() {
    // The positive case above passes for any implementation that discovers
    // *something* under `steamapps/common`. This is what makes it a mapping:
    // an install sitting under another game's folder name is not this game's.
    let response = auto_discovered_vanilla_response("hoi4", "Stellaris");
    assert_refused(
        &response,
        "an install under a different game's Steam folder",
    );
}

// ── Loc second-class fix: outline, references, rename ────────────────────

#[test]
fn test_loc_outline_shows_keys_in_yml() {
    let yml = "l_english:\n my_key:0 \"Hello\"\n my_key_desc:0 \"Desc\"\n my_other:0 \"World\"\n";
    let files = &[("localisation/test_l_english.yml", yml)];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["localisation/test_l_english.yml"],
        serde_json::json!({"textDocument": {"documentSymbol": {"hierarchicalDocumentSymbolSupport": false}}}),
        "localisation/test_l_english.yml",
        "textDocument/documentSymbol",
        serde_json::json!({}),
    );
    let syms = result.as_array().expect("loc outline array");
    let names: Vec<String> = syms
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        names.contains(&"my_key".to_string()),
        "my_key in {:?}",
        names
    );
    assert!(
        names.contains(&"my_key_desc".to_string()),
        "my_key_desc in {:?}",
        names
    );
    assert!(
        names.contains(&"my_other".to_string()),
        "my_other in {:?}",
        names
    );
}

#[test]
fn test_loc_references_find_script_usages() {
    let yml = "l_english:\n my_loc_key:0 \"Hello\"\n";
    let script = "generated = {\n    title = my_loc_key\n    desc = my_loc_key\n}\n";
    let files = &[
        ("localisation/test_l_english.yml", yml),
        ("common/test/event.txt", script),
    ];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["localisation/test_l_english.yml", "common/test/event.txt"],
        serde_json::json!({}),
        "localisation/test_l_english.yml",
        "textDocument/references",
        serde_json::json!({
            "position": {"line": 1, "character": 2},
            "context": {"includeDeclaration": false}
        }),
    );
    let locs = result.as_array().expect("references array");
    assert!(!locs.is_empty(), "expected script usages, got null/empty");
    let has_script = locs
        .iter()
        .any(|l| l["uri"].as_str().unwrap().contains("event.txt"));
    assert!(has_script, "script usage not found in {:?}", result);
}

#[test]
fn test_loc_rename_with_desc_sibling() {
    let yml =
        "l_english:\n my_item:0 \"Item\"\n my_item_desc:0 \"Desc\"\n my_item_tooltip:0 \"Tip\"\n";
    let script = "thing = {\n    name = my_item\n    tooltip = my_item_tooltip\n}\n";
    let files = &[
        ("localisation/test_l_english.yml", yml),
        ("common/test/thing.txt", script),
    ];
    // Prepare rename range
    let prep = feature_request(
        GOTO_RULES,
        files,
        &["localisation/test_l_english.yml"],
        serde_json::json!({}),
        "localisation/test_l_english.yml",
        "textDocument/prepareRename",
        serde_json::json!({"position": {"line": 1, "character": 2}}),
    );
    assert!(
        !prep.is_null(),
        "prepareRename for loc key must not be null, got {:?}",
        prep
    );
    // Rename
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();
    for (rel, content) in files {
        let p = ws.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
    }
    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": ws_uri,
            "capabilities": {"workspace": {"workspaceEdit": {"documentChanges": true}}},
            "initializationOptions": {"language": "hoi4", "rulesCache": rules_dir.path().to_string_lossy(), "vanilla": vanilla.path().to_string_lossy()}
        }),
    );
    write_frame(&mut child, &init).unwrap();
    let _ = read_response(&mut reader).unwrap();
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);
    {
        let rel = "localisation/test_l_english.yml";
        let content = files.iter().find(|(r, _)| *r == rel).unwrap().1;
        let uri = path_uri(ws.path().join(rel));
        write_frame(
            &mut child,
            &jsonrpc_notification(
                "textDocument/didOpen",
                serde_json::json!({"textDocument": {"uri": uri, "languageId": "hoi4", "version": 1, "text": content}}),
            ),
        )
        .unwrap();
        wait_for_diagnostics(&mut reader, rel);
    }
    let uri = path_uri(ws.path().join("localisation/test_l_english.yml"));
    let req = jsonrpc_request(
        10,
        "textDocument/rename",
        serde_json::json!({"textDocument": {"uri": uri}, "position": {"line": 1, "character": 2}, "newName": "new_item"}),
    );
    write_frame(&mut child, &req).unwrap();
    let resp_str = read_response(&mut reader).unwrap();
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    child.kill().ok();
    let edit = &resp["result"];
    assert!(
        !edit.is_null(),
        "rename result must not be null, got {}",
        resp_str
    );
    let new_texts = {
        let mut v = Vec::new();
        if let Some(arr) = edit["documentChanges"].as_array() {
            for doc in arr {
                if let Some(edits) = doc["edits"].as_array() {
                    for e in edits {
                        if let Some(t) = e["newText"].as_str() {
                            v.push(t.to_string());
                        }
                    }
                }
            }
        } else if let Some(map) = edit["changes"].as_object() {
            for (_, edits) in map {
                if let Some(arr) = edits.as_array() {
                    for e in arr {
                        if let Some(t) = e["newText"].as_str() {
                            v.push(t.to_string());
                        }
                    }
                }
            }
        }
        v
    };
    assert!(
        new_texts.contains(&"new_item".to_string()),
        "new_item in edit, got {:?}",
        new_texts
    );
    assert!(
        new_texts.contains(&"new_item_desc".to_string()),
        "sibling _desc renamed, got {:?}",
        new_texts
    );
    assert!(
        new_texts.contains(&"new_item_tooltip".to_string()),
        "sibling _tooltip renamed, got {:?}",
        new_texts
    );
    assert!(
        !new_texts.iter().any(|s| s.contains("extra")),
        "unexpected extra sibling, got {:?}",
        new_texts
    );
}

#[test]
fn test_loc_outline_hierarchical_vs_flat() {
    let yml = "l_english:\n aaa:0 \"A\"\n bbb:0 \"B\"\n";
    let files = &[("localisation/test_l_english.yml", yml)];
    // Flat
    let flat = feature_request(
        GOTO_RULES,
        files,
        &["localisation/test_l_english.yml"],
        serde_json::json!({"textDocument": {"documentSymbol": {"hierarchicalDocumentSymbolSupport": false}}}),
        "localisation/test_l_english.yml",
        "textDocument/documentSymbol",
        serde_json::json!({}),
    );
    let flat_arr = flat.as_array().expect("flat array");
    assert_eq!(
        flat_arr.len(),
        2,
        "flat outline should have 2 symbols, got {:?}",
        flat
    );
    assert!(
        flat_arr[0]["kind"].is_number(),
        "SymbolInformation must have kind"
    );
    // Hierarchical
    let hierarchical = feature_request(
        GOTO_RULES,
        files,
        &["localisation/test_l_english.yml"],
        serde_json::json!({"textDocument": {"documentSymbol": {"hierarchicalDocumentSymbolSupport": true}}}),
        "localisation/test_l_english.yml",
        "textDocument/documentSymbol",
        serde_json::json!({}),
    );
    let hier_arr = hierarchical.as_array().expect("hierarchical array");
    assert_eq!(
        hier_arr.len(),
        2,
        "hierarchical outline should have 2 symbols, got {:?}",
        hierarchical
    );
    assert!(
        hier_arr[0]["selectionRange"].is_object(),
        "DocumentSymbol must have selectionRange"
    );
}

#[test]
fn test_loc_outline_outside_localisation_is_empty() {
    let yml = "l_english:\n my_key:0 \"Hello\"\n";
    // File under .github is has_loc_ext but not is_loc_file, so outline should be empty (no game AST)
    let files = &[(".github/workflows/ci.yml", yml)];
    let result = feature_request(
        GOTO_RULES,
        files,
        &[".github/workflows/ci.yml"],
        serde_json::json!({"textDocument": {"documentSymbol": {"hierarchicalDocumentSymbolSupport": false}}}),
        ".github/workflows/ci.yml",
        "textDocument/documentSymbol",
        serde_json::json!({}),
    );
    assert!(
        result.is_null() || result.as_array().map(|a| a.is_empty()).unwrap_or(false),
        "outside localisation dir should have no outline, got {:?}",
        result
    );
}

#[test]
fn test_loc_references_include_declaration_flag() {
    let yml = "l_english:\n my_key:0 \"Hello\"\n";
    let script = "x = {\n    title = my_key\n}\n";
    let files = &[
        ("localisation/test_l_english.yml", yml),
        ("common/test/event.txt", script),
    ];
    let with_decl = feature_request(
        GOTO_RULES,
        files,
        &["localisation/test_l_english.yml", "common/test/event.txt"],
        serde_json::json!({}),
        "localisation/test_l_english.yml",
        "textDocument/references",
        serde_json::json!({"position": {"line": 1, "character": 2}, "context": {"includeDeclaration": true}}),
    );
    let locs_true = with_decl.as_array().expect("refs with decl");
    assert!(
        locs_true
            .iter()
            .any(|l| l["uri"].as_str().unwrap().contains("test_l_english.yml")),
        "includeDeclaration true must include yml definition, got {:?}",
        with_decl
    );
    let without_decl = feature_request(
        GOTO_RULES,
        files,
        &["localisation/test_l_english.yml", "common/test/event.txt"],
        serde_json::json!({}),
        "localisation/test_l_english.yml",
        "textDocument/references",
        serde_json::json!({"position": {"line": 1, "character": 2}, "context": {"includeDeclaration": false}}),
    );
    let locs_false = without_decl.as_array().expect("refs without decl");
    assert!(
        !locs_false
            .iter()
            .any(|l| l["uri"].as_str().unwrap().contains("test_l_english.yml")),
        "includeDeclaration false must not include yml definition, got {:?}",
        without_decl
    );
    assert!(
        locs_false
            .iter()
            .any(|l| l["uri"].as_str().unwrap().contains("event.txt")),
        "script usage must still be present, got {:?}",
        without_decl
    );
}

#[test]
fn test_loc_references_case_insensitive_and_whole_token() {
    let yml = "l_english:\n my_key:0 \"Hello\"\n";
    // MY_KEY different case should match, my_key_extra should NOT, comment should NOT
    let script = "x = {\n    title = MY_KEY\n    other = my_key_extra\n    # my_key comment\n    quoted = \"my_key\"\n}\n";
    let files = &[
        ("localisation/test_l_english.yml", yml),
        ("common/test/event.txt", script),
    ];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["localisation/test_l_english.yml", "common/test/event.txt"],
        serde_json::json!({}),
        "localisation/test_l_english.yml",
        "textDocument/references",
        serde_json::json!({"position": {"line": 1, "character": 2}, "context": {"includeDeclaration": false}}),
    );
    let locs = result.as_array().expect("refs");
    // Filter to event.txt locations
    let script_locs: Vec<_> = locs
        .iter()
        .filter(|l| l["uri"].as_str().unwrap().contains("event.txt"))
        .collect();
    // Should find MY_KEY (line 1) and quoted "my_key" (line 4), but not my_key_extra nor comment
    assert_eq!(
        script_locs.len(),
        2,
        "expected 2 script usages (case-insensitive, whole-token, quoted), got {:?}",
        result
    );
    let lines: Vec<u64> = script_locs
        .iter()
        .map(|l| l["range"]["start"]["line"].as_u64().unwrap())
        .collect();
    assert!(
        lines.contains(&1),
        "MY_KEY on line 1 not found, got {:?}",
        lines
    );
    assert!(
        lines.contains(&4),
        "quoted my_key on line 4 not found, got {:?}",
        lines
    );
}

#[test]
fn test_loc_references_from_script_side() {
    let yml = "l_english:\n my_key:0 \"Hello\"\n";
    let script = "x = {\n    title = my_key\n}\n";
    let files = &[
        ("localisation/test_l_english.yml", yml),
        ("common/test/event.txt", script),
    ];
    // Cursor on script usage (line 1, inside my_key)
    let result = feature_request(
        GOTO_RULES,
        files,
        &["localisation/test_l_english.yml", "common/test/event.txt"],
        serde_json::json!({}),
        "common/test/event.txt",
        "textDocument/references",
        serde_json::json!({"position": {"line": 1, "character": 13}, "context": {"includeDeclaration": true}}),
    );
    let locs = result.as_array().expect("refs from script");
    assert!(
        locs.iter()
            .any(|l| l["uri"].as_str().unwrap().contains("test_l_english.yml")),
        "script-side references must include yml definition, got {:?}",
        result
    );
}

#[test]
fn test_loc_rename_only_existing_siblings() {
    let yml = "l_english:\n my_item:0 \"Item\"\n my_item_desc:0 \"Desc\"\n";
    // No my_item_tooltip exists, so rename should NOT produce it
    let files = &[("localisation/test_l_english.yml", yml)];
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();
    for (rel, content) in files {
        let p = ws.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
    }
    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({"processId": std::process::id(), "rootUri": ws_uri, "capabilities": {"workspace": {"workspaceEdit": {"documentChanges": true}}}, "initializationOptions": {"language": "hoi4", "rulesCache": rules_dir.path().to_string_lossy(), "vanilla": vanilla.path().to_string_lossy()}}),
    );
    write_frame(&mut child, &init).unwrap();
    let _ = read_response(&mut reader).unwrap();
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);
    {
        let rel = "localisation/test_l_english.yml";
        let content = yml;
        let uri = path_uri(ws.path().join(rel));
        write_frame(&mut child, &jsonrpc_notification("textDocument/didOpen", serde_json::json!({"textDocument": {"uri": uri, "languageId": "hoi4", "version": 1, "text": content}}))).unwrap();
        wait_for_diagnostics(&mut reader, rel);
    }
    let uri = path_uri(ws.path().join("localisation/test_l_english.yml"));
    let req = jsonrpc_request(
        10,
        "textDocument/rename",
        serde_json::json!({"textDocument": {"uri": uri}, "position": {"line": 1, "character": 2}, "newName": "new_item"}),
    );
    write_frame(&mut child, &req).unwrap();
    let resp_str = read_response(&mut reader).unwrap();
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    child.kill().ok();
    let edit = &resp["result"];
    let new_texts = {
        let mut v = Vec::new();
        if let Some(arr) = edit["documentChanges"].as_array() {
            for doc in arr {
                if let Some(edits) = doc["edits"].as_array() {
                    for e in edits {
                        if let Some(t) = e["newText"].as_str() {
                            v.push(t.to_string());
                        }
                    }
                }
            }
        } else if let Some(map) = edit["changes"].as_object() {
            for (_, edits) in map {
                if let Some(arr) = edits.as_array() {
                    for e in arr {
                        if let Some(t) = e["newText"].as_str() {
                            v.push(t.to_string());
                        }
                    }
                }
            }
        }
        v
    };
    assert!(
        new_texts.contains(&"new_item".to_string()),
        "new_item, got {:?}",
        new_texts
    );
    assert!(
        new_texts.contains(&"new_item_desc".to_string()),
        "sibling _desc must be renamed, got {:?}",
        new_texts
    );
    assert!(
        !new_texts.contains(&"new_item_tooltip".to_string()),
        "non-existent _tooltip must NOT be in edit, got {:?}",
        new_texts
    );
}

#[test]
fn test_loc_rename_across_languages() {
    let yml_en = "l_english:\n my_key:0 \"Hello\"\n";
    let yml_fr = "l_french:\n my_key:0 \"Bonjour\"\n";
    let files = &[
        ("localisation/test_l_english.yml", yml_en),
        ("localisation/test_l_french.yml", yml_fr),
    ];
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();
    for (rel, content) in files {
        let p = ws.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
    }
    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({"processId": std::process::id(), "rootUri": ws_uri, "capabilities": {"workspace": {"workspaceEdit": {"documentChanges": true}}}, "initializationOptions": {"language": "hoi4", "rulesCache": rules_dir.path().to_string_lossy(), "vanilla": vanilla.path().to_string_lossy()}}),
    );
    write_frame(&mut child, &init).unwrap();
    let _ = read_response(&mut reader).unwrap();
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);
    {
        let rel = "localisation/test_l_english.yml";
        let uri = path_uri(ws.path().join(rel));
        write_frame(&mut child, &jsonrpc_notification("textDocument/didOpen", serde_json::json!({"textDocument": {"uri": uri, "languageId": "hoi4", "version": 1, "text": yml_en}}))).unwrap();
        wait_for_diagnostics(&mut reader, rel);
    }
    let uri = path_uri(ws.path().join("localisation/test_l_english.yml"));
    let req = jsonrpc_request(
        10,
        "textDocument/rename",
        serde_json::json!({"textDocument": {"uri": uri}, "position": {"line": 1, "character": 2}, "newName": "new_key"}),
    );
    write_frame(&mut child, &req).unwrap();
    let resp_str = read_response(&mut reader).unwrap();
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    child.kill().ok();
    let edit = &resp["result"];
    assert!(!edit.is_null(), "rename must succeed, got {}", resp_str);
    let mut uris = Vec::new();
    let mut new_texts = Vec::new();
    if let Some(arr) = edit["documentChanges"].as_array() {
        for doc in arr {
            if let Some(uri) = doc["textDocument"]["uri"].as_str() {
                uris.push(uri.to_string());
            }
            if let Some(edits) = doc["edits"].as_array() {
                for e in edits {
                    if let Some(t) = e["newText"].as_str() {
                        new_texts.push(t.to_string());
                    }
                }
            }
        }
    } else if let Some(map) = edit["changes"].as_object() {
        for (uri, edits) in map {
            uris.push(uri.clone());
            if let Some(arr) = edits.as_array() {
                for e in arr {
                    if let Some(t) = e["newText"].as_str() {
                        new_texts.push(t.to_string());
                    }
                }
            }
        }
    }
    assert!(
        uris.iter().any(|u| u.contains("test_l_english.yml")),
        "english file must be edited, got {:?}",
        uris
    );
    assert!(
        uris.iter().any(|u| u.contains("test_l_french.yml")),
        "french file must be edited, got {:?}",
        uris
    );
    assert!(
        new_texts.iter().filter(|t| *t == "new_key").count() >= 2,
        "expected at least 2 new_key edits, got {:?} in {:?}",
        new_texts,
        uris
    );
}

#[test]
fn test_loc_rename_updates_dollar_refs_in_yml() {
    let yml_main = "l_english:\n my_key:0 \"Hello\"\n";
    let yml_ref = "l_english:\n other:0 \"See $my_key$ and $my_key|Y$ end\"\n";
    let files = &[
        ("localisation/main_l_english.yml", yml_main),
        ("localisation/ref_l_english.yml", yml_ref),
    ];
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), GOTO_RULES).unwrap();
    for (rel, content) in files {
        let p = ws.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
    }
    let ws_uri = path_uri(ws.path());
    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({"processId": std::process::id(), "rootUri": ws_uri, "capabilities": {"workspace": {"workspaceEdit": {"documentChanges": true}}}, "initializationOptions": {"language": "hoi4", "rulesCache": rules_dir.path().to_string_lossy(), "vanilla": vanilla.path().to_string_lossy()}}),
    );
    write_frame(&mut child, &init).unwrap();
    let _ = read_response(&mut reader).unwrap();
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    wait_for_scan_done(&mut reader);
    {
        let rel = "localisation/main_l_english.yml";
        let uri = path_uri(ws.path().join(rel));
        write_frame(&mut child, &jsonrpc_notification("textDocument/didOpen", serde_json::json!({"textDocument": {"uri": uri, "languageId": "hoi4", "version": 1, "text": yml_main}}))).unwrap();
        wait_for_diagnostics(&mut reader, rel);
    }
    let uri = path_uri(ws.path().join("localisation/main_l_english.yml"));
    let req = jsonrpc_request(
        10,
        "textDocument/rename",
        serde_json::json!({"textDocument": {"uri": uri}, "position": {"line": 1, "character": 2}, "newName": "new_key"}),
    );
    write_frame(&mut child, &req).unwrap();
    let resp_str = read_response(&mut reader).unwrap();
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    child.kill().ok();
    let edit = &resp["result"];
    assert!(!edit.is_null(), "rename must succeed, got {}", resp_str);
    let mut uris = Vec::new();
    let mut new_texts = Vec::new();
    if let Some(arr) = edit["documentChanges"].as_array() {
        for doc in arr {
            if let Some(uri) = doc["textDocument"]["uri"].as_str() {
                uris.push(uri.to_string());
            }
            if let Some(edits) = doc["edits"].as_array() {
                for e in edits {
                    if let Some(t) = e["newText"].as_str() {
                        new_texts.push(t.to_string());
                    }
                }
            }
        }
    } else if let Some(map) = edit["changes"].as_object() {
        for (uri, edits) in map {
            uris.push(uri.clone());
            if let Some(arr) = edits.as_array() {
                for e in arr {
                    if let Some(t) = e["newText"].as_str() {
                        new_texts.push(t.to_string());
                    }
                }
            }
        }
    }
    assert!(
        uris.iter().any(|u| u.contains("main_l_english.yml")),
        "main file must be edited, got {:?}",
        uris
    );
    assert!(
        uris.iter().any(|u| u.contains("ref_l_english.yml")),
        "ref file must be edited, got {:?}",
        uris
    );
    assert!(
        new_texts.iter().filter(|t| *t == "new_key").count() >= 3,
        "expected at least 3 new_key edits (def + 2 refs), got {:?} in {:?}",
        new_texts,
        uris
    );
}

#[test]
fn test_ignored_file_is_not_validated_on_open_and_after_config_change() {
    // Repro from #323: ignored file is clean while closed and must stay clean
    // when opened, and adding a glob while the file is open must clear its
    // diagnostics instead of leaving stale squiggles.
    let ws = tempfile::tempdir().unwrap();
    let rules_dir = tempfile::tempdir().unwrap();
    std::fs::write(rules_dir.path().join("r.cwt"), COMPLETION_RULES).unwrap();
    // Two decision files with an undefined <focus> reference (CW500) and one
    // engine-baseline file (README.txt) that must also stay clean.
    let decision_text =
        "decision = { id = t; allowed = { has_completed_focus = missing_focus } }\n";
    std::fs::create_dir_all(ws.path().join("common/decisions")).unwrap();
    std::fs::write(ws.path().join("common/decisions/kept.txt"), decision_text).unwrap();
    std::fs::write(
        ws.path().join("common/decisions/ignored.txt"),
        decision_text,
    )
    .unwrap();
    std::fs::write(ws.path().join("common/decisions/later.txt"), decision_text).unwrap();
    std::fs::write(ws.path().join("README.txt"), decision_text).unwrap();

    let mut child = cwtools_server_cmd()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    // ignoreFilePatterns: ignored.txt (user glob) — README.txt is baseline.
    let init = jsonrpc_request(
        1,
        "initialize",
        serde_json::json!({
            "processId": std::process::id(),
            "rootUri": path_uri(ws.path()),
            "capabilities": {},
            "initializationOptions": {
                "language": "hoi4",
                "rulesCache": rules_dir.path().to_string_lossy(),
                "ignoreFilePatterns": ["ignored.txt"]
            }
        }),
    );
    write_frame(&mut child, &init).unwrap();
    let _ = read_response(&mut reader).expect("init response");
    write_frame(
        &mut child,
        &jsonrpc_notification("initialized", serde_json::json!({})),
    )
    .unwrap();
    // Workspace scan must finish before didOpen; wait for kept.txt diagnostics.
    wait_for_diagnostics(&mut reader, "kept.txt");

    // Helper: wait for publishDiagnostics for suffix and return its diagnostics array.
    let mut wait_publish = |suffix: &str| -> serde_json::Value {
        for _ in 0..400 {
            let raw = read_frame(&mut reader).expect("read frame");
            if raw.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw)
                && v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with(suffix))
            {
                return v["params"]["diagnostics"].clone();
            }
        }
        panic!("no publishDiagnostics for {suffix}");
    };

    // Open ignored, README (baseline), and later (currently kept). The two ignored
    // must publish empty, later must publish CW500.
    for rel in [
        "common/decisions/ignored.txt",
        "README.txt",
        "common/decisions/later.txt",
    ] {
        let path = ws.path().join(rel);
        let uri = path_uri(&path);
        let text = std::fs::read_to_string(&path).unwrap();
        write_frame(
            &mut child,
            &jsonrpc_notification(
                "textDocument/didOpen",
                serde_json::json!({"textDocument": {"uri": uri, "languageId": "hoi4", "version": 1, "text": text}}),
            ),
        )
        .unwrap();
    }
    let ignored_diags = wait_publish("ignored.txt");
    assert!(
        ignored_diags.as_array().is_some_and(|a| a.is_empty()),
        "ignored.txt must publish empty diagnostics when opened, got: {ignored_diags}"
    );
    let readme_diags = wait_publish("README.txt");
    assert!(
        readme_diags.as_array().is_some_and(|a| a.is_empty()),
        "README.txt (engine baseline) must publish empty when opened, got: {readme_diags}"
    );
    let later_diags = wait_publish("later.txt");
    assert!(
        later_diags.as_array().is_some_and(|a| !a.is_empty()),
        "later.txt must publish diagnostics before it is ignored, got: {later_diags}"
    );

    // Now add later.txt to the ignore list while it is still open. The open
    // buffer must clear its diagnostics (publish empty) instead of keeping stale
    // squiggles until the tab is closed.
    write_frame(
        &mut child,
        &jsonrpc_notification(
            "workspace/didChangeConfiguration",
            serde_json::json!({"settings": {"ignoreFilePatterns": ["ignored.txt", "later.txt"]}}),
        ),
    )
    .unwrap();
    // The config change triggers revalidate_all_open_docs; wait for later.txt to go empty.
    let mut later_cleared = serde_json::json!([]);
    for _ in 0..400 {
        let raw = read_frame(&mut reader).expect("read frame");
        if raw.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw)
            && v["method"] == "textDocument/publishDiagnostics"
            && v["params"]["uri"]
                .as_str()
                .is_some_and(|u| u.ends_with("later.txt"))
        {
            later_cleared = v["params"]["diagnostics"].clone();
            if later_cleared.as_array().is_some_and(|a| a.is_empty()) {
                break;
            }
        }
    }
    assert!(
        later_cleared.as_array().is_some_and(|a| a.is_empty()),
        "later.txt must clear diagnostics after being added to ignoreFilePatterns while open, got: {later_cleared}"
    );
    child.kill().ok();
}
