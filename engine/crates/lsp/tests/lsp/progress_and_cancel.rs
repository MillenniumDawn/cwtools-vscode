use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::common::{
    EDITOR_RULES, ServerBuilder, TestServer, clear_scan_release, is_scan_started, poll_until,
    wait_for_scan_release,
};

/// A scan's `$/progress` token for the startup stream.
const SCAN_TOKEN: &str = "cwtools/scan";

/// A builder over `EDITOR_RULES` and one focus file.
fn focus_builder() -> ServerBuilder {
    TestServer::builder()
        .rules_file("editor_rules.cwt", EDITOR_RULES)
        .file(
            "common/national_focus/tree.txt",
            "my_focus = {\n    id = my_focus\n}\n",
        )
}

/// The same, with 200 focus files so a phase outlasts a 1ms sample by a wide
/// margin while the debug-build scan stays quick.
fn many_focus_builder() -> ServerBuilder {
    let mut builder = TestServer::builder().rules_file("editor_rules.cwt", EDITOR_RULES);
    for i in 0..200 {
        builder = builder.file(
            &format!("common/national_focus/{i}.txt"),
            format!("focus_{i} = {{\n    id = focus_{i}\n}}\n"),
        );
    }
    builder
}

/// A client that advertises `window.workDoneProgress`.
fn progress_caps() -> Value {
    json!({ "window": { "workDoneProgress": true } })
}

fn is_bar_off(frame: &Value) -> bool {
    frame["method"] == "loadingBar" && frame["params"]["enable"] == Value::Bool(false)
}

/// Read frames for up to `secs`, answering each `window/workDoneProgress/create`
/// (the server blocks its `begin` on that response), and return the first frame
/// `want` accepts.
fn pump(server: &mut TestServer, secs: u64, want: impl Fn(&Value) -> bool) -> Option<Value> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        let Some(frame) = server.try_read_frame(Duration::from_millis(200)) else {
            continue;
        };
        if frame["method"] == "window/workDoneProgress/create" {
            server.reply(&frame);
            continue;
        }
        if want(&frame) {
            return Some(frame);
        }
    }
    None
}

