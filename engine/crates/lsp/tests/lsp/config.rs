use std::collections::HashSet;
use std::time::Duration;

use serde_json::{Value, json};

use crate::common::{
    COMPLETION_RULES, GOTO_RULES, STORM_FILE, ServerBuilder, TestServer, bom, count_validate_log,
    publish_has_code, storm_builder, storm_server, storm_start,
};

const CFG_REL: &str = "common/decisions/cfg.txt";

fn change_config(server: &mut TestServer, settings: Value) {
    server.notify(
        "workspace/didChangeConfiguration",
        json!({ "settings": settings }),
    );
}

/// A profiling server with `cfg.txt` open and its didOpen publish consumed, so
/// a config observation window starts clean.
fn config_server() -> TestServer {
    let mut server = storm_server();
    server.write_file(CFG_REL, STORM_FILE);
    server.open(CFG_REL, STORM_FILE);
    server.wait_for_diagnostics("cfg.txt");
    server
}

#[test]
fn test_config_no_op_skips_revalidate_then_real_change_runs() {
    // Identical didChangeConfiguration payloads must not trigger a revalidate on
    // the second send; a genuinely changed ignoredErrorCodes must trigger
    // exactly one `(configChange)` pass over the single open document.
    //
    // One open game file means `revalidate_all_open_docs` has exactly one doc to
    // validate — one `(configChange)` log per real pass.
    let mut server = config_server();
    let quiet = Duration::from_millis(700);
    let budget = Duration::from_secs(5);

    // Counts are cumulative over the non-draining profiling buffer, so each
    // window asserts the running total of `(configChange)` validations.

    // First send changes the (empty) live codes → one configChange pass.
    change_config(&mut server, json!({ "ignoredErrorCodes": ["CW999"] }));
    server.drain_after_first(quiet, budget);
    let log1 = server.profiling_log();
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
    change_config(&mut server, json!({ "ignoredErrorCodes": ["CW999"] }));
    server.drain_until_quiet(quiet, budget);
    let log2 = server.profiling_log();
    assert_eq!(
        count_validate_log(&log2, "configChange"),
        1,
        "an identical config re-send must not revalidate"
    );

    // A real change → one more configChange pass (total 2).
    change_config(&mut server, json!({ "ignoredErrorCodes": ["CW998"] }));
    server.drain_after_first(quiet, budget);
    let log3 = server.profiling_log();
    server.stop();
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
    let mut server = config_server();
    let quiet = Duration::from_millis(700);
    let budget = Duration::from_secs(5);

    // Cumulative `(configChange)` counts over the non-draining profiling buffer.

    // 5 differs from the 15s default → one configChange pass.
    change_config(&mut server, json!({ "backgroundReindexIdleSeconds": 5 }));
    server.drain_after_first(quiet, budget);
    let log1 = server.profiling_log();
    assert_eq!(
        count_validate_log(&log1, "configChange"),
        1,
        "an idle-only config change must not be swallowed by the no-op guard"
    );

    // Identical payload → no-op guard skips the revalidate (total stays 1).
    change_config(&mut server, json!({ "backgroundReindexIdleSeconds": 5 }));
    server.drain_until_quiet(quiet, budget);
    let log2 = server.profiling_log();
    server.stop();
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
    let mut server = config_server();
    let quiet = Duration::from_millis(700);
    let budget = Duration::from_secs(5);
    let baseline_log = server.profiling_log();
    let baseline_scans = baseline_log.matches("workspace_scan_start").count();

    // Establish non-empty ignore lists. Discovery changed, so this needs a full
    // scan to rebuild both the script and localisation indexes.
    change_config(
        &mut server,
        json!({
            "ignoredErrorCodes": ["CW999"],
            "ignoreFilePatterns": ["**/skip.txt"],
        }),
    );
    server.drain_after_first(quiet, budget);
    let log1 = server.profiling_log();
    assert_eq!(
        log1.matches("workspace_scan_start").count(),
        baseline_scans + 1,
        "setting the ignore lists should run one full scan"
    );

    // Idle-only partial payload → revalidates (idle changed) but must keep
    // the ignore lists.
    change_config(&mut server, json!({ "backgroundReindexIdleSeconds": 5 }));
    server.drain_after_first(quiet, budget);
    let log2 = server.profiling_log();
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
    change_config(
        &mut server,
        json!({
            "ignoredErrorCodes": ["CW999"],
            "ignoreFilePatterns": ["**/skip.txt"],
            "backgroundReindexIdleSeconds": 5,
        }),
    );
    server.drain_until_quiet(quiet, budget);
    let log3 = server.profiling_log();
    server.stop();
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
    let loc_rel = "localisation/ignored_l_english.yml";
    let mut server = storm_start(
        storm_builder()
            .file("common/decisions/scan.txt", STORM_FILE)
            .file(loc_rel, bom("l_english:\n broken:0 \"unterminated\n")),
    );
    let ignored_uri = server.uri(loc_rel);
    server.reindex_until_scan_starts();
    server.read_until_within(
        Duration::from_secs(10),
        "the localisation diagnostic before it is ignored",
        |v| {
            v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"] == ignored_uri
                && publish_has_code(v, "CW268")
        },
    );

    change_config(
        &mut server,
        json!({ "ignoreFilePatterns": ["**/ignored_l_english.yml"] }),
    );
    server.read_until_within(
        Duration::from_secs(10),
        "an empty publish clearing the ignored localisation diagnostic",
        |v| {
            v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"] == ignored_uri
                && v["params"]["diagnostics"]
                    .as_array()
                    .is_some_and(Vec::is_empty)
        },
    );
    server.stop();
}

