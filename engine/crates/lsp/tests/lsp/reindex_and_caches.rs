use std::time::Duration;

use serde_json::json;

use crate::common::{GOTO_RULES, TestServer, diagnostics_of, storm_builder, storm_start};

const A_REL: &str = "common/scripted_effects/a.txt";
const A_TEXT: &str = "my_se = { log = \"hi\" }\n";
const B_REL: &str = "common/decisions/b.txt";
const B_TEXT: &str = "my_dec = {\n    complete_effect = {\n        my_se = yes\n    }\n}\n";

#[test]
fn test_rescan_prunes_deleted_file_from_index() {
    // Definition file A holds scripted_effect my_se; caller B references it.
    // A is deleted from disk (no watcher event — e.g. deleted while the server
    // wasn't watching, or the file was never open) and a rescan is forced via
    // `clearAllCaches`. The rescan re-indexes what's still on disk (just B) but
    // must also PRUNE A's now-stale entries, or my_se keeps "resolving" forever
    // and B's CW263 never comes back.
    let cache_dir = tempfile::tempdir().unwrap(); // isolate clearAllCaches from the real cache
    let mut server = TestServer::builder()
        .rules_file("r.cwt", GOTO_RULES)
        .vanilla() // empty dir -> index marked complete
        .init_opts(json!({ "cacheDir": cache_dir.path().to_string_lossy() }))
        .file(A_REL, A_TEXT)
        .file(B_REL, B_TEXT)
        .spawn();
    server.initialized();

    // Both files exist on disk for the initial scan, so my_se resolves.
    let before = server.diag_codes_for("b.txt", 1);
    assert!(
        !before.contains(&"CW263".to_string()),
        "expected my_se to resolve while A exists, got: {:?}",
        before
    );

    // Delete the definition, then force a rescan (no file watcher in this test).
    std::fs::remove_file(server.path(A_REL)).unwrap();
    server.send_request(
        2,
        "workspace/executeCommand",
        json!({"command": "clearAllCaches", "arguments": []}),
    );

    let after = server.diag_codes_for("b.txt", 1);
    server.stop();
    assert!(
        after.contains(&"CW263".to_string()),
        "deleting A should resurrect B's CW263 once the rescan prunes it, got: {:?}",
        after
    );
}

#[test]
fn test_clear_all_caches_only_deletes_cwtools_caches() {
    // #159: `cacheDir` is client input, and the purge used to recursively delete
    // `<cacheDir>/parse-cache` and every `vanilla-*` child by name. Point it at
    // a directory holding unrelated entries of both shapes: they must survive
    // the startup scan (which prunes) and the command (which purges), while the
    // genuine caches next to them are cleared.
    let cache_dir = tempfile::tempdir().unwrap();
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

    let mut server = TestServer::builder()
        .rules_file("r.cwt", GOTO_RULES)
        .vanilla()
        .init_opts(json!({ "cacheDir": cache_dir.path().to_string_lossy() }))
        .file("a.txt", "my_dec = { }\n")
        .spawn();
    server.initialized();
    server.wait_for_diagnostics("a.txt");

    let result = server.request(
        "workspace/executeCommand",
        json!({"command": "clearAllCaches", "arguments": []}),
    );
    server.stop();

    let message = result.as_str().expect("clearAllCaches returns a message");
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

/// Read until a `publishDiagnostics` for a URI ending in `suffix` arrives whose
/// codes no longer include `missing_code`. Fails immediately if a `loadingBar`
/// notification is observed along the way — a quiet background pass must never
/// touch the status bar, unlike the startup scan or `clearAllCaches`.
fn wait_for_cleared_diag_quiet(
    server: &mut TestServer,
    suffix: &str,
    missing_code: &str,
    budget: Duration,
) {
    server.read_until_within(
        budget,
        &format!("{suffix} diagnostics without {missing_code}"),
        |v| {
            assert!(
                v["method"] != "loadingBar",
                "unexpected loadingBar notification during quiet background pass: {v}"
            );
            v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with(suffix))
                && !diagnostics_of(v).iter().any(|d| d["code"] == missing_code)
        },
    );
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
    //
    // Only the caller exists on disk at first; the definition is added later,
    // directly on disk, simulating a watcher-missed change.
    let mut server = TestServer::builder()
        .rules_file("r.cwt", GOTO_RULES)
        .vanilla() // empty dir -> index marked complete
        .file(B_REL, B_TEXT)
        .env("CWTOOLS_REINDEX_INTERVAL_SECS", "1")
        .env("CWTOOLS_REINDEX_IDLE_SECS", "0")
        .spawn();
    server.initialized();
    // The startup scan runs non-quiet; drain its loadingBar traffic before the
    // quiet-observation window starts.
    server.wait_for_scan_done();

    // Open the caller; the definition is absent, so B shows CW263.
    server.open(B_REL, B_TEXT);
    let before = server.diag_codes_for("b.txt", 1);
    assert!(
        before.contains(&"CW263".to_string()),
        "expected CW263 before the definition exists, got: {:?}",
        before
    );

    // Create the defining file directly on disk — no didOpen, no
    // didChangeWatchedFiles notification. Only the periodic background
    // pass's own filesystem walk can find it.
    server.write_file(A_REL, A_TEXT);

    wait_for_cleared_diag_quiet(&mut server, "b.txt", "CW263", Duration::from_secs(30));
    server.stop();
}