#[test]
fn test_scan_reports_standard_work_done_progress() {
    // A client that advertises window.workDoneProgress must see the standard
    // $/progress stream, not only the custom `loadingBar` notification.
    let mut server = focus_builder().capabilities(progress_caps()).spawn();
    server.initialized();

    let mut created = false;
    let mut kinds: Vec<String> = Vec::new();
    let mut saw_loading_bar = false;
    for _ in 0..2000 {
        let v = server.read_frame();
        match v["method"].as_str() {
            Some("window/workDoneProgress/create") => {
                println!("window/workDoneProgress/create = {}", v["params"]);
                assert_eq!(v["params"]["token"], SCAN_TOKEN);
                created = true;
                // The server waits for this response before it begins.
                server.reply(&v);
            }
            Some("$/progress") => {
                println!("$/progress = {}", v["params"]);
                assert_eq!(v["params"]["token"], SCAN_TOKEN);
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

/// #221: the startup scan carries no client token, so between "Indexing pass"
/// and "Workspace validation complete" it used to log nothing at all while the
/// bar sat on one phase boundary percentage — a slow phase and a wedged one
/// looked identical. Every phase now says how long it took.
#[test]
fn test_startup_scan_logs_each_phase_duration() {
    let mut server = focus_builder().spawn();
    server.initialized();

    let mut phases: Vec<String> = Vec::new();
    server.read_until("the scan to finish", |v| {
        if v["method"] == "window/logMessage"
            && let Some(msg) = v["params"]["message"].as_str()
            && let Some(phase) = msg.strip_prefix("Scan phase finished: ")
        {
            phases.push(phase.to_string());
        }
        is_bar_off(v)
    });

    // Not asserting the phase text itself: it's an i18n string (`progress.*`
    // in `cwtools_i18n`) and a reworded or translated value must not fail this
    // test. Six distinct labels, one per `Phase` variant, is what actually
    // matters — `label ({duration}s)` is the format `command_progress.rs`
    // logs, so splitting on the trailing `" ("` recovers the label without
    // depending on its wording.
    assert_eq!(
        phases.len(),
        6,
        "expected 6 phase-finish log lines, got {phases:?}"
    );
    let distinct_labels: std::collections::HashSet<&str> = phases
        .iter()
        .map(|p| p.rsplit_once(" (").map_or(p.as_str(), |(label, _)| label))
        .collect();
    assert_eq!(
        distinct_labels.len(),
        6,
        "expected 6 distinct phase labels, got {phases:?}"
    );
}

/// #221 itself: the percentage has to move *inside* a phase, not only at the
/// six phase boundaries. The startup scan carries no `workDoneToken`, which is
/// exactly the case that used to get an inert sampler and sit on "Validating
/// workspace… 70%" for the whole of pass 2.
///
/// `CWTOOLS_SAMPLE_INTERVAL_MS=1` so a fixture small enough to keep the test
/// quick still gets sampled several times per phase; the assertion is then on
/// what was reported, not on winning a race with a 200ms timer.
#[test]
fn test_the_bar_percentage_moves_inside_a_phase() {
    let mut server = many_focus_builder()
        .env("CWTOOLS_SAMPLE_INTERVAL_MS", "1")
        .spawn();
    server.initialized();

    // `value` is the phase label (whatever `Phase::label()` currently
    // says); pairing it with the percentage lets the assertion below find
    // each phase's own opening boundary from the stream itself instead of
    // a hardcoded `Phase::span()` mirror that a re-weighting would
    // silently outdate.
    let mut samples: Vec<(String, u64)> = Vec::new();
    server.read_until("the scan to finish", |v| {
        if v["method"] != "loadingBar" {
            return false;
        }
        if is_bar_off(v) {
            return true;
        }
        if let (Some(value), Some(pct)) = (
            v["params"]["value"].as_str(),
            v["params"]["percentage"].as_u64(),
        ) {
            samples.push((value.to_string(), pct));
        }
        false
    });

    assert!(
        !samples.is_empty(),
        "the scan reported no percentages at all"
    );

    // The first percentage reported for a phase label is that phase's opening
    // boundary (`report_phase`/`start_phase`'s initial report); any later
    // percentage under the same label came from the sampler reading the
    // phase's counter, i.e. the bar moving inside the phase.
    let mut boundaries: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    let moved_inside_a_phase =
        samples
            .iter()
            .any(|(value, pct)| match boundaries.entry(value.as_str()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(*pct);
                    false
                }
                std::collections::hash_map::Entry::Occupied(entry) => *entry.get() != *pct,
            });
    assert!(
        moved_inside_a_phase,
        "every percentage matched its phase's opening boundary, so the bar never moved inside a phase: {samples:?}"
    );
}

/// #433: a sampler tick must not retry `window/workDoneProgress/create` on
/// its own once the client has refused it — that's the guard in
/// `send_work_done_progress`'s `(true, false)` arm (`!open_stream` returns
/// before the request goes out). The server *does* retry the create at the
/// next phase boundary by design, on the chance a later answer differs, so
/// this can't assert "at most one create" — that would fail even with the
/// guard in place. Instead it bounds the count by the phase-boundary count:
/// a retry per phase boundary is expected, one per sampler tick is the bug.
///
/// `CWTOOLS_SAMPLE_INTERVAL_MS=1` over the same 200-file fixture as
/// `test_the_bar_percentage_moves_inside_a_phase` guarantees sampler ticks
/// land mid-phase. Every create the server sends is answered with a
/// JSON-RPC error, so `scan_progress_active` never latches true and every
/// tick for the rest of the scan exercises the guarded branch.
#[test]
fn test_sampler_ticks_do_not_retry_a_refused_progress_create() {
    let mut server = many_focus_builder()
        .capabilities(progress_caps())
        .env("CWTOOLS_SAMPLE_INTERVAL_MS", "1")
        .spawn();
    server.initialized();

    // The server blocks the scan on the create round-trip, so every create must
    // be answered as it arrives or the scan (and the test) wedges.
    let mut creates = 0u32;
    for _ in 0..20_000 {
        let v = server.read_frame();
        match v["method"].as_str() {
            Some("window/workDoneProgress/create") => {
                assert_eq!(v["params"]["token"], SCAN_TOKEN);
                creates += 1;
                let refusal = json!({
                    "jsonrpc": "2.0",
                    "id": v["id"],
                    "error": { "code": -32601, "message": "refused" }
                });
                server.send(&refusal.to_string());
            }
            Some("loadingBar") if v["params"]["enable"] == Value::Bool(false) => break,
            _ => {}
        }
    }

    assert!(
        creates > 0,
        "the scan never attempted window/workDoneProgress/create at all"
    );
    // Six phases (Discover, Parse, Vanilla, Localisation, Validate, Publish),
    // one boundary retry each; no vanilla dir is configured here, so no
    // extra "Indexing base game…" emit. The slack above that keeps this from
    // being a mirror of the phase count while still catching a
    // sampler-driven retry, which would blow well past it at a 1ms interval.
    assert!(
        creates <= 8,
        "a sampler tick retried window/workDoneProgress/create instead of leaving it to the \
         next phase boundary: {creates} attempts, expected at most one per phase"
    );
}

/// #221 / #434: a live phase sampler must never reopen a bar that something
/// else already closed. The samplers are detached tasks on a timer, so this
/// has to catch one that outlives its own scan's close, not just prove the
/// startup scan itself ends tidily — a fixture too small to keep any sampler
/// alive past the close would pass whether or not the guard exists.
///
/// `CWTOOLS_PARSE_HOLD_FILE` holds the startup scan just inside Parse — a
/// *counted* phase, unlike Discover, so its sampler actually calls
/// `report_loading_bar_pct` on a 1ms `CWTOOLS_SAMPLE_INTERVAL_MS` — instead of
/// racing to catch it exactly at a phase boundary. While it's held, `cacheVanilla`
/// (no vanilla dir configured, so it does nothing) is sent with no
/// `workDoneToken`: its `ScanGuard::finish` still unconditionally closes
/// whatever bar is open, which is the *held* scan's, not its own. That leaves
/// a live, un-aborted counted-phase sampler next to a bar that just closed —
/// deterministically, not a race — so the guard in `report_loading_bar_pct`
/// is the only thing standing between that tick and a reopened bar. Removing
/// it turns this red (#434).
#[test]
fn test_the_bar_stays_closed_after_the_scan_closes_it() {
    let signals = tempfile::tempdir().unwrap();
    let gate = signals.path().join("parse-hold");
    // Armed before the server starts, so it's the startup scan's own Parse
    // phase that trips it.
    std::fs::write(&gate, "").unwrap();

    let mut server = focus_builder()
        .env("CWTOOLS_SAMPLE_INTERVAL_MS", "1")
        .env("CWTOOLS_PARSE_HOLD_FILE", &gate)
        .spawn();
    server.initialized();

    // Three `enable=true` reports: Discover's own boundary, Parse's boundary,
    // and at least one sampler tick from inside the hold — proof the held
    // phase's ticker is alive and reporting, not just present.
    let mut opens = 0u32;
    let deadline = Instant::now() + Duration::from_secs(30);
    while opens < 3 && Instant::now() < deadline {
        if let Some(v) = server.try_read_frame(Duration::from_millis(200))
            && is_scan_started(&v)
        {
            opens += 1;
        }
    }
    assert!(
        opens >= 3,
        "the held Parse phase never showed a live sampler tick"
    );

    // A second, independent operation that closes the *shared* bar: with no
    // vanilla dir configured `cacheVanilla` indexes nothing, but its guard
    // still closes whatever bar is open regardless of who opened it.
    server.send_request(
        9001,
        "workspace/executeCommand",
        json!({ "command": "cacheVanilla", "arguments": [] }),
    );
    // Waits on the close notification itself rather than the command's
    // response: they go out from the same task in that order, but a
    // notification and a response ride separate channels with no ordering
    // guarantee between them once both are in flight.
    let mut saw_close = false;
    let close_deadline = Instant::now() + Duration::from_secs(10);
    while !saw_close && Instant::now() < close_deadline {
        if let Some(v) = server.try_read_frame(Duration::from_millis(200))
            && is_bar_off(&v)
        {
            saw_close = true;
        }
    }
    assert!(saw_close, "cacheVanilla never closed the shared bar");

    // The startup scan's Parse-phase ticker is still alive and untouched (the
    // hold file still exists) — its very next tick, at most 1ms away, is the
    // one under test.
    let after = server.drain_until_quiet(Duration::from_millis(500), Duration::from_secs(3));
    std::fs::remove_file(&gate).ok();
    server.stop();

    let reopened: Vec<&Value> = after.iter().filter(|v| is_scan_started(v)).collect();
    assert!(
        reopened.is_empty(),
        "a sampler tick reopened the bar after cacheVanilla closed it: {reopened:?}"
    );
}

/// #145: a command that carries a `workDoneToken` gets its progress reported
/// against *that* token — one bar for the operation, driven by the client's own
/// notification — rather than the server opening its `cwtools/scan` stream
/// alongside it.
#[test]
fn test_execute_command_reports_progress_against_the_client_token() {
    let mut server = focus_builder().capabilities(progress_caps()).spawn();
    assert_eq!(
        server.init_response()["result"]["capabilities"]["executeCommandProvider"]["workDoneProgress"],
        json!(true),
        "the client feature-detects command progress on this flag"
    );
    server.initialized();
    // The wait for the startup scan has to *answer*
    // `window/workDoneProgress/create`. This client advertises
    // `window.workDoneProgress`, and the server blocks on that request before
    // it opens its own stream — draining frames without replying wedges the
    // scan and the test with it.
    let mut kinds: Vec<String> = Vec::new();
    let mut percentages: Vec<u64> = Vec::new();
    let mut foreign_during_command: Vec<String> = Vec::new();
    let mut created_for_command = false;
    let mut result: Option<Value> = None;
    let mut startup_bar_closed = false;
    let mut startup_progress_ended = false;
    let mut token = String::new();
    let command_token = "cwtools/command/1";
    for _ in 0..20_000 {
        let v = server.read_frame();
        if v["id"] == json!(100) && v.get("result").is_some() {
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
                assert_eq!(v["params"]["token"], SCAN_TOKEN);
                if !token.is_empty() {
                    created_for_command = true;
                }
                server.reply(&v);
            }
            Some("$/progress") => {
                let seen = v["params"]["token"].as_str().unwrap_or_default();
                let kind = v["params"]["value"]["kind"].as_str().unwrap();
                if seen == SCAN_TOKEN && kind == "end" {
                    startup_progress_ended = true;
                }
                if seen != token {
                    if !token.is_empty() {
                        foreign_during_command.push(seen.to_string());
                    }
                } else {
                    kinds.push(kind.to_string());
                    if let Some(pct) = v["params"]["value"]["percentage"].as_u64() {
                        percentages.push(pct);
                    }
                    if result.is_some() && kinds.last().map(String::as_str) == Some("end") {
                        break;
                    }
                }
            }
            Some("loadingBar") if v["params"]["enable"] == Value::Bool(false) => {
                startup_bar_closed = true;
            }
            _ => {}
        }
        if token.is_empty() && startup_bar_closed && startup_progress_ended {
            server.send_request(
                100,
                "workspace/executeCommand",
                json!({
                    "command": "reindexWorkspace",
                    "arguments": [],
                    "workDoneToken": command_token,
                }),
            );
            token = command_token.to_string();
        }
    }
    server.stop();

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

/// Create the scan gate and send a `reindexWorkspace` carrying a fresh token,
/// request id `id_base + attempt`. The caller keeps `attempt` so the frames of
/// each try stay attributable.
fn send_held_reindex(
    server: &mut TestServer,
    gate: &Path,
    released: &Path,
    id_base: i64,
    attempt: i64,
    token_prefix: &str,
) -> String {
    clear_scan_release(released);
    std::fs::write(gate, b"hold").expect("create scan gate");
    let token = format!("{token_prefix}/{attempt}");
    server.send_request(
        id_base + attempt,
        "workspace/executeCommand",
        json!({
            "command": "reindexWorkspace",
            "arguments": [],
            "workDoneToken": token,
        }),
    );
    token
}

/// #228: two commands in flight at once each keep their own `$/progress`
/// stream. `cacheVanilla` doesn't take the scan flag, so it can begin while a
/// `reindexWorkspace` scan is still running; the scan's later phases have to
/// stay on the re-index token instead of following whichever command began
/// last and then falling back to the server's stream when that one ends.
#[test]
fn test_concurrent_commands_keep_separate_progress_streams() {
    let signals = tempfile::tempdir().unwrap();
    let gate = signals.path().join("scan-hold");
    let ready = signals.path().join("scan-hold-ready");
    let released = signals.path().join("scan-released");

    // The test creates the gate after the startup scan, then releases it
    // only after cacheVanilla has closed its own progress stream. The base game
    // is explicit and empty, so `cacheVanilla` re-indexes nothing instead of
    // whatever real game install auto-discovery finds on the host.
    let mut server = focus_builder()
        .vanilla()
        .capabilities(progress_caps())
        .env("CWTOOLS_SCAN_HOLD_FILE", &gate)
        .env("CWTOOLS_SCAN_HOLD_READY_FILE", &ready)
        .env("CWTOOLS_SCAN_RELEASE_FILE", &released)
        .spawn();
    server.initialized();

    // Every `$/progress` frame as `(token, kind)`, in arrival order — the
    // interleaving is the thing under test.
    let mut events: Vec<(String, String)> = Vec::new();
    let mut reindex_result: Option<Value> = None;
    let mut reindex_closed = false;
    // Same scan-guard race as the tests above: the bar-off notification
    // precedes the flag release, so a command sent on it can lose the CAS.
    // Retry until one actually re-indexes, each attempt on its own ids and
    // tokens so the frames stay attributable.
    let mut attempt = 0i64;
    let mut reindex_token = String::new();
    let mut vanilla_token = String::new();
    for _ in 0..20_000 {
        let v = server.read_frame();
        if v["id"] == json!(100 + attempt) && v.get("result").is_some() {
            if v["result"] == json!("Re-index already in progress.") {
                attempt += 1;
                events.clear();
                vanilla_token.clear();
                reindex_closed = false;
                wait_for_scan_release(&released);
                reindex_token = send_held_reindex(
                    &mut server,
                    &gate,
                    &released,
                    100,
                    attempt,
                    "cwtools/command/228/reindex",
                );
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
            Some("window/workDoneProgress/create") => server.reply(&v),
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
                    server.send_request(
                        200 + attempt,
                        "workspace/executeCommand",
                        json!({
                            "command": "cacheVanilla",
                            "arguments": [],
                            "workDoneToken": vanilla_token,
                        }),
                    );
                }
                if token == vanilla_token && kind == "end" && gate.exists() {
                    std::fs::remove_file(&gate).expect("release scan gate");
                }
                reindex_closed |= token == reindex_token && kind == "end";
                events.push((token, kind));
                if reindex_closed && reindex_result.is_some() {
                    break;
                }
            }
            // The startup scan is done; run the command it was blocking.
            Some("loadingBar")
                if reindex_token.is_empty() && v["params"]["enable"] == Value::Bool(false) =>
            {
                reindex_token = send_held_reindex(
                    &mut server,
                    &gate,
                    &released,
                    100,
                    attempt,
                    "cwtools/command/228/reindex",
                );
            }
            _ => {}
        }
    }
    server.stop();

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
    let signals = tempfile::tempdir().unwrap();
    let gate = signals.path().join("scan-hold");
    let ready = signals.path().join("scan-hold-ready");
    let cancelled = signals.path().join("cancel-ack");
    let released = signals.path().join("scan-released");

    let mut server = focus_builder()
        .capabilities(progress_caps())
        .env("CWTOOLS_SCAN_HOLD_FILE", &gate)
        .env("CWTOOLS_SCAN_HOLD_READY_FILE", &ready)
        .env("CWTOOLS_CANCEL_ACK_FILE", &cancelled)
        .env("CWTOOLS_SCAN_RELEASE_FILE", &released)
        .spawn();
    server.initialized();

    // The whole exchange answers `window/workDoneProgress/create` while waiting
    // out the startup scan.
    let mut saw_cancellable_begin = false;
    let mut progress_end = None;
    let mut result = None;
    // Same scan-guard race as the token test above: the bar-off notification
    // precedes the flag release, so a command sent on it can lose the CAS.
    // Retry until one actually starts a scan to cancel.
    let mut attempt = 0i64;
    let mut token = String::new();
    for _ in 0..20_000 {
        let v = server.read_frame();
        if v["id"] == json!(100 + attempt) && v.get("result").is_some() {
            if v["result"] == json!("Re-index already in progress.") {
                attempt += 1;
                saw_cancellable_begin = false;
                progress_end = None;
                result = None;
                wait_for_scan_release(&released);
                token = send_held_reindex(
                    &mut server,
                    &gate,
                    &released,
                    100,
                    attempt,
                    "cwtools/command/7",
                );
                continue;
            }
            result = Some(v["result"].clone());
            if progress_end.is_some() {
                break;
            }
            continue;
        }
        match v["method"].as_str() {
            Some("window/workDoneProgress/create") => server.reply(&v),
            // The startup scan is done; run the command it was blocking.
            Some("loadingBar")
                if token.is_empty() && v["params"]["enable"] == Value::Bool(false) =>
            {
                token = send_held_reindex(
                    &mut server,
                    &gate,
                    &released,
                    100,
                    attempt,
                    "cwtools/command/7",
                );
            }
            // Cancel only once `begin` has landed: the token isn't in the
            // server's registry until then, and a cancel naming a token it
            // doesn't know is dropped.
            Some("$/progress")
                if v["params"]["token"] == token.as_str()
                    && v["params"]["value"]["kind"] == "begin" =>
            {
                assert!(
                    poll_until(
                        Instant::now() + Duration::from_secs(5),
                        Duration::from_millis(5),
                        || ready.exists().then_some(())
                    )
                    .is_some(),
                    "scan did not enter its file gate before cancellation"
                );
                saw_cancellable_begin = v["params"]["value"]["cancellable"] == Value::Bool(true);
                server.notify("window/workDoneProgress/cancel", json!({ "token": token }));
                assert!(
                    poll_until(
                        Instant::now() + Duration::from_secs(5),
                        Duration::from_millis(5),
                        || cancelled.exists().then_some(())
                    )
                    .is_some(),
                    "server did not acknowledge cancellation while the scan was held"
                );
                std::fs::remove_file(&gate).expect("release cancelled scan");
            }
            Some("$/progress")
                if v["params"]["token"] == token.as_str()
                    && v["params"]["value"]["kind"] == "end" =>
            {
                progress_end = Some(v["params"]["value"].clone());
                if result.is_some() {
                    break;
                }
            }
            _ => {}
        }
    }
    server.stop();

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
    let progress_end = progress_end.expect("cancelled command never closed its progress token");
    assert_eq!(progress_end["message"], "Re-index cancelled.");
}

/// The state #470 describes, reproduced: pass 1 of a command-driven scan owns
/// its thread, and everything else has to keep working anyway.
///
/// `CWTOOLS_SCAN_HOLD_FILE` holds the scan at an `.await` — it yields, so it
/// cannot reproduce this at all. `CWTOOLS_PARSE_BLOCKING_HOLD_FILE` parks the
/// thread from inside pass 1's `block_in_place`, and writes the ready file first
/// so the test acts while the thread is held rather than before it starts.
///
/// The scan has to be command-driven: the startup scan already runs on a task of
/// its own, where `block_in_place` behaves.
struct HeldPass1 {
    markers: tempfile::TempDir,
}

impl HeldPass1 {
    fn new() -> Self {
        Self {
            markers: tempfile::tempdir().unwrap(),
        }
    }

    fn ready_file(&self) -> PathBuf {
        self.markers.path().join("pass1-holding")
    }

    fn hold_file(&self) -> PathBuf {
        self.markers.path().join("pass1-release")
    }

    fn cancel_ack_file(&self) -> PathBuf {
        self.markers.path().join("cancel-ack")
    }

    fn scan_release_file(&self) -> PathBuf {
        self.markers.path().join("scan-released")
    }

    /// A handshaken server whose scans park pass 1 while the gate exists.
    fn start(&self) -> TestServer {
        let mut server = focus_builder()
            .capabilities(progress_caps())
            .env("CWTOOLS_PARSE_BLOCKING_HOLD_FILE", self.hold_file())
            .env("CWTOOLS_PARSE_BLOCKING_HOLD_READY_FILE", self.ready_file())
            .env("CWTOOLS_CANCEL_ACK_FILE", self.cancel_ack_file())
            .env("CWTOOLS_SCAN_RELEASE_FILE", self.scan_release_file())
            .spawn();
        server.initialized();
        server
    }
}

/// How long to give the marker once a scan has opened its bar. The budget is
/// spent not reading, so it stays well short of what would fill the stdout pipe.
const MARKER_BUDGET: Duration = Duration::from_secs(1);

/// How long a probe sent while pass 1 holds its thread may take to come back.
/// A working pump answers in milliseconds; past this the pump is stalled (#470).
const PROBE_BUDGET: Duration = Duration::from_secs(20);

/// Blocks until pass 1 says it has parked its thread. `false` means this bar
/// belonged to some other scan — a startup or deferred one — and the caller
/// should go back to reading and try again on the next one.
fn wait_for_pass1_hold(ready: &Path, budget: Duration) -> bool {
    poll_until(Instant::now() + budget, Duration::from_millis(10), || {
        ready.exists().then_some(())
    })
    .is_some()
}

/// #470: a `reindexWorkspace` whose pass 1 owns the thread used to own the
/// message pump with it, so nothing else was read, written or dispatched until
/// the scan finished. An unrelated request has to come back while the hold is
/// still running.
#[test]
fn test_a_blocked_scan_does_not_stall_the_pump() {
    let fixture = HeldPass1::new();
    let ready = fixture.ready_file();
    let gate = fixture.hold_file();
    let released = fixture.scan_release_file();
    let mut server = fixture.start();

    let mut attempt = 0i64;
    let mut command_sent = false;
    let mut probe_sent_at: Option<Instant> = None;
    let mut answered_while_held = None;
    let send_command = |server: &mut TestServer, attempt: i64| {
        // Clearing the marker is what keeps an earlier scan's hold from
        // being mistaken for this one's — the startup scan parks on the
        // same hold, off the pump, where `block_in_place` behaves and this
        // bug does not exist.
        let _ = std::fs::remove_file(&ready);
        clear_scan_release(&released);
        std::fs::write(&gate, b"hold").expect("create pass-1 gate");
        server.send_request(
            600 + attempt,
            "workspace/executeCommand",
            json!({ "command": "reindexWorkspace", "arguments": [] }),
        );
    };

    for _ in 0..20_000 {
        let v = server.read_frame();
        if v["method"] == "window/workDoneProgress/create" {
            server.reply(&v);
            continue;
        }
        // The probe's answer is the whole measurement. The watchdog lifts the
        // gate once PROBE_BUDGET runs out, so a stalled pump answers late,
        // with the gate gone, instead of hanging until the run deadline.
        if v["id"] == json!(701) && v.get("result").is_some() {
            answered_while_held = Some(
                gate.exists() && probe_sent_at.is_some_and(|sent| sent.elapsed() < PROBE_BUDGET),
            );
            break;
        }
        // Same scan-guard race the cancel tests hit: a command sent on the
        // bar-off notification can still lose the CAS to the scan that just
        // released it. Retry until one actually starts a scan.
        if v["id"] == json!(600 + attempt) && v["result"] == json!("Re-index already in progress.")
        {
            attempt += 1;
            wait_for_scan_release(&released);
            send_command(&mut server, attempt);
            continue;
        }
        if v["method"] != "loadingBar" {
            continue;
        }
        // The startup scan is done; run the command it was blocking.
        if !command_sent && v["params"]["enable"] == Value::Bool(false) {
            command_sent = true;
            send_command(&mut server, attempt);
            continue;
        }
        // The command's own scan has opened its bar, so it won the scan
        // guard and is committed — no "already in progress" reply is coming
        // and it is safe to stop reading and wait for pass 1 to park.
        if command_sent && probe_sent_at.is_none() && v["params"]["enable"] == Value::Bool(true) {
            if !wait_for_pass1_hold(&ready, MARKER_BUDGET) {
                continue;
            }
            probe_sent_at = Some(Instant::now());
            let watchdog_gate = gate.clone();
            std::thread::spawn(move || {
                std::thread::sleep(PROBE_BUDGET);
                let _ = std::fs::remove_file(watchdog_gate);
            });
            server.send_request(
                701,
                "workspace/executeCommand",
                json!({ "command": "exportProfilingLog", "arguments": [] }),
            );
        }
    }
    // Release the hold so the held scan can finish and the server can exit.
    let _ = std::fs::remove_file(&gate);
    server.stop();

    let answered_while_held = answered_while_held.expect("the probe request was never answered");
    assert!(
        answered_while_held,
        "the probe was not answered within {PROBE_BUDGET:?} while pass 1 held its thread (#470)"
    );
}

/// The acceptance case from #470: the cancel arrives while the scan owns its
/// thread — not before it starts, which is all the older cancel tests could
/// arrange — and the scan actually stops.
///
/// "Actually stops" needs an observable, because the reply says
/// "Re-index cancelled." either way: a cancel that is only read *after* pass 1
/// still trips a later checkpoint. Pass 1's own log line is that observable. Its
/// per-file `cancel.is_cancelled()` poll drops every file, and the `return`
/// right after pass 1 comes before the "Indexing pass:" summary — so with the
/// cancel observed in time the scan never reaches that log, and without it the
/// whole workspace is parsed and counted first.
const INDEXING_PASS_LOG: &str = "Indexing pass:";

#[test]
fn test_cancel_reaches_a_scan_that_holds_its_thread() {
    let fixture = HeldPass1::new();
    let ready = fixture.ready_file();
    let gate = fixture.hold_file();
    let cancelled = fixture.cancel_ack_file();
    let released = fixture.scan_release_file();
    let mut server = fixture.start();

    let mut attempt = 0i64;
    let mut token = String::new();
    let mut cancelled_mid_hold = false;
    let mut indexed_after_cancel = false;
    let mut result = None;
    let mut progress_end = None;
    for _ in 0..20_000 {
        let v = server.read_frame();
        if v["id"] == json!(800 + attempt) && v.get("result").is_some() {
            if v["result"] == json!("Re-index already in progress.") {
                attempt += 1;
                cancelled_mid_hold = false;
                indexed_after_cancel = false;
                progress_end = None;
                wait_for_scan_release(&released);
                let _ = std::fs::remove_file(&ready);
                token = send_held_reindex(
                    &mut server,
                    &gate,
                    &released,
                    800,
                    attempt,
                    "cwtools/command/470",
                );
                continue;
            }
            result = Some(v["result"].clone());
            if progress_end.is_some() {
                break;
            }
            continue;
        }
        match v["method"].as_str() {
            Some("window/workDoneProgress/create") => server.reply(&v),
            Some("window/logMessage")
                if cancelled_mid_hold
                    && v["params"]["message"]
                        .as_str()
                        .is_some_and(|m| m.contains(INDEXING_PASS_LOG)) =>
            {
                indexed_after_cancel = true;
            }
            // The startup scan is done; run the command it was blocking.
            Some("loadingBar")
                if token.is_empty() && v["params"]["enable"] == Value::Bool(false) =>
            {
                let _ = std::fs::remove_file(&ready);
                token = send_held_reindex(
                    &mut server,
                    &gate,
                    &released,
                    800,
                    attempt,
                    "cwtools/command/470",
                );
            }
            // Some scan has opened a bar. If it is this command's, pass 1
            // parks within milliseconds and the marker appears; if it is a
            // startup or deferred one, the budget lapses and the next bar
            // gets another try. Either way the cancel only goes out with the
            // thread genuinely held, which is the whole point — `begin` on
            // its own lands before the scan has even started.
            Some("loadingBar")
                if !token.is_empty()
                    && !cancelled_mid_hold
                    && v["params"]["enable"] == Value::Bool(true)
                    && wait_for_pass1_hold(&ready, MARKER_BUDGET) =>
            {
                cancelled_mid_hold = true;
                server.notify("window/workDoneProgress/cancel", json!({ "token": token }));
                assert!(
                    poll_until(
                        Instant::now() + Duration::from_secs(5),
                        Duration::from_millis(5),
                        || cancelled.exists().then_some(())
                    )
                    .is_some(),
                    "server did not acknowledge cancellation while pass 1 was held"
                );
                std::fs::remove_file(&gate).expect("release cancelled pass-1 scan");
            }
            Some("$/progress")
                if v["params"]["token"] == token.as_str()
                    && v["params"]["value"]["kind"] == "end" =>
            {
                progress_end = Some(v["params"]["value"].clone());
                if result.is_some() {
                    break;
                }
            }
            _ => {}
        }
    }
    server.stop();

    assert!(
        cancelled_mid_hold,
        "the cancel has to be sent while pass 1 owns its thread, or this proves nothing"
    );
    assert!(
        !indexed_after_cancel,
        "the scan indexed the workspace after being cancelled, so the cancel was not read \
         until pass 1 had already finished"
    );
    assert_eq!(
        result.expect("cancelled command never answered").as_str(),
        Some("Re-index cancelled."),
        "a cancel delivered mid-scan must stop the scan and say so"
    );
    assert_eq!(
        progress_end.expect("cancelled command never closed its token")["message"],
        "Re-index cancelled."
    );
}

/// #204: the client cancels a long `workspace/executeCommand` while its scan is
/// running. `tower-lsp` answers `$/cancelRequest` by dropping the handler, so
/// `CommandProgress::Drop` has to close the client token while `ScanGuard::Drop`
/// closes the loading bar and releases the scan for the next command.
#[test]
fn test_cancelling_a_scan_command_closes_progress_and_frees_the_scan() {
    let signals = tempfile::tempdir().unwrap();
    let gate = signals.path().join("scan-hold");
    let ready = signals.path().join("scan-hold-ready");

    let mut server = focus_builder()
        .capabilities(progress_caps())
        .env("CWTOOLS_SCAN_HOLD_FILE", &gate)
        .env("CWTOOLS_SCAN_HOLD_READY_FILE", &ready)
        .spawn();
    server.initialized();

    // Wait out the startup scan, which holds too. The server blocks its `begin`
    // on the create response, so every wait here goes through `pump`, which
    // answers it.
    assert!(
        pump(&mut server, 30, is_bar_off).is_some(),
        "the startup scan never closed its loading bar"
    );

    std::fs::write(&gate, b"hold").unwrap();
    let token = "cwtools/command/204/cancel";
    server.send_request(
        900,
        "workspace/executeCommand",
        json!({
            "command": "reindexWorkspace",
            "arguments": [],
            "workDoneToken": token,
        }),
    );
    // Cancel only once the client token opens, so its registry entry exists
    // and the dropped handler has an `end` notification to send.
    assert!(
        pump(&mut server, 30, |v| v["method"] == "$/progress"
            && v["params"]["token"] == token
            && v["params"]["value"]["kind"] == "begin")
        .is_some(),
        "no $/progress begin for the command token"
    );
    assert!(
        poll_until(
            Instant::now() + Duration::from_secs(5),
            Duration::from_millis(5),
            || ready.exists().then_some(())
        )
        .is_some(),
        "scan did not enter its file gate before request cancellation"
    );

    server.notify("$/cancelRequest", json!({ "id": 900 }));

    let mut saw_bar_off = false;
    let mut saw_progress_end = false;
    let mut response = None;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !(saw_bar_off && saw_progress_end) {
        let Some(v) = pump(&mut server, 5, |v| {
            v["id"] == 900
                || is_bar_off(v)
                || (v["method"] == "$/progress"
                    && v["params"]["token"] == token
                    && v["params"]["value"]["kind"] == "end")
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
            assert_eq!(v["params"]["value"]["message"], "Cancelled.");
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
        .or_else(|| pump(&mut server, 10, |v| v["id"] == 900))
        .expect("the cancelled request never answered");
    assert_eq!(
        response["error"]["code"], -32800,
        "a cancelled request answers RequestCancelled: {response}"
    );
    std::fs::remove_file(&gate).expect("release scan gate after cancellation");

    // The guard releases the scan flag only once the close has gone out, so the
    // first retry can still lose the CAS — the point is that one of them wins.
    let mut reindexed = None;
    for id in 901..906 {
        server.send_request(
            id,
            "workspace/executeCommand",
            json!({ "command": "reindexWorkspace", "arguments": [] }),
        );
        let answer = pump(&mut server, 30, |v| v["id"] == id)
            .unwrap_or_else(|| panic!("reindexWorkspace {id} never answered"));
        if answer["result"] == "Workspace re-indexed." {
            reindexed = Some(answer);
            break;
        }
    }
    server.stop();
    assert!(
        reindexed.is_some(),
        "no scan could start after the cancelled one released the guard"
    );
}
