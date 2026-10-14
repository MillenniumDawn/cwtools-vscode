use std::time::{Duration, Instant};

use serde_json::json;

use crate::common::{
    COMPLETION_RULES, GOTO_RULES, STORM_FILE, TestServer, client_spelling, count_publishes,
    count_validate_log, diagnostics_of, path_uri, poll_until, storm_builder, storm_server,
    storm_start,
};

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
    let rel_path = "common/decisions/dup.txt";
    let text = "my_decision = {\n    cost = 5\n}\n";
    let mut server = TestServer::builder()
        .rules(COMPLETION_RULES)
        .file(rel_path, text)
        .spawn_uninitialized();

    let canonical_uri = server.uri(rel_path);
    // The client's spelling: same file, `d` written as `%64`.
    let client_uri = client_spelling(&canonical_uri).replace("dup.txt", "%64up.txt");
    assert_ne!(client_uri, canonical_uri, "the spellings must differ");
    let client_root = client_spelling(&path_uri(server.workspace()));
    let rules_cache = server.rules_dir().to_string_lossy().to_string();
    server.initialize_with(json!({
        "processId": std::process::id(),
        "rootUri": client_root,
        "capabilities": {},
        "initializationOptions": {
            "language": "hoi4",
            "rulesCache": rules_cache,
        }
    }));

    server.open_uri(&client_uri, text);

    // The URI the diagnostics come back under is the key the document was
    // indexed under, so it is what proves the fold happened.
    let published = server.publish_for("up.txt", 1);
    server.stop();

    assert_eq!(
        published["params"]["uri"].as_str(),
        Some(canonical_uri.as_str()),
        "did_open must key the document by the canonical URI, not the client's spelling"
    );
}

#[test]
fn test_did_open_definition_clears_open_caller_stale_error() {
    // Caller B references scripted_effect my_se; the defining file A is opened
    // afterwards. Opening A must re-validate B so its "undefined" diagnostic
    // (CW263 — the call matches no `<scripted_effect>` alias until my_se is
    // indexed) clears without a manual re-save.
    // Only the caller exists on disk at first; the definition is added later.
    let b_rel = "common/decisions/b.txt";
    let b_text = "my_dec = {\n    complete_effect = {\n        my_se = yes\n    }\n}\n";
    let mut server = TestServer::builder()
        .rules_file("r.cwt", GOTO_RULES)
        .vanilla() // empty dir -> index marked complete
        .file(b_rel, b_text)
        .spawn();
    server.initialized();
    // Wait until the scan finishes so diagnostics are no longer deferred.
    server.wait_for_scan_done();

    // Open the caller; the definition is absent, so B shows CW263.
    server.open(b_rel, b_text);
    let before = server.diag_codes_for("b.txt", 1);
    assert!(
        before.contains(&"CW263".to_string()),
        "expected CW263 before the definition is opened, got: {:?}",
        before
    );

    // Now create + open the defining scripted_effect file.
    let a_rel = "common/scripted_effects/a.txt";
    let a_text = "my_se = { log = \"hi\" }\n";
    server.write_file(a_rel, a_text);
    server.open(a_rel, a_text);

    // The did_open dependent sweep must re-publish B without the CW263.
    let after = server.diag_codes_for("b.txt", 1);
    server.stop();
    assert!(
        !after.contains(&"CW263".to_string()),
        "opening the definition file should clear B's stale CW263, got: {:?}",
        after
    );
}

#[test]
fn test_did_open_burst_stops_at_the_document_count_limit() {
    const MAX_OPEN_DOCUMENTS: usize = 128;

    let mut server = storm_server();
    for i in 0..=MAX_OPEN_DOCUMENTS {
        let uri = server.uri(&format!("common/decisions/{i}.txt"));
        server.open_uri(&uri, "x = 1\n");
    }

    let mut validations = 0;
    let deadline = Instant::now() + Duration::from_secs(15);
    let _ = poll_until(deadline, Duration::from_millis(100), || {
        let log = server.profiling_log();
        validations = count_validate_log(&log, "didOpen");
        (validations >= MAX_OPEN_DOCUMENTS).then_some(())
    });
    server.stop();

    assert_eq!(validations, MAX_OPEN_DOCUMENTS);
}

#[test]
fn test_did_open_validates_deferred() {
    // did_open now offloads validation off the message future; the file must
    // still get validated and its diagnostics published.
    let mut server = storm_server();
    let rel = "common/decisions/o.txt";
    std::fs::create_dir_all(server.path("common/decisions")).unwrap();

    server.open(rel, STORM_FILE);

    let frames = server.drain_after_first(Duration::from_millis(800), Duration::from_secs(6));
    let log = server.profiling_log();
    server.stop();

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
    let mut server = storm_server();
    let rel = "common/decisions/c.txt";
    std::fs::create_dir_all(server.path("common/decisions")).unwrap();

    server.open(rel, STORM_FILE);
    server.close(rel);

    let frames = server.drain_after_first(Duration::from_millis(1000), Duration::from_secs(6));
    server.stop();

    let last_publish = frames.iter().rev().find(|v| {
        v["method"] == "textDocument/publishDiagnostics"
            && v["params"]["uri"]
                .as_str()
                .is_some_and(|u| u.ends_with("c.txt"))
    });
    let last = last_publish.expect("expected at least the did_close empty publish for c.txt");
    let diags = diagnostics_of(last);
    assert!(
        diags.is_empty(),
        "final publish for a closed file must be empty, got: {:?}",
        diags
    );
}

