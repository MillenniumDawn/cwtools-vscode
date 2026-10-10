//! Completion at Millennium Dawn scale, through the real server.
//!
//! Spawns `cwtools-server` against a Millennium-Dawn checkout and the hoi4
//! rules, waits for the workspace scan, then fires `textDocument/completion`
//! twice (cold, then warm) at four representative cursor positions. For each
//! it prints the server's own `cwtools_completion` summary line next to the
//! size of the response. That is a table, not a timed closure, so this is a
//! plain `main` rather than a criterion group.
//!
//!   CWTOOLS_CORPUS=/path/to/Millennium-Dawn \
//!   CWTOOLS_RULES=/path/to/cwtools-hoi4-config/Config \
//!     cargo bench -p cwtools_lsp --bench completion_md
//!
//! Both are also found under `CWTOOLS_PROJECTS`, or as siblings of this repo.
//! When either is missing it prints why and measures nothing. No base game is
//! configured, so references into it do not resolve. A full run takes a few
//! minutes, most of it the workspace scan.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tower_lsp::lsp_types::Url;

type Reader = BufReader<ChildStdout>;

const NAME: &str = "completion_md";

/// MD's scan publishes one diagnostics notification per file (7000+) before
/// the closing `loadingBar`, so this bounds wall-clock time, not frames read.
const SCAN_TIMEOUT: Duration = Duration::from_secs(600);

/// (path under the mod, 0-based line, 0-based character, label)
const POSITIONS: [(&str, u32, u32, &str); 4] = [
    // A sibling-key context inside a small focus, resolved by `completions_from_rules`.
    (
        "common/national_focus/eritrea_puppet.txt",
        22,
        2,
        "block-key (focus)",
    ),
    // Mid-value on a `<state>` reference, resolved by `value_completions` against
    // every state instance.
    (
        "common/national_focus/05_botswana.txt",
        1032,
        21,
        "state-ref (value)",
    ),
    // The root key of a file under a path no type covers, so `root_type_snippets` is
    // empty and it falls through to the flat variable/event-target fallback.
    (
        "common/technology_tags/00_technology.txt",
        3,
        0,
        "flat-fallback",
    ),
    // An effect-alias key inside `completion_reward`. At this scale the list is
    // dominated by `<scripted_effect>` instances, which never carry docs, so `bytes`
    // does not move with the doc deferral.
    (
        "common/national_focus/03_benelux_shared.txt",
        23,
        2,
        "effect-alias (key)",
    ),
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--list") {
        println!("{NAME}: bench");
        return;
    }
    // `cargo test --benches` runs this binary without `--bench`; do nothing then.
    let selected = args
        .iter()
        .filter(|arg| !arg.starts_with('-'))
        .all(|filter| NAME.contains(filter.as_str()));
    if args.iter().any(|arg| arg == "--bench") && selected {
        run();
    }
}

fn run() {
    let (Some(mod_path), Some(rules_dir)) = (
        checkout("CWTOOLS_CORPUS", "Millennium-Dawn"),
        checkout("CWTOOLS_RULES", "cwtools-hoi4-config/Config"),
    ) else {
        eprintln!(
            "{NAME}: no corpus. Set CWTOOLS_CORPUS to a Millennium-Dawn checkout and \
             CWTOOLS_RULES to a cwtools-hoi4-config/Config checkout (or CWTOOLS_PROJECTS \
             to the folder holding both)"
        );
        return;
    };

    let cache_dir = tempfile::tempdir().expect("cache dir");
    // Pinned so the server finds no base game and never writes the real cache.
    let home = tempfile::tempdir().expect("scratch home");
    let mut child = Command::new(env!("CARGO_BIN_EXE_cwtools-server"))
        .env("RUST_LOG", "cwtools_completion=info")
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("LOCALAPPDATA", home.path().join("localappdata"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn cwtools-server");
    let mut reader = BufReader::new(child.stdout.take().expect("server stdout"));

    // Drained on a thread so a full run's tracing output cannot fill the pipe and
    // stall the server; the lines are parsed for summaries once the server exits.
    let stderr = child.stderr.take().expect("server stderr");
    let stderr_thread = std::thread::spawn(move || {
        BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
            .collect::<Vec<String>>()
    });

    send(
        &mut child,
        &request(
            1,
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": file_uri(&mod_path),
                "capabilities": {},
                "initializationOptions": {
                    "language": "hoi4",
                    "rulesCache": rules_dir.to_string_lossy(),
                    "cacheDir": cache_dir.path().to_string_lossy(),
                },
            }),
        ),
    );
    read_response(&mut reader).expect("no init response");
    send(&mut child, &notification("initialized", json!({})));

    eprintln!("{NAME}: waiting for workspace scan to finish...");
    wait_for_scan_done(&mut reader, SCAN_TIMEOUT);
    eprintln!("{NAME}: workspace scan done, firing completions");

    let mut next_id = 10i64;
    // Serialized response size per request, in request order (1:1 with the
    // `cwtools_completion` summaries collected below).
    let mut response_bytes: Vec<usize> = Vec::new();
    let mut fired: Vec<&str> = Vec::new();
    for (rel_path, line, character, label) in POSITIONS {
        let file_path = mod_path.join(rel_path);
        let Ok(text) = std::fs::read_to_string(&file_path) else {
            eprintln!(
                "{NAME}: skipping missing sample file {}",
                file_path.display()
            );
            continue;
        };
        let doc_uri = file_uri(&file_path);
        send(
            &mut child,
            &notification(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": doc_uri,
                        "languageId": "hoi4",
                        "version": 1,
                        "text": text,
                    }
                }),
            ),
        );
        wait_for_diagnostics(&mut reader, rel_path);
        fired.push(label);

        for pass in ["cold", "warm"] {
            let id = next_id;
            next_id += 1;
            send(
                &mut child,
                &request(
                    id,
                    "textDocument/completion",
                    json!({
                        "textDocument": { "uri": doc_uri },
                        "position": { "line": line, "character": character },
                    }),
                ),
            );
            let response = read_response(&mut reader).expect("no completion response");
            response_bytes.push(response.len());
            let parsed: Value = serde_json::from_str(&response).expect("completion response");
            assert_eq!(parsed["id"], id, "{label} {pass} got: {response}");
        }
    }

    send(
        &mut child,
        &json!({ "jsonrpc": "2.0", "id": 999, "method": "shutdown" }),
    );
    read_response(&mut reader).ok();
    send(&mut child, &notification("exit", json!({})));
    drop(child.stdin.take());
    child.wait().ok();
    let lines = stderr_thread.join().expect("stderr thread");

    let summaries: Vec<_> = lines.iter().filter_map(|l| parse_summary(l)).collect();
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
    let passes = ["cold", "warm"];
    // Each position fires two completion requests; the summaries appear in request
    // order, so pair them off positionally (labels x cold/warm).
    for (i, summary) in summaries.iter().enumerate() {
        let label = fired.get(i / 2).copied().unwrap_or("?");
        let pass = passes.get(i % 2).copied().unwrap_or("?");
        let bytes = response_bytes
            .get(i)
            .map_or_else(|| "?".to_string(), ToString::to_string);
        let field = |key: &str| summary.get(key).map_or("?", String::as_str);
        println!(
            "{:<22} {:<6} {:>10} {:>10} {:>7} {:>9} {:<10} {:<10} {:<10}",
            label,
            pass,
            field("total_us"),
            field("build_us"),
            field("items"),
            bytes,
            field("path"),
            field("strategy"),
            field("incomplete"),
        );
    }
    assert!(
        !summaries.is_empty(),
        "expected at least one cwtools_completion summary line in stderr"
    );
}