#[test]
fn test_init_ignore_pattern_caps_cut_hostile_payload() {
    // #169: initializationOptions carrying a 1 MB glob and over-count lists
    // must be cut at the boundary. The startup summary (window/logMessage)
    // reports the capped counts: the 1 MB glob is dropped, the dir and code
    // lists truncated. A reverted cap would report 300 dirs and 250 codes
    // and feed the 1 MB pattern to the matcher for every walked entry.
    let huge = "?".repeat(1024 * 1024);
    let over_count_dirs: Vec<String> = (0..300).map(|i| format!("dir{i}")).collect();
    let over_count_codes: Vec<String> = (0..250).map(|i| format!("CW{i}")).collect();
    let mut server = TestServer::builder()
        .rules_file("r.cwt", GOTO_RULES)
        .vanilla()
        .file("common/decisions/a.txt", STORM_FILE)
        .file("common/decisions/b.txt", STORM_FILE)
        .file("events/e.txt", STORM_FILE)
        .init_opts(json!({
            "ignoreFilePatterns": ["**/skip.txt", huge],
            "ignoreDirectories": over_count_dirs,
            "ignoredErrorCodes": over_count_codes,
        }))
        .spawn_uninitialized();
    server.send_initialize();

    // The summary is logged during initialize, before the response. Reading
    // the response first would skip it, so scan frames in order.
    let log = server.read_until("the startup summary logMessage", |v| {
        v["method"] == "window/logMessage"
            && v["params"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("ignore patterns:"))
    });
    let summary = log["params"]["message"].as_str().unwrap();
    assert!(
        summary.contains("1 files, 200 dirs, 200 suppressed codes"),
        "caps must truncate the hostile payload; got: {summary}"
    );

    // The startup scan must still complete with the capped patterns in place.
    server.initialized();
    server.wait_for_scan_done();
    server.stop();
}

/// A rules directory holding only `broken.cwt`, started and told `initialized`.
fn broken_rules_server(broken: &str) -> TestServer {
    let mut server = TestServer::builder()
        .rules_file("broken.cwt", broken)
        .spawn();
    server.initialized();
    server
}

fn is_broken_cwt_publish(v: &Value) -> bool {
    v["method"] == "textDocument/publishDiagnostics"
        && v["params"]["uri"]
            .as_str()
            .is_some_and(|u| u.ends_with("broken.cwt"))
}

/// The rules load runs inside `initialize`, where tower-lsp drops notifications,
/// so the diagnostics behind the "N rules-config error(s)" popup went nowhere.
#[test]
fn test_rules_config_errors_publish_after_initialized() {
    let mut server = broken_rules_server(
        "types = {\n  type[foo] = { path = \"common/foo\" }\n}\nr = {\n  a = <undefined_thing>\n  b = <also_missing>\n}\n",
    );
    server.read_until("diagnostics for the broken .cwt file", |v| {
        is_broken_cwt_publish(v)
            && v["params"]["diagnostics"]
                .as_array()
                .is_some_and(|d| !d.is_empty())
    });
}