#[test]
fn test_background_reindex_survives_a_panicking_pass() {
    // #155: CWTOOLS_REINDEX_PANIC_ONCE makes the FIRST background pass panic
    // before it ever scans. `run_reindex_pass`'s wrapper must log-and-swallow
    // that panic and let `background_reindex_loop`'s own task keep going, so
    // the SECOND pass (one interval later) still discovers the file — same
    // setup as test_background_reindex_picks_up_new_file_quietly, but this
    // proves the loop survives one bad pass instead of just that it works.
    // Goes through a profiling server (CWTOOLS_PROFILE=1) and asserts on the
    // profiling log so a build where the injection never actually fired
    // (env var drift, the injection point moved) fails here instead of
    // passing for the wrong reason — every other assertion below is equally
    // true of a pass that never panicked.
    //
    // Only the caller exists on disk at first; the definition is added later,
    // directly on disk, simulating a watcher-missed change.
    let mut server = storm_start(
        storm_builder()
            .file(B_REL, B_TEXT)
            .env("CWTOOLS_REINDEX_INTERVAL_SECS", "1")
            .env("CWTOOLS_REINDEX_IDLE_SECS", "0")
            .env("CWTOOLS_REINDEX_PANIC_ONCE", "1"),
    );

    // Open the caller; the definition is absent, so B shows CW263.
    server.open(B_REL, B_TEXT);
    let before = server.diag_codes_for("b.txt", 1);
    assert!(
        before.contains(&"CW263".to_string()),
        "expected CW263 before the definition exists, got: {:?}",
        before
    );

    // Create the defining file directly on disk — no didOpen, no
    // didChangeWatchedFiles notification. Only the periodic background
    // pass's own filesystem walk can find it.
    server.write_file(A_REL, A_TEXT);

    // The first pass (interval #1) panics and is swallowed; only the second
    // pass (interval #2, ~1s later) actually scans and finds a.txt. A 15s
    // budget comfortably covers both 1s intervals plus scan time — and turns
    // a stuck condition into a test failure instead of a hung CI job.
    wait_for_cleared_diag_quiet(&mut server, "b.txt", "CW263", Duration::from_secs(15));
    let log = server.profiling_log();
    server.stop();

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
    let mut server = TestServer::builder()
        .rules_file("r.cwt", GOTO_RULES)
        .vanilla() // empty dir -> index marked complete
        .file(B_REL, B_TEXT)
        .init_opts(json!({ "backgroundReindexIdleSeconds": 0 }))
        .env("CWTOOLS_REINDEX_INTERVAL_SECS", "1")
        .spawn();
    server.initialized();
    server.wait_for_scan_done();

    server.open(B_REL, B_TEXT);
    let before = server.diag_codes_for("b.txt", 1);
    assert!(
        before.contains(&"CW263".to_string()),
        "expected CW263 before the definition exists, got: {:?}",
        before
    );

    server.write_file(A_REL, A_TEXT);

    wait_for_cleared_diag_quiet(&mut server, "b.txt", "CW263", Duration::from_secs(10));
    server.stop();
}

#[test]
fn test_clear_all_caches_reports_reindexed_message() {
    // clearAllCaches purges the caches then re-indexes. With no competing scan
    // it wins the CAS on the first try, so the honest-reporting refactor (fix 1)
    // must still surface the success message — not silently no-op. The rescan
    // itself is covered by test_rescan_prunes_deleted_file_from_index; this
    // pins the returned status string.
    let cache_dir = tempfile::tempdir().unwrap();
    let mut server = TestServer::builder()
        .rules_file("r.cwt", GOTO_RULES)
        .vanilla() // empty dir -> index marked complete
        .init_opts(json!({ "cacheDir": cache_dir.path().to_string_lossy() }))
        .file(B_REL, "my_dec = {\n}\n")
        .spawn();
    server.initialized();
    server.wait_for_scan_done();

    let resp = server.call(
        "workspace/executeCommand",
        json!({"command": "clearAllCaches", "arguments": []}),
    );
    server.stop();
    assert_eq!(resp["id"], 2, "got: {}", resp);
    // The file count in the middle depends on what the scan cached, so the two
    // halves that carry meaning are matched instead.
    let result = resp["result"]
        .as_str()
        .unwrap_or_else(|| panic!("clearAllCaches returned no message: {resp}"));
    assert!(
        result.starts_with("Caches cleared (") && result.ends_with("workspace re-indexed."),
        "clearAllCaches should report a successful re-index, got: {}",
        resp
    );
}