/// A checkout named by `var`, else found under `CWTOOLS_PROJECTS`, else beside
/// this repo.
fn checkout(var: &str, name: &str) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var(var) {
        return Some(PathBuf::from(dir));
    }
    let projects = match std::env::var("CWTOOLS_PROJECTS") {
        Ok(dir) => PathBuf::from(dir),
        // crates/lsp -> repo root is ../../.., siblings sit next to it.
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .ok()?,
    };
    let dir = projects.join(name);
    dir.is_dir().then_some(dir)
}

fn file_uri(path: &Path) -> String {
    Url::from_file_path(path)
        .expect("the checkout path must be absolute")
        .to_string()
}

fn request(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

fn send(child: &mut Child, message: &Value) {
    let body = message.to_string();
    let stdin = child.stdin.as_mut().expect("server stdin");
    write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("write frame");
    stdin.flush().expect("flush frame");
}

/// One framed message body; empty when the frame carries no body.
fn read_frame(reader: &mut Reader) -> std::io::Result<String> {
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
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    String::from_utf8(body).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// The next frame that answers a request, skipping notifications.
fn read_response(reader: &mut Reader) -> std::io::Result<String> {
    loop {
        let raw = read_frame(reader)?;
        match serde_json::from_str::<Value>(&raw) {
            Ok(val) if val.get("id").is_none() => {}
            _ => return Ok(raw),
        }
    }
}

/// Drain frames until the `publishDiagnostics` for `rel_path`. `didOpen`
/// publishes only after the file's index write lands, so this is the readiness
/// signal that a following completion will see its exports.
fn wait_for_diagnostics(reader: &mut Reader, rel_path: &str) {
    for _ in 0..400 {
        let Ok(raw) = read_frame(reader) else {
            return;
        };
        if let Ok(v) = serde_json::from_str::<Value>(&raw)
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

fn wait_for_scan_done(reader: &mut Reader, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let raw = read_frame(reader).expect("server closed stdout before the scan finished");
        if let Ok(v) = serde_json::from_str::<Value>(&raw)
            && v["method"] == "loadingBar"
            && v["params"]["enable"] == Value::Bool(false)
        {
            return;
        }
    }
    panic!("workspace scan did not finish within {timeout:?}");
}

/// Strip ANSI SGR escapes (`\x1b[...m`). A piped stderr still gets color from the
/// fmt subscriber, so the summary arrives as e.g. `\x1b[3mtotal_us\x1b[2m=\x1b[0m161`.
fn strip_ansi(s: &str) -> String {
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

/// Parse one `cwtools_completion` summary line (`log_completion_summary` in
/// `completion/mod.rs`) into its `key=value` fields. Only whitespace-separated
/// `k=v` tokens are taken, so whatever the subscriber puts before them is
/// ignored; returns `None` for a line that is not a summary.
fn parse_summary(line: &str) -> Option<HashMap<String, String>> {
    let line = strip_ansi(line);
    let mut map = HashMap::new();
    for tok in line.split_whitespace() {
        if let Some((k, v)) = tok.split_once('=') {
            map.insert(k.to_string(), v.trim_matches('"').to_string());
        }
    }
    map.contains_key("total_us").then_some(map)
}