/// A `.cwt` that parsed badly and then parses cleanly has to have its squiggle
/// taken back. Only files *with* errors are published, so without an explicit
/// clear the stale diagnostic sits in the Problems panel forever.
#[test]
fn test_rules_config_diagnostics_clear_after_a_clean_reload() {
    let mut server = broken_rules_server(
        "types = {\n  type[foo] = { path = \"common/foo\" }\n}\nr = {\n  a = <undefined_thing>\n}\n",
    );

    // Repair the rules on disk, then ask the server to re-read them.
    std::fs::write(
        server.rules_dir().join("broken.cwt"),
        "types = {\n  type[foo] = { path = \"common/foo\" }\n}\n",
    )
    .unwrap();
    server.send_request(
        2,
        "workspace/executeCommand",
        json!({"command": "reloadrulesconfig", "arguments": []}),
    );

    server.read_until("the recovered .cwt's diagnostics being cleared", |v| {
        is_broken_cwt_publish(v)
            && v["params"]["diagnostics"]
                .as_array()
                .is_some_and(|d| d.is_empty())
    });
}

/// Toasts seen until the response to request `id` arrives.
fn toasts_until_response(server: &mut TestServer, id: i64) -> usize {
    let mut toasts = 0;
    server.read_until(&format!("the response to request {id}"), |v| {
        if v["method"] == "window/showMessage" {
            toasts += 1;
        }
        v["id"] == id && v.get("method").is_none()
    });
    toasts
}

/// #98: the rules-error toast fired inside `initialize`, where tower-lsp drops
/// notifications, and again from the boot-time `reloadrulesconfig` the client
/// sends after its rules clone. It must arrive exactly once, after the
/// handshake; an unchanged error set must not toast again; a changed one must —
/// even one that keeps the same count and the same first error, which the
/// displayed summary alone cannot distinguish.
#[test]
fn test_rules_config_toast_defers_to_initialized_and_dedupes() {
    let mut server = broken_rules_server(
        "types = {\n  type[foo] = { path = \"common/foo\" }\n}\nr = {\n  a = <undefined_thing>\n  b = <also_missing>\n}\n",
    );

    // Phase 1: the deferred boot toast arrives with no further prompting.
    server.read_until("the deferred boot toast", |v| {
        v["method"] == "window/showMessage"
    });

    // Phase 2: reloading the same broken rules must not toast again.
    server.send_request(
        2,
        "workspace/executeCommand",
        json!({"command": "reloadrulesconfig", "arguments": []}),
    );
    let same_reload = toasts_until_response(&mut server, 2);

    // Phase 3: a different error set toasts again — here one that keeps
    // the count (2) and the first error, so a summary-keyed dedupe would
    // wrongly swallow it.
    std::fs::write(
        server.rules_dir().join("broken.cwt"),
        "types = {\n  type[foo] = { path = \"common/foo\" }\n}\nr = {\n  a = <undefined_thing>\n  b = <other_thing>\n}\n",
    )
    .unwrap();
    server.send_request(
        3,
        "workspace/executeCommand",
        json!({"command": "reloadrulesconfig", "arguments": []}),
    );
    let changed_reload = toasts_until_response(&mut server, 3);

    assert_eq!(
        same_reload, 0,
        "reloading an unchanged error set must not toast again"
    );
    assert_eq!(
        changed_reload, 1,
        "a changed error set must toast exactly once"
    );
}

