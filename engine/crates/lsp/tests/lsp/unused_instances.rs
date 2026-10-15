use std::time::Duration;

use cwtools_test_support::{
    ALIAS_BRANCH_LIMIT_RULES, CAPPED_UNUSED_RULES, UNUSED_THINGS as A_TEXT, UNUSED_USERS as B_TEXT,
    capped_alias_file, unused_rules,
};

use crate::common::{TestServer, count_publishes, diagnostics_of};

const A_REL: &str = "common/things/a.txt";
const B_REL: &str = "common/users/b.txt";

/// A server over a workspace with one `should_be_used` type: `a.txt` defines
/// `used_thing` (referenced from `b.txt`) and `lone_thing` (referenced from
/// nowhere). The startup scan is running.
fn unused_server() -> TestServer {
    let mut server = TestServer::builder()
        .rules_file("r.cwt", &unused_rules(true))
        .vanilla() // empty dir -> index marked complete
        .file(A_REL, A_TEXT)
        .file(B_REL, B_TEXT)
        .scan_release_marker()
        .spawn();
    server.initialized();
    server
}

/// A server over a workspace holding `files` under `rules`, with the startup
/// scan running.
fn alias_server(rules: &str, files: &[(&str, &str)]) -> TestServer {
    let mut builder = TestServer::builder().rules_file("r.cwt", rules).vanilla();
    for (rel, text) in files {
        builder = builder.file(rel, *text);
    }
    let mut server = builder.spawn();
    server.initialized();
    server
}

const CAPPED_ALIAS_REL_PATH: &str = "common/users/capped.txt";

#[test]
fn test_scan_reports_unused_should_be_used_instance() {
    // The workspace scan runs the batch-style two-phase pass, so a definition
    // nothing in the workspace references gets CW239 without any file open.
    let mut server = unused_server();
    let a_diags = server.diag_codes_for("a.txt", 1);
    server.stop();
    assert!(
        a_diags.contains(&"CW239".to_string()),
        "lone_thing is referenced nowhere, expected CW239, got: {a_diags:?}"
    );
}

#[test]
fn test_scan_reports_alias_branch_limit() {
    let mut server = alias_server(
        ALIAS_BRANCH_LIMIT_RULES,
        &[(CAPPED_ALIAS_REL_PATH, capped_alias_file(0).as_str())],
    );
    let diagnostics = server.diag_codes_for(CAPPED_ALIAS_REL_PATH, 1);
    server.stop();
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
    let mut server = alias_server(
        ALIAS_BRANCH_LIMIT_RULES,
        &[(CAPPED_ALIAS_REL_PATH, capped_alias_file(150).as_str())],
    );
    let diagnostics = server.diag_codes_for(CAPPED_ALIAS_REL_PATH, 1);
    server.stop();
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
    let mut server = alias_server(
        CAPPED_UNUSED_RULES,
        &[
            ("common/things/a.txt", LONE),
            (CAPPED_ALIAS_REL_PATH, capped_alias_file(0).as_str()),
        ],
    );
    server.wait_for_scan_done();

    server.open("common/things/a.txt", LONE);
    let while_capped = server.diag_codes_for("a.txt", 1);
    assert!(
        !while_capped.contains(&"CW239".to_string()),
        "a capped file can't prove nothing uses lone_thing: {while_capped:?}"
    );

    // Same file without the recursion: it validates fully, records no uses, and
    // lone_thing really is referenced nowhere.
    server.open(CAPPED_ALIAS_REL_PATH, &capped_alias_file(0));
    let _ = server.diag_codes_for("capped.txt", 1);
    server.change(CAPPED_ALIAS_REL_PATH, 2, "a_user = { }\n");
    let after_fix = server.diag_codes_for("a.txt", 1);
    server.stop();
    assert!(
        after_fix.contains(&"CW239".to_string()),
        "with the cap lifted lone_thing is unreferenced and must report: {after_fix:?}"
    );
}

#[test]
fn test_edit_toggling_reference_updates_open_cw239() {
    // Editing a reference in one open file must flip CW239 on the open file
    // that DEFINES the instance, through the dependent sweep — no rescan.
    let mut server = unused_server();
    server.wait_for_scan_done();

    server.open(A_REL, A_TEXT);
    let before = server.diag_codes_for("a.txt", 1);
    assert!(
        before.contains(&"CW239".to_string()),
        "expected CW239 on lone_thing while unreferenced, got: {before:?}"
    );

    server.open(B_REL, B_TEXT);
    let _ = server.diag_codes_for("b.txt", 1);

    // Add a reference to lone_thing: the sweep must republish a.txt clean.
    server.change(
        B_REL,
        2,
        "a_user = { uses = used_thing }\nb_user = { uses = lone_thing }\n",
    );
    let after_add = server.diag_codes_for("a.txt", 1);
    assert!(
        !after_add.contains(&"CW239".to_string()),
        "adding a reference should clear lone_thing's CW239, got: {after_add:?}"
    );

    // Remove it again: the CW239 must come back the same way.
    server.change(B_REL, 3, B_TEXT);
    let after_remove = server.diag_codes_for("a.txt", 1);
    server.stop();
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
    let mut server = unused_server();
    server.wait_for_scan_done();

    server.open(A_REL, A_TEXT);
    let before = server.diag_codes_for("a.txt", 1);
    assert_eq!(
        before.iter().filter(|c| *c == "CW239").count(),
        1,
        "only lone_thing is unreferenced at rest, got: {before:?}"
    );

    server.open(B_REL, B_TEXT);
    let _ = server.diag_codes_for("b.txt", 1);

    server.change(B_REL, 2, "a_user = { }\n");
    let while_edited = server.diag_codes_for("a.txt", 1);
    assert_eq!(
        while_edited.iter().filter(|c| *c == "CW239").count(),
        2,
        "the unsaved edit drops used_thing's only reference, got: {while_edited:?}"
    );

    // Close without saving: b.txt on disk still references used_thing.
    server.close(B_REL);
    // Collected with a budget rather than read straight off the pipe: without
    // the refresh nothing republishes a.txt at all, which must fail the test
    // rather than block it.
    let frames = server.drain_after_first(Duration::from_millis(800), Duration::from_secs(6));
    server.stop();

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
    let codes: Vec<&str> = diagnostics_of(republished)
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
    let mut server = unused_server();
    let scanned = server.diag_codes_for("a.txt", 1);
    assert!(
        scanned.contains(&"CW239".to_string()),
        "lone_thing starts out referenced by nothing, got: {scanned:?}"
    );
    server.wait_for_scan_done();

    server.open(B_REL, B_TEXT);
    let _ = server.diag_codes_for("b.txt", 1);

    // Reference lone_thing from the open buffer. a.txt, which defines it, is
    // closed and stays closed.
    server.change(
        B_REL,
        2,
        "a_user = { uses = used_thing }\nb_user = { uses = lone_thing }\n",
    );
    let frames = server.drain_after_first(Duration::from_millis(800), Duration::from_secs(6));
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
    server.reindex_until_scan_starts();
    let republished = server.read_until_within(
        Duration::from_secs(30),
        "the reindex to republish a.txt",
        |v| {
            v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with("a.txt"))
        },
    );
    server.stop();

    let codes: Vec<&str> = diagnostics_of(&republished)
        .iter()
        .filter_map(|d| d["code"].as_str())
        .collect();
    assert!(
        !codes.contains(&"CW239"),
        "the scan must catch the closed file up on the reference added since, \
         got: {codes:?}"
    );
}
