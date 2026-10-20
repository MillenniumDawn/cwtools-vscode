use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::common::{
    STORM_FILE, TestServer, count_publishes, count_validate_log, path_uri, poll_until,
    publish_has_code, storm_base, storm_builder, storm_server, storm_start, wait_for_scan_release,
    watched_changes, watched_created,
};

#[test]
fn test_watched_repeated_change_coalesces_to_one_validate() {
    // Ten CHANGED events for the same non-open file, each in its own
    // notification, must collapse into exactly one `(watched)` validation and
    // one publish — the 1:1 amplification that drove the #90 storm is gone.
    let mut server = storm_server();
    let uri = server.write_file_uri("common/decisions/a.txt", STORM_FILE);

    for _ in 0..10 {
        server.send(&watched_changes(std::slice::from_ref(&uri)));
    }

    let frames = server.drain_after_first(Duration::from_millis(900), Duration::from_secs(8));
    let log = server.profiling_log();
    server.stop();

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
    let mut server = storm_start(storm_builder().env("CWTOOLS_WATCHED_BATCH_PANIC_ONCE", "1"));
    let uri = server.write_file_uri("common/decisions/a.txt", STORM_FILE);

    server.send(&watched_changes(std::slice::from_ref(&uri)));

    let frames = server.drain_after_first(Duration::from_millis(900), Duration::from_secs(8));
    let log = server.profiling_log();
    server.stop();

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
    let mut server = storm_start(storm_builder().env("CWTOOLS_VALIDATE_PANIC_ONCE", "1"));
    let rel = "common/decisions/a.txt";
    server.write_file(rel, STORM_FILE);

    server.open(rel, STORM_FILE);
    let after_open = server.drain_until_quiet(Duration::from_millis(900), Duration::from_secs(8));
    assert_eq!(
        count_publishes(&after_open, "a.txt"),
        0,
        "the panicking open validation should not have published"
    );

    server.change(rel, 2, STORM_FILE);
    let after_edit = server.drain_after_first(Duration::from_millis(900), Duration::from_secs(8));

    // Two edits back to back, so the first is superseded inside the debounce
    // window and its task is aborted rather than left to finish. That abort is
    // the other half of the join's contract: it must not be reported as a
    // panic, or every keystroke would log one. Nothing else here would notice,
    // since the abort is invisible in the publishes.
    for version in [3, 4] {
        server.change(rel, version, STORM_FILE);
    }
    server.drain_after_first(Duration::from_millis(900), Duration::from_secs(8));
    let log = server.profiling_log();
    server.stop();

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
    let mut server = storm_server();
    let m = 8usize;
    let uris: Vec<String> = (0..m)
        .map(|i| server.write_file_uri(&format!("common/decisions/f{i}.txt"), STORM_FILE))
        .collect();

    server.send(&watched_changes(&uris));

    let frames = server.drain_after_first(Duration::from_millis(900), Duration::from_secs(10));
    let log = server.profiling_log();
    server.stop();

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
    let mut server = storm_server();
    let real_uri = server.write_file_uri("common/decisions/real.txt", STORM_FILE);
    let link = server.path("common/decisions/linked.txt");
    std::os::unix::fs::symlink(link.with_file_name("real.txt"), &link).unwrap();
    let link_uri = path_uri(&link);

    server.send(&watched_changes(&[real_uri, link_uri]));

    let frames = server.drain_after_first(Duration::from_millis(900), Duration::from_secs(10));
    let log = server.profiling_log();
    server.stop();

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
    let mut server = storm_server();
    let n = 205usize;
    let uris: Vec<String> = (0..n)
        .map(|i| server.write_file_uri(&format!("common/decisions/b{i}.txt"), STORM_FILE))
        .collect();

    server.send(&watched_changes(&uris));

    let frames = server.drain_through_scan_release(Duration::from_millis(900));
    let log = server.profiling_log();
    server.stop();

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

const RESOURCE_RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
decision = {
    ## cardinality = 0..1
    picture = filepath[gfx/,.dds]
}
"#;

const PICTURE_DOC_REL: &str = "common/decisions/d.txt";

/// A started server whose file index holds one texture, with a decision open
/// whose `picture` names a texture that is not on disk yet (CW113).
fn resource_server() -> TestServer {
    let mut server = storm_start(
        storm_base()
            .rules_file("r.cwt", RESOURCE_RULES)
            .file("gfx/present.dds", "dds"),
    );
    let text = "adec = {\n    picture = new_pic\n}\n";
    server.write_file(PICTURE_DOC_REL, text);
    server.open(PICTURE_DOC_REL, text);
    let opened = server.wait_for_diagnostics("d.txt");
    assert!(
        publish_has_code(&opened, "CW113"),
        "the missing texture must report CW113 before it exists, got: {opened}"
    );
    server
}

/// The last diagnostics published for the open decision.
fn last_picture_publish(frames: &[Value]) -> &Value {
    frames
        .iter()
        .rev()
        .find(|frame| {
            frame["method"] == "textDocument/publishDiagnostics"
                && frame["params"]["uri"]
                    .as_str()
                    .is_some_and(|uri| uri.ends_with("d.txt"))
        })
        .unwrap_or_else(|| panic!("the open decision was not republished, got: {frames:?}"))
}

/// Creates the referenced texture plus `filler` unrelated ones, reports them in
/// one watched batch, and checks the open decision revalidates in place.
fn created_textures_clear_cw113(filler: usize) {
    let mut server = resource_server();
    let mut uris: Vec<String> = (0..filler)
        .map(|i| server.write_file_uri(&format!("gfx/burst/t{i}.dds"), "dds"))
        .collect();
    uris.push(server.write_file_uri("gfx/new_pic.dds", "dds"));

    server.send(&watched_created(&uris));

    let frames = server.drain_after_first(Duration::from_millis(900), Duration::from_secs(10));
    let log = server.profiling_log();
    server.stop();

    assert!(
        !publish_has_code(last_picture_publish(&frames), "CW113"),
        "CW113 must clear once the texture is reported, got: {frames:?}"
    );
    assert_eq!(
        count_publishes(&frames, ".dds"),
        0,
        "a resource must never be published, got: {frames:?}"
    );
    assert!(
        !log.contains("watched batch over cap"),
        "textures must not count toward the rescan cap, got: {log}"
    );
    assert_eq!(
        count_validate_log(&log, "watched"),
        0,
        "a resource must never be parsed or validated, got: {log}"
    );
    assert_eq!(
        count_validate_log(&log, "reindex"),
        0,
        "the open decision must revalidate without a reindex, got: {log}"
    );
}

#[test]
fn test_watched_created_texture_clears_cw113_without_a_reindex() {
    created_textures_clear_cw113(0);
}

#[test]
fn test_watched_created_script_clears_cw113_without_a_reindex() {
    const RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
decision = {
    ## cardinality = 0..1
    picture = filepath[gfx/,.gfx]
}
"#;
    let mut server = storm_start(
        storm_base()
            .rules_file("r.cwt", RULES)
            .file("gfx/present.gfx", "# existing script target\n"),
    );
    let text = "adec = {\n    picture = new_pic\n}\n";
    server.write_file(PICTURE_DOC_REL, text);
    server.open(PICTURE_DOC_REL, text);
    let opened = server.wait_for_diagnostics("d.txt");
    assert!(
        publish_has_code(&opened, "CW113"),
        "the missing script target must report CW113 before it exists, got: {opened}"
    );

    let target = server.write_file_uri("gfx/new_pic.gfx", "# no exported names\n");
    server.send(&watched_created(std::slice::from_ref(&target)));

    let frames = server.drain_after_first(Duration::from_millis(900), Duration::from_secs(10));
    let log = server.profiling_log();
    server.stop();

    assert!(
        !publish_has_code(last_picture_publish(&frames), "CW113"),
        "CW113 must clear after the script target is created, got: {frames:?}"
    );
    assert_eq!(
        count_validate_log(&log, "reindex"),
        0,
        "the open decision must revalidate without a full reindex, got: {log}"
    );
}

#[test]
fn test_watched_texture_burst_does_not_force_a_rescan() {
    // More events than WATCHED_BULK_CAP (200), all resources: a folder copy.
    created_textures_clear_cw113(205);
}

#[test]
fn test_watched_localisation_extension_outside_a_loc_folder_is_ignored() {
    // The resource watcher is broad, so it reports files such as a CI workflow
    // or a map table. Discovery never indexes them, so neither may the watcher.
    let mut server = storm_server();
    let script = server.write_file_uri("common/decisions/a.txt", STORM_FILE);
    let csv = server.write_file_uri("map/definition.csv", "0;0;0;0;land;false;plains;0\n");
    let yml = server.write_file_uri(".github/workflows/ci.yml", "name: ci\n");

    server.send(&watched_changes(&[script, csv, yml]));

    let frames = server.drain_after_first(Duration::from_millis(900), Duration::from_secs(10));
    let log = server.profiling_log();
    server.stop();

    // The script is the precondition: without it a dead watcher passes too.
    assert_eq!(count_publishes(&frames, "a.txt"), 1, "got: {frames:?}");
    assert_eq!(
        count_publishes(&frames, "definition.csv"),
        0,
        "got: {frames:?}"
    );
    assert_eq!(count_publishes(&frames, "ci.yml"), 0, "got: {frames:?}");
    assert_eq!(
        count_validate_log(&log, "watched"),
        1,
        "only the script may validate, got: {log}"
    );
}

/// The number of over-cap batches the profiling log has recorded so far.
fn over_cap_batches(server: &mut TestServer) -> usize {
    server
        .profiling_log()
        .matches("watched batch over cap")
        .count()
}

#[test]
fn test_watched_overcap_batch_does_not_spin_against_running_scan() {
    // An over-cap batch that loses the rescan CAS to an in-flight scan used to
    // requeue and immediately re-arm, retrying (and logging "over cap") every
    // 500ms window for the scan's whole duration (#90 residual). It must park
    // the requeue for the scan winner to drain once: one over-cap line for the
    // losing drain, one for the winner-armed drain that runs the real rescan.
    let signals = tempfile::tempdir().unwrap();
    let gate = signals.path().join("scan-hold");
    let ready = signals.path().join("scan-hold-ready");

    // The startup scan passes before the gate is created; the rescan then waits
    // for the watched drain to report its first over-cap batch.
    let mut server = storm_start(
        storm_builder()
            .env("CWTOOLS_SCAN_HOLD_FILE", &gate)
            .env("CWTOOLS_SCAN_HOLD_READY_FILE", &ready),
    );
    let n = 205usize;
    let uris: Vec<String> = (0..n)
        .map(|i| server.write_file_uri(&format!("common/decisions/s{i}.txt"), STORM_FILE))
        .collect();
    let scan_release = server.scan_release_path();

    // Start a scan, then flood while it holds the CAS. The hold begins after
    // the bar-on, so waiting for that signal keeps the flood's debounce window
    // from slipping past the scan and winning the CAS.
    std::fs::write(&gate, b"hold").unwrap();
    server.reindex_until_scan_starts();
    server.send(&watched_changes(&uris));

    assert!(
        poll_until(
            Instant::now() + Duration::from_secs(30),
            Duration::from_millis(100),
            || (over_cap_batches(&mut server) >= 1).then_some(())
        )
        .is_some(),
        "the losing watched drain never reported its over-cap batch"
    );
    let retried_while_held = poll_until(
        Instant::now() + Duration::from_millis(900),
        Duration::from_millis(100),
        || (over_cap_batches(&mut server) > 1).then_some(()),
    );
    assert!(
        retried_while_held.is_none(),
        "the losing watched drain must park until the scan releases its gate"
    );
    assert!(ready.exists(), "scan did not enter its file gate");
    std::fs::remove_file(&gate).unwrap();
    wait_for_scan_release(&scan_release);

    // The scan's release signal is the readiness edge for the winner-armed
    // retry. Then observe one debounce window to catch any accidental re-arm.
    assert!(
        poll_until(
            Instant::now() + Duration::from_secs(30),
            Duration::from_millis(100),
            || (over_cap_batches(&mut server) >= 2).then_some(())
        )
        .is_some(),
        "the winner-armed watched drain never reported its over-cap batch"
    );
    let retried_after_winner = poll_until(
        Instant::now() + Duration::from_millis(900),
        Duration::from_millis(100),
        || (over_cap_batches(&mut server) > 2).then_some(()),
    );
    assert!(
        retried_after_winner.is_none(),
        "the winner-armed watched batch must not re-arm after its one rescan"
    );
    let log = server.profiling_log();
    server.stop();

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
fn test_create_and_delete_file_notifications_request_client_refreshes() {
    let mut server = TestServer::builder()
        .capabilities(json!({
            "workspace": {
                "semanticTokens": { "refreshSupport": true },
                "codeLens": { "refreshSupport": true },
            },
        }))
        .init_opts(json!({ "language": "hoi4" }))
        .spawn();
    let init = server.init_response().clone();
    assert_eq!(init["id"], 1, "got: {init}");
    let operations = &init["result"]["capabilities"]["workspace"]["fileOperations"];
    let glob = "**/*.{txt,gui,gfx,asset,yml,cwt}";
    for operation in ["didCreate", "didRename", "didDelete"] {
        assert_eq!(
            operations[operation]["filters"][0]["pattern"]["glob"], glob,
            "{operation} file notification must be advertised"
        );
    }

    server.initialized();

    let mut scan_finished = false;
    let mut startup_refreshes = Vec::new();
    for _ in 0..10_000 {
        let frame = server.read_frame();
        if frame["method"] == "loadingBar" && frame["params"]["enable"] == Value::Bool(false) {
            scan_finished = true;
        }
        if frame.get("method").is_some() && frame.get("id").is_some() {
            startup_refreshes.push(frame["method"].as_str().unwrap().to_string());
            server.reply(&frame);
        }
        if scan_finished
            && startup_refreshes
                .iter()
                .any(|method| method == "workspace/semanticTokens/refresh")
            && startup_refreshes
                .iter()
                .any(|method| method == "workspace/codeLens/refresh")
        {
            break;
        }
    }
    assert!(scan_finished, "the initial workspace scan did not finish");
    startup_refreshes.sort();
    assert_eq!(
        startup_refreshes,
        [
            "workspace/codeLens/refresh".to_string(),
            "workspace/semanticTokens/refresh".to_string(),
        ],
        "the server must finish startup refresh requests before the file-operation assertions"
    );

    let create_path = server.write_file(
        "common/national_focus/created.txt",
        "created_focus = { cost = 1 }\n",
    );
    server.notify(
        "workspace/didCreateFiles",
        json!({ "files": [{ "uri": path_uri(&create_path) }] }),
    );
    let created = server.answer_server_request();
    assert_eq!(created["jsonrpc"], "2.0", "got: {created}");
    assert!(
        created.get("id").is_some(),
        "refresh is a request: {created}"
    );
    assert_eq!(created["method"], "workspace/semanticTokens/refresh");

    let delete_path = server.write_file(
        "common/national_focus/deleted.txt",
        "deleted_focus = { cost = 2 }\n",
    );
    std::fs::remove_file(&delete_path).unwrap();
    server.notify(
        "workspace/didDeleteFiles",
        json!({ "files": [{ "uri": path_uri(&delete_path) }] }),
    );
    let first = server.answer_server_request();
    let second = server.answer_server_request();
    let mut deleted = vec![
        first["method"].as_str().unwrap().to_string(),
        second["method"].as_str().unwrap().to_string(),
    ];
    deleted.sort();
    assert_eq!(
        deleted,
        [
            "workspace/codeLens/refresh".to_string(),
            "workspace/semanticTokens/refresh".to_string(),
        ],
        "deleting a file must refresh both cached token and code-lens views"
    );
}