/// A storm server whose competing scan holds while `gate` exists.
fn held_scan_server(builder: ServerBuilder, gate: &std::path::Path) -> TestServer {
    storm_start(builder.env("CWTOOLS_SCAN_HOLD_FILE", gate))
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
    let signals = tempfile::tempdir().unwrap();
    let gate = signals.path().join("scan-hold");
    let ready = signals.path().join("scan-hold-ready");

    // Hold the explicit competing scan until the reload has proved it remains
    // pending, then release it and observe the queued retry.
    let mut server = held_scan_server(
        storm_builder().env("CWTOOLS_SCAN_HOLD_READY_FILE", &ready),
        &gate,
    );

    // Start a competing scan and wait for the scan-started signal, so the hold
    // is definitely active when the reload arrives.
    std::fs::write(&gate, b"hold").unwrap();
    server.reindex_until_scan_starts();

    // Fire the reload while the scan holds the CAS: it must not answer until
    // the competing scan is gone and a revalidation has run.
    server.send_request(
        901,
        "workspace/executeCommand",
        json!({ "command": "reloadrulesconfig", "arguments": [] }),
    );
    server.assert_no_response_within(
        901,
        Duration::from_secs(2),
        "reloadrulesconfig must not answer while the competing scan holds the guard",
    );
    assert!(ready.exists(), "competing scan did not enter its file gate");
    std::fs::remove_file(&gate).unwrap();

    // The competing scan releases after its hold, the retry wins the CAS, and
    // one full revalidation (a second loadingBar on→off cycle) must complete
    // before the success response.
    let (response, saw_revalidation) = server.wait_for_response_watching_scans(
        901,
        Duration::from_secs(30),
        "reloadrulesconfig to answer once it won the guard",
    );
    server.stop();

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
    let signals = tempfile::tempdir().unwrap();
    let gate = signals.path().join("scan-hold");
    let ready = signals.path().join("scan-hold-ready");

    // Keep the competing scan held while the reload reaches its original 1s
    // retry deadline and reports the queued state.
    let mut server = held_scan_server(
        storm_builder()
            .env("CWTOOLS_SCAN_HOLD_READY_FILE", &ready)
            .env("CWTOOLS_RETRY_DEADLINE_MS", "1000"),
        &gate,
    );

    // Start a competing scan and wait for the scan-started signal, so the hold
    // is definitely active for the reload's whole deadline.
    std::fs::write(&gate, b"hold").unwrap();
    server.reindex_until_scan_starts();

    // Fire the reload. Its 1s deadline expires while the gate remains closed,
    // so the answer must arrive promptly, report the pending state,
    // and no revalidation scan may have run.
    server.send_request(
        901,
        "workspace/executeCommand",
        json!({ "command": "reloadrulesconfig", "arguments": [] }),
    );
    // The file gate remains closed, so any answer inside this window must be
    // the reload's own give-up response.
    let (response, saw_revalidation) = server.wait_for_response_watching_scans(
        901,
        Duration::from_secs(5),
        "reloadrulesconfig to give up on its 1s deadline",
    );
    assert!(ready.exists(), "competing scan did not enter its file gate");
    server.stop();

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
    let signals = tempfile::tempdir().unwrap();

    // This test cares about both ends of the hold: the reload's 1s deadline has
    // to expire inside it, and the deferred retry has to run once it releases.
    // A fixed hold makes both a bet on wall-clock that parallel load can lose,
    // so the competing scan holds while `gate` exists and this test moves the
    // gate itself (#198). The file lives outside the workspace so creating it
    // isn't a watched-file change.
    let gate = signals.path().join("scan-hold");
    let mut server = held_scan_server(
        storm_builder().env("CWTOOLS_RETRY_DEADLINE_MS", "1000"),
        &gate,
    );

    // Arm the hold (the startup scan is already done, so it isn't caught by
    // it), then start the competing scan that trips it.
    std::fs::write(&gate, "").unwrap();
    server.reindex_until_scan_starts();

    // The give-up response arrives ~1s in, with the scan still held.
    server.send_request(
        901,
        "workspace/executeCommand",
        json!({ "command": "reloadrulesconfig", "arguments": [] }),
    );
    let (response, _) = server.wait_for_response_watching_scans(
        901,
        Duration::from_secs(30),
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
    server.wait_for_scan_started(
        Duration::from_secs(30),
        "the deferred revalidation to run after the reload gave up",
    );
    server.stop();
}

#[test]
fn test_ignored_file_close_keeps_server_responsive() {
    let user_rel = "common/decisions/user_ignored.txt";
    let baseline_rel = "notes.md";
    let mut server = storm_start(
        storm_builder()
            .file(user_rel, STORM_FILE)
            .file(baseline_rel, STORM_FILE)
            .init_opts(json!({ "ignoreFilePatterns": ["user_ignored.txt"] }))
            .deadline(Duration::from_secs(10)),
    );
    let ignored = [(user_rel, "user_ignored.txt"), (baseline_rel, "notes.md")];
    for (rel, suffix) in ignored {
        server.open(rel, STORM_FILE);
        server.wait_for_diagnostics(suffix);
    }

    for (rel, _) in ignored {
        server.close(rel);
    }
    let mut closed = HashSet::new();
    server.read_until("an empty publish for each closed file", |v| {
        if v["method"] == "textDocument/publishDiagnostics"
            && v["params"]["diagnostics"]
                .as_array()
                .is_some_and(Vec::is_empty)
        {
            let uri = v["params"]["uri"].as_str().unwrap_or_default();
            for (_, suffix) in ignored {
                if uri.ends_with(suffix) {
                    closed.insert(suffix);
                }
            }
        }
        closed.len() == ignored.len()
    });

    let hover = server.request(
        "textDocument/hover",
        json!({
            "textDocument": {"uri": server.uri(user_rel)},
            "position": {"line": 0, "character": 0}
        }),
    );

    assert!(hover.is_null(), "unexpected hover response: {hover}");
}

#[test]
fn test_ignored_file_is_not_validated_on_open_and_after_config_change() {
    // Repro from #323: ignored file is clean while closed and must stay clean
    // when opened, and adding a glob while the file is open must clear its
    // diagnostics instead of leaving stale squiggles.
    //
    // Two decision files with an undefined <focus> reference (CW500) and one
    // engine-baseline file (README.txt) that must also stay clean.
    let decision_text =
        "decision = { id = t; allowed = { has_completed_focus = missing_focus } }\n";
    // ignoreFilePatterns: ignored.txt (user glob) — README.txt is baseline.
    let mut server = TestServer::builder()
        .rules_file("r.cwt", COMPLETION_RULES)
        .file("common/decisions/kept.txt", decision_text)
        .file("common/decisions/ignored.txt", decision_text)
        .file("common/decisions/later.txt", decision_text)
        .file("README.txt", decision_text)
        .init_opts(json!({ "ignoreFilePatterns": ["ignored.txt"] }))
        .spawn();
    server.initialized();
    // Workspace scan must finish before didOpen; wait for kept.txt diagnostics.
    server.wait_for_diagnostics("kept.txt");

    // Open ignored, README (baseline), and later (currently kept). The two ignored
    // must publish empty, later must publish CW500.
    for rel in [
        "common/decisions/ignored.txt",
        "README.txt",
        "common/decisions/later.txt",
    ] {
        server.open(rel, decision_text);
    }
    let diagnostics = |server: &mut TestServer, suffix: &str| {
        server.publish_for(suffix, 1)["params"]["diagnostics"].clone()
    };
    let ignored_diags = diagnostics(&mut server, "ignored.txt");
    assert!(
        ignored_diags.as_array().is_some_and(|a| a.is_empty()),
        "ignored.txt must publish empty diagnostics when opened, got: {ignored_diags}"
    );
    let readme_diags = diagnostics(&mut server, "README.txt");
    assert!(
        readme_diags.as_array().is_some_and(|a| a.is_empty()),
        "README.txt (engine baseline) must publish empty when opened, got: {readme_diags}"
    );
    let later_diags = diagnostics(&mut server, "later.txt");
    assert!(
        later_diags.as_array().is_some_and(|a| !a.is_empty()),
        "later.txt must publish diagnostics before it is ignored, got: {later_diags}"
    );

    // Now add later.txt to the ignore list while it is still open. The open
    // buffer must clear its diagnostics (publish empty) instead of keeping stale
    // squiggles until the tab is closed.
    change_config(
        &mut server,
        json!({"ignoreFilePatterns": ["ignored.txt", "later.txt"]}),
    );
    // The config change triggers revalidate_all_open_docs; wait for later.txt to
    // go empty.
    server.read_until(
        "later.txt to clear its diagnostics after being added to ignoreFilePatterns while open",
        |v| {
            v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with("later.txt"))
                && v["params"]["diagnostics"]
                    .as_array()
                    .is_some_and(|a| a.is_empty())
        },
    );
    server.stop();
}