#[test]
fn test_did_save_on_ignored_open_file_publishes_empty_diagnostics() {
    let rel = "common/decisions/saved_ignored.txt";
    let mut server = storm_start(
        storm_builder()
            .file(rel, STORM_FILE)
            .init_opts(json!({ "ignoreFilePatterns": ["saved_ignored.txt"] })),
    );
    let uri = server.uri(rel);

    server.open_uri(&uri, STORM_FILE);
    // Consume the open's publication so only the save can satisfy the assert.
    server.wait_for_diagnostics(rel);
    server.save_uri(&uri);
    let saved = server.read_until("the ignored save's publication", |frame| {
        frame["method"] == "textDocument/publishDiagnostics" && frame["params"]["uri"] == uri
    });
    server.stop();
    assert_eq!(saved["params"]["uri"], uri);
    assert_eq!(saved["params"]["version"], 1);
    assert_eq!(saved["params"]["diagnostics"], json!([]));
}

#[test]
fn test_did_save_revalidates_the_open_document() {
    // did_save takes its own path: no text arrives with the notification
    // (`include_text` is false), nothing is written to the document store, and
    // the edit generation is read rather than bumped — it re-validates the
    // buffer the server already holds. Nothing pinned that it validates at all.
    let mut server = storm_server();
    let uri = server.write_file_uri("common/decisions/s.txt", STORM_FILE);

    server.open_uri(&uri, STORM_FILE);
    let opened = server.drain_after_first(Duration::from_millis(800), Duration::from_secs(6));
    assert_eq!(
        count_publishes(&opened, "s.txt"),
        1,
        "the open must publish before the save window opens"
    );

    server.save_uri(&uri);
    let saved = server.drain_after_first(Duration::from_millis(800), Duration::from_secs(6));
    let log = server.profiling_log();
    server.stop();

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
    let mut server = storm_server();
    let uri = server.write_file_uri("common/decisions/u.txt", STORM_FILE);

    server.save_uri(&uri);

    let frames = server.drain_until_quiet(Duration::from_millis(900), Duration::from_secs(6));
    let log = server.profiling_log();
    server.stop();

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
    let definition_rel = "common/national_focus/f.txt";
    let usage_rel = "common/decisions/d.txt";
    let usage = "my_dec = {\n    has_focus = DISK_FOCUS\n}\n";
    let mut server = storm_start(
        storm_builder()
            .file(definition_rel, "DISK_FOCUS = { x = yes }\n")
            .file(usage_rel, usage),
    );
    let definition_uri = server.uri(definition_rel);
    let usage_uri = server.uri(usage_rel);

    server.open_and_wait(usage_rel, usage);
    server.open_and_wait(definition_rel, "LIVE_FOCUS = { x = yes }\n");
    server.close(definition_rel);
    server.wait_for_diagnostics(definition_rel);

    let result = server.request(
        "textDocument/definition",
        json!({
            "textDocument": {"uri": usage_uri},
            "position": {"line": 1, "character": 20},
        }),
    );
    server.stop();
    assert!(
        result.as_array().is_some_and(|locations| locations
            .iter()
            .any(|location| location["uri"] == definition_uri))
            || result["uri"] == definition_uri,
        "closing the live buffer should restore the disk definition, got: {result:?}"
    );
}

/// Fixture adapted from `crates/validation/tests/integration/subtype_membership.rs`: a
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
fn test_did_open_indexes_own_subtype_membership() {
    let rel_path = "common/units/equipment/ships.txt";
    // An explicit empty `vanilla` dir, or `ensure_vanilla_index` auto-discovers
    // a real game install on the host (e.g. a Steam copy of HOI4) and merges its
    // equipment archetypes into `equipment.naval_equip`, masking the bug this
    // test guards against.
    //
    // Empty on disk: the initial workspace scan (which indexes correctly via
    // `index_parsed_file`, unaffected by this bug) must never see the
    // archetype. It's introduced only by the live edit below, so any
    // `equipment.naval_equip` membership the variant resolves against can only
    // have come from `parse_and_validate`'s own indexing of that edit.
    let mut server = TestServer::builder()
        .rules(SUBTYPE_RULES)
        .vanilla()
        .file(rel_path, "")
        .spawn();
    server.initialized();
    // Diagnostics are gated (published as an empty set) until the initial scan
    // finishes and flips `index_ready`; wait for it so the assertions below
    // see the server's real validation output, not the gate's placeholder.
    server.wait_for_scan_done();

    server.open_and_wait(rel_path, "");

    // Edit in the archetype and its naval variant together: the variant's
    // `archetype = <equipment.naval_equip>` discriminator only activates once
    // this file's own archetype is merged into `equipment.naval_equip` by the
    // same edit's indexing pass (did_change also runs through
    // `parse_and_validate`).
    server.change(rel_path, 2, SUBTYPE_SCRIPT);

    let codes = server.diag_codes_for(rel_path, 1);
    server.stop();

    assert!(
        !codes.contains(&"CW263".to_string()),
        "an edit that adds both the archetype and its naval variant in one \
         didChange must index the archetype's own subtype membership so the \
         variant activates naval_equip and `model` is a recognized field, \
         got: {:?}",
        codes
    );
}
