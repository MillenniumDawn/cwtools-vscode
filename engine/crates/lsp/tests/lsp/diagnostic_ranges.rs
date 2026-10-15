use serde_json::Value;

use crate::common::{TestServer, diagnostics_of};

const REL: &str = "common/scripted_effects/e.txt";
const TEXT: &str = "my_effect = {\n    NOT = { a = 1 b = 2 }\n    x = yes\n}\n";

/// A server over `e.txt`, which holds one CW223. CW223 needs no rules to fire.
fn range_server() -> TestServer {
    let mut server = TestServer::builder()
        .rules_file(
            "types.cwt",
            "types = {\n    type[scripted_effect] = {\n        path = \"game/common/scripted_effects\"\n    }\n}\n",
        )
        .file(REL, TEXT)
        .spawn();
    server.initialized();
    server
}

/// The range of the CW223 in a `publishDiagnostics` for `e.txt`.
fn cw223_range(frame: &Value) -> Option<Value> {
    if frame["method"] != "textDocument/publishDiagnostics"
        || !frame["params"]["uri"]
            .as_str()
            .is_some_and(|u| u.ends_with("e.txt"))
    {
        return None;
    }
    let diagnostic = diagnostics_of(frame)
        .iter()
        .find(|d| d["code"] == "CW223")?;
    Some(diagnostic["range"].clone())
}

/// The first publish for `e.txt` that carries a CW223 and the document version.
///
/// The workspace scan publishes without a `version` and with the whole-line
/// fallback range (no doc text at hand). The open-doc publishers (didOpen
/// re-validate, post-scan refresh) tag the frame with the document version and
/// publish precise columns. Skip the scan's frame so the race can't hand a test
/// the imprecise range (#179).
fn open_doc_cw223(server: &mut TestServer) -> Value {
    // No fixed frame budget: the diagnostic can trail an arbitrary number of
    // other notifications (progress, other files' diagnostics), and under a
    // loaded CI box the server is slow to emit them.
    server.read_until("a version-tagged CW223 publish for e.txt", |frame| {
        frame["params"]["version"].as_i64().is_some() && cw223_range(frame).is_some()
    })
}

/// Nothing asserted a published diagnostic's range end, which is how the parser's
/// whitespace-absorbing `SourceRange.end` reached users as a squiggle running a
/// line long. CW223 needs no rules to fire, so this is a pure range check.
#[test]
fn test_published_diagnostic_range_stays_on_its_own_line() {
    let mut server = range_server();
    server.open(REL, TEXT);
    let publish = open_doc_cw223(&mut server);
    server.stop();

    let range = cw223_range(&publish).expect("no CW223 diagnostic published");
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
    let mut server = range_server();

    // Phase 1: the scan's publish. e.txt is on disk and not open yet, so the
    // scan validates it with no document text and publishes the version-less
    // fallback range.
    let scan = server.read_until("the scan's CW223 publish for e.txt", |frame| {
        cw223_range(frame).is_some()
    });
    let scan_version = scan["params"]["version"].clone();
    let scan_range = cw223_range(&scan).expect("the scan published a CW223");

    // Phase 2: open the doc, then wait for the open-doc re-validate's publish.
    // The scan's version-less frame is already in the pipe, so skipping it is
    // exactly what the range test's filter must do; the first version-tagged
    // CW223 is always the precise range.
    server.open(REL, TEXT);
    let opened = open_doc_cw223(&mut server);
    server.stop();
    let open_version = opened["params"]["version"].clone();
    let open_range = cw223_range(&opened).expect("the open-doc publish carries a CW223");

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
