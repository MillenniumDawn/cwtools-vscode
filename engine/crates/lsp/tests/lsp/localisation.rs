use std::time::Duration;

use serde_json::{Value, json};

use crate::common::{
    GOTO_RULES, TestServer, count_publishes, definition_locations, diagnostics_of, loc,
    storm_builder, storm_start, watched_changes,
};

/// Codes of every publish for `suffix` in `frames`, in arrival order.
fn publish_codes_for(frames: &[Value], suffix: &str) -> Vec<Vec<String>> {
    frames
        .iter()
        .filter(|v| {
            v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["uri"]
                    .as_str()
                    .is_some_and(|u| u.ends_with(suffix))
        })
        .map(|v| {
            diagnostics_of(v)
                .iter()
                .filter_map(|d| d["code"].as_str().map(String::from))
                .collect()
        })
        .collect()
}

fn drain(server: &mut TestServer) -> Vec<Value> {
    server.drain_after_first(Duration::from_millis(900), Duration::from_secs(8))
}

#[test]
fn test_vanilla_loc_is_read_once_and_the_mod_wins_a_shared_key() {
    // #89: the base game's loc is read on the first scan and kept for the rest
    // of the session — a rescan walks the workspace only. Two things follow,
    // and both are asserted here: a key the mod redefines resolves to the mod's
    // file, and a base-game-only key still resolves after a re-index even
    // though the install's loc is no longer on disk (nothing re-read it).
    let cache = tempfile::tempdir().unwrap();
    let use_rel = "localisation/english/use_l_english.yml";
    let use_text = loc(" A:0 \"$SHARED_KEY$\"\n B:0 \"$BASE_ONLY_KEY$\"\n");
    let mut server = TestServer::builder()
        .rules(GOTO_RULES)
        .vanilla_file(
            "localisation/english/base_l_english.yml",
            loc(" SHARED_KEY:0 \"Base text\"\n BASE_ONLY_KEY:0 \"Base only\"\n"),
        )
        .file(
            "localisation/english/def_l_english.yml",
            loc(" SHARED_KEY:0 \"Mod text\"\n"),
        )
        .file(use_rel, use_text.as_str())
        .file("common/scan_trigger.txt", "# trigger\n")
        .init_opts(json!({ "cacheDir": cache.path().to_string_lossy() }))
        .spawn();
    server.initialized();

    server.open(use_rel, &use_text);
    server.wait_for_diagnostics(use_rel);

    // `$SHARED_KEY$` on line 1 col 10, `$BASE_ONLY_KEY$` on line 2 col 10.
    // loc_locations lands via the async scan, so poll until it answers. An
    // empty list means it never did, which each use below asserts against.
    let goto = |server: &mut TestServer, line: u32| -> Vec<String> {
        definition_locations(server, use_rel, line, 10, 50)
            .into_iter()
            .map(|(uri, _)| uri)
            .collect()
    };

    let shared = goto(&mut server, 1);
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
    let base_only = goto(&mut server, 2);
    assert!(
        base_only.iter().any(|u| u.ends_with("base_l_english.yml")),
        "a base-game-only key should resolve to the install, got: {:?}",
        base_only
    );

    // Take the install's loc away and re-index. The memo still has it, so the
    // key resolves; a rescan that re-read the install would lose it.
    std::fs::remove_dir_all(server.vanilla_dir().join("localisation")).unwrap();
    let reindexed = server.request(
        "workspace/executeCommand",
        json!({ "command": "reindexWorkspace", "arguments": [] }),
    );
    assert_eq!(
        reindexed.as_str(),
        Some("Workspace re-indexed."),
        "re-index should have run, got: {reindexed}"
    );

    let after = goto(&mut server, 2);
    server.stop();
    assert!(
        after.iter().any(|u| u.ends_with("base_l_english.yml")),
        "the base game's loc must survive a re-index without being re-read, got: {:?}",
        after
    );
}

#[test]
fn test_watched_loc_value_change_does_not_sweep_open_loc_files() {
    // A watched change to a NON-open loc file must stay out of the live overlay
    // (that map is open-doc state): before this fix its first sight treated the
    // whole file as changed and swept every other open loc file on EVERY event.
    // First sight seeds the watched overlay and may sweep once; a repeat change
    // that leaves the key set unchanged must not sweep at all.
    let watched_rel = "localisation/watched_l_english.yml";
    let open_rel = "localisation/open_l_english.yml";
    let open_text = loc(" OPEN_KEY:0 \"hi\"\n");
    let mut server = storm_start(
        storm_builder()
            .file(watched_rel, loc(" W_KEY:0 \"one\"\n"))
            .file(open_rel, open_text.as_str()),
    );
    let watched_uri = server.uri(watched_rel);
    server.open_as(open_rel, "paradox", &open_text);
    server.wait_for_diagnostics("open_l_english.yml");

    // Round 1: first sight seeds the watched overlay (one batch sweep allowed).
    server.write_file(watched_rel, loc(" W_KEY:0 \"two\"\n"));
    server.send(&watched_changes(std::slice::from_ref(&watched_uri)));
    let frames = drain(&mut server);
    assert_eq!(
        count_publishes(&frames, "watched_l_english.yml"),
        1,
        "the watched loc file itself should be validated and published once"
    );

    // Round 2: same keys again, new value — no sweep, no open-file republish.
    server.write_file(watched_rel, loc(" W_KEY:0 \"three\"\n"));
    server.send(&watched_changes(std::slice::from_ref(&watched_uri)));
    let frames = drain(&mut server);
    server.stop();

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
fn test_open_loc_key_edit_revalidates_only_referencing_open_files() {
    let definition_rel = "localisation/definition_l_english.yml";
    let reference_rel = "localisation/reference_l_english.yml";
    let unrelated_rel = "localisation/unrelated_l_english.yml";
    let definition_text = loc(" DEF_KEY:0 \"definition\"\n");
    let reference_text = loc(" REF_KEY:0 \"see $new_key$\"\n");
    let unrelated_text = loc(" OTHER_KEY:0 \"nothing relevant\"\n");
    let mut server = storm_start(
        storm_builder()
            .file(definition_rel, definition_text.as_str())
            .file(reference_rel, reference_text.as_str())
            .file(unrelated_rel, unrelated_text.as_str()),
    );
    for (rel, suffix, text) in [
        (definition_rel, "definition_l_english.yml", &definition_text),
        (reference_rel, "reference_l_english.yml", &reference_text),
        (unrelated_rel, "unrelated_l_english.yml", &unrelated_text),
    ] {
        server.open_as(rel, "paradox", text);
        let codes = server.diag_codes_for(suffix, 1);
        if suffix == "reference_l_english.yml" {
            assert!(codes.contains(&"CW225".to_string()), "got: {codes:?}");
        }
    }

    server.change(
        definition_rel,
        2,
        &loc(" DEF_KEY:0 \"definition\"\n NEW_KEY:0 \"new\"\n"),
    );
    let frames = drain(&mut server);
    server.stop();

    assert_eq!(count_publishes(&frames, "definition_l_english.yml"), 1);
    assert_eq!(count_publishes(&frames, "reference_l_english.yml"), 1);
    assert_eq!(
        count_publishes(&frames, "unrelated_l_english.yml"),
        0,
        "an unrelated current cache must exclude its document from the sweep"
    );
    let reference_publish = frames
        .iter()
        .find(|frame| {
            frame["method"] == "textDocument/publishDiagnostics"
                && frame["params"]["uri"]
                    .as_str()
                    .is_some_and(|uri| uri.ends_with("reference_l_english.yml"))
        })
        .expect("reference diagnostics publish");
    assert_eq!(reference_publish["params"]["version"], 1);
    let codes: Vec<String> = diagnostics_of(reference_publish)
        .iter()
        .filter_map(|diagnostic| diagnostic["code"].as_str().map(String::from))
        .collect();
    assert!(!codes.contains(&"CW225".to_string()), "got: {codes:?}");
}

#[test]
fn test_open_loc_revalidation_skips_ignored_targets() {
    let definition_rel = "localisation/definition_l_english.yml";
    let ignored_rel = "localisation/ignored_l_english.yml";
    let definition_text = loc(" DEF_KEY:0 \"definition\"\n NEW_KEY:0 \"new\"\n");
    let reference_text = loc(" REF_KEY:0 \"see $new_key$\"\n");
    let mut server = storm_start(
        storm_builder()
            .file(definition_rel, definition_text.as_str())
            .file(ignored_rel, reference_text.as_str())
            .init_opts(json!({ "ignoreFilePatterns": ["ignored_l_english.yml"] })),
    );
    for (rel, suffix, text) in [
        (definition_rel, "definition_l_english.yml", &definition_text),
        (ignored_rel, "ignored_l_english.yml", &reference_text),
    ] {
        server.open_as(rel, "paradox", text);
        let _ = server.diag_codes_for(suffix, 1);
    }

    server.change(definition_rel, 2, &loc(" DEF_KEY:0 \"definition\"\n"));
    let frames = drain(&mut server);
    server.stop();

    assert_eq!(
        count_publishes(&frames, "ignored_l_english.yml"),
        0,
        "an ignored target must not be revalidated by a loc-key sweep"
    );
}

#[test]
fn test_open_loc_revalidation_applies_inline_ignore() {
    let definition_rel = "localisation/definition_l_english.yml";
    let reference_rel = "localisation/reference_l_english.yml";
    let definition_text = loc(" DEF_KEY:0 \"definition\"\n NEW_KEY:0 \"new\"\n");
    let reference_text = loc(" REF_KEY:0 \"see $new_key$\" # cwtools-ignore CW225\n");
    let mut server = storm_start(
        storm_builder()
            .file(definition_rel, definition_text.as_str())
            .file(reference_rel, reference_text.as_str()),
    );
    for (rel, suffix, text) in [
        (definition_rel, "definition_l_english.yml", &definition_text),
        (reference_rel, "reference_l_english.yml", &reference_text),
    ] {
        server.open_as(rel, "paradox", text);
        let codes = server.diag_codes_for(suffix, 1);
        if suffix == "reference_l_english.yml" {
            assert!(!codes.contains(&"CW225".to_string()), "got: {codes:?}");
        }
    }

    server.change(definition_rel, 2, &loc(" DEF_KEY:0 \"definition\"\n"));
    let frames = drain(&mut server);
    server.stop();

    let target_publish = frames
        .iter()
        .find(|frame| {
            frame["method"] == "textDocument/publishDiagnostics"
                && frame["params"]["uri"]
                    .as_str()
                    .is_some_and(|uri| uri.ends_with("reference_l_english.yml"))
        })
        .expect("reference diagnostics publish");
    let codes: Vec<String> = diagnostics_of(target_publish)
        .iter()
        .filter_map(|diagnostic| diagnostic["code"].as_str().map(String::from))
        .collect();
    assert!(!codes.contains(&"CW225".to_string()), "got: {codes:?}");
}

#[test]
fn test_watched_loc_new_keys_revalidate_only_referencing_open_files() {
    let b_rel = "localisation/b_l_english.yml";
    let c_rel = "localisation/c_l_english.yml";
    let open_rel = "localisation/open_l_english.yml";
    let unrelated_rel = "localisation/unrelated_l_english.yml";
    let open_text = loc(" malformed line\n OPEN_KEY:0 \"see $NeW_KeY$ and $new_key2$\"\n");
    let unrelated_text = loc(" OTHER_KEY:0 \"nothing relevant\"\n");
    let mut server = storm_start(
        storm_builder()
            .file(b_rel, loc(" B_KEY:0 \"b\"\n"))
            .file(c_rel, loc(" C_KEY:0 \"c\"\n"))
            .file(open_rel, open_text.as_str())
            .file(unrelated_rel, unrelated_text.as_str()),
    );
    let b_uri = server.uri(b_rel);
    let c_uri = server.uri(c_rel);

    server.open_as(open_rel, "paradox", &open_text);
    let codes = server.diag_codes_for("open_l_english.yml", 1);
    assert!(
        codes.contains(&"CW225".to_string()),
        "the open file's refs must be unresolved before the watched batch, got: {codes:?}"
    );
    assert!(codes.contains(&"CW001".to_string()), "got: {codes:?}");
    server.open_as(unrelated_rel, "paradox", &unrelated_text);
    let _ = server.diag_codes_for("unrelated_l_english.yml", 1);

    server.write_file(b_rel, loc(" B_KEY:0 \"b\"\n NEW_KEY:0 \"n1\"\n"));
    server.write_file(c_rel, loc(" C_KEY:0 \"c\"\n NEW_KEY2:0 \"n2\"\n"));
    server.send(&watched_changes(&[b_uri, c_uri]));

    let frames = drain(&mut server);
    server.stop();

    let open_publishes: Vec<&Value> = frames
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
        "a batch must revalidate the matching open loc file once"
    );
    assert_eq!(
        count_publishes(&frames, "unrelated_l_english.yml"),
        0,
        "an open loc file without changed-key references must not be revalidated"
    );
    let codes: Vec<String> = diagnostics_of(open_publishes[0])
        .iter()
        .filter_map(|d| d["code"].as_str().map(String::from))
        .collect();
    assert!(
        !codes.contains(&"CW225".to_string()),
        "keys added by watched loc files must resolve cross-file, got: {codes:?}"
    );
    assert!(
        codes.contains(&"CW001".to_string()),
        "cached validation must preserve malformed-line diagnostics, got: {codes:?}"
    );
}

#[test]
fn test_watched_loc_keys_survive_scan_index_install() {
    // A workspace scan REPLACES the loc index wholesale from its own disk
    // reads, which can predate a watched change (the read raced the write), so
    // keys held only by the scanned index would silently vanish. The watched
    // overlay must survive the install. Deterministic proxy for the race:
    // revert the watched file on disk with NO watched event, so the rescan
    // reads the OLD content while the overlay still holds the recorded key.
    let b_rel = "localisation/b_l_english.yml";
    let open_rel = "localisation/open_l_english.yml";
    let open_text = loc(" OPEN_KEY:0 \"see $new_key$\"\n");
    let mut server = storm_start(
        storm_builder()
            .file(b_rel, loc(" B_KEY:0 \"b\"\n"))
            .file(open_rel, open_text.as_str()),
    );
    let b_uri = server.uri(b_rel);

    server.open_as(open_rel, "paradox", &open_text);
    let codes = server.diag_codes_for("open_l_english.yml", 1);
    assert!(codes.contains(&"CW225".to_string()), "got: {codes:?}");

    // The watched change adds the key; the batch records it and sweeps.
    server.write_file(b_rel, loc(" B_KEY:0 \"b\"\n NEW_KEY:0 \"n1\"\n"));
    server.send(&watched_changes(std::slice::from_ref(&b_uri)));
    let frames = drain(&mut server);
    let opens = publish_codes_for(&frames, "open_l_english.yml");
    assert!(
        opens
            .last()
            .is_some_and(|c| !c.contains(&"CW225".to_string())),
        "the recorded key must resolve before the rescan, got: {opens:?}"
    );

    // Revert on disk with no event, then rescan: the scan's loc walk reads the
    // pre-change content, standing in for a read that predated the change.
    server.write_file(b_rel, loc(" B_KEY:0 \"b\"\n"));
    server.send_request(
        700,
        "workspace/executeCommand",
        json!({ "command": "reindexWorkspace", "arguments": [] }),
    );
    let frames = server.drain_through_scan_release(Duration::from_millis(900));
    server.stop();

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
    let b_rel = "localisation/b_l_english.yml";
    let open_rel = "localisation/open_l_english.yml";
    let open_text = loc(" OPEN_KEY:0 \"see $new_key$\"\n");
    let mut server = storm_start(
        storm_builder()
            .file(b_rel, loc(" B_KEY:0 \"b\"\n"))
            .file(open_rel, open_text.as_str()),
    );
    let b_uri = server.uri(b_rel);

    server.open_as(open_rel, "paradox", &open_text);
    let codes = server.diag_codes_for("open_l_english.yml", 1);
    assert!(codes.contains(&"CW225".to_string()), "got: {codes:?}");

    // Round 1: the key appears; the open file's ref resolves.
    server.write_file(b_rel, loc(" B_KEY:0 \"b\"\n NEW_KEY:0 \"n1\"\n"));
    server.send(&watched_changes(std::slice::from_ref(&b_uri)));
    let frames = drain(&mut server);
    let opens = publish_codes_for(&frames, "open_l_english.yml");
    assert!(
        opens
            .last()
            .is_some_and(|c| !c.contains(&"CW225".to_string())),
        "the added key must resolve after round 1, got: {opens:?}"
    );

    // Round 2: the key is removed again; the ref must go back to unresolved.
    server.write_file(b_rel, loc(" B_KEY:0 \"b\"\n"));
    server.send(&watched_changes(std::slice::from_ref(&b_uri)));
    let frames = drain(&mut server);
    server.stop();

    let opens = publish_codes_for(&frames, "open_l_english.yml");
    assert_eq!(opens.len(), 1, "the removal must trigger one sweep");
    assert!(
        opens[0].contains(&"CW225".to_string()),
        "a key removed from a watched loc file must stop resolving, got: {opens:?}"
    );
}

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
fn test_watched_loc_delete_revalidates_open_loc_and_game_dependents() {
    let watched_rel = "localisation/watched_l_english.yml";
    let dependent_rel = "localisation/dependent_l_english.yml";
    let game_rel = "common/things/new_thing.txt";
    let loc_text = loc(" REF_KEY:0 \"see $new_thing_desc$\"\n");
    let mut server = storm_start(
        storm_builder()
            .rules_file("missing.cwt", MISSING_LOC_RULES)
            .file(watched_rel, loc(" WATCHED_KEY:0 \"watched\"\n"))
            .file(dependent_rel, loc_text.as_str())
            .file(game_rel, "new_thing = { x = 1 }\n"),
    );
    let watched_uri = server.uri(watched_rel);

    server.open_as(dependent_rel, "paradox", &loc_text);
    let codes = server.diag_codes_for("dependent_l_english.yml", 1);
    assert!(codes.contains(&"CW225".to_string()), "got: {codes:?}");
    server.open(game_rel, "new_thing = { x = 1 }\n");
    let codes = server.diag_codes_for("new_thing.txt", 1);
    assert!(codes.contains(&"CW100".to_string()), "got: {codes:?}");

    server.write_file(
        watched_rel,
        loc(" WATCHED_KEY:0 \"watched\"\n NEW_THING:0 \"New thing\"\n NEW_THING_DESC:0 \"Description\"\n"),
    );
    server.send(&watched_changes(std::slice::from_ref(&watched_uri)));
    let frames = drain(&mut server);
    let loc_codes = publish_codes_for(&frames, "dependent_l_english.yml");
    assert!(
        loc_codes
            .last()
            .is_some_and(|codes| !codes.contains(&"CW225".to_string())),
        "the watched keys must resolve the loc reference, got: {loc_codes:?}"
    );
    let game_codes = publish_codes_for(&frames, "new_thing.txt");
    assert!(
        game_codes
            .last()
            .is_some_and(|codes| !codes.contains(&"CW100".to_string())),
        "the watched keys must resolve the game file's required loc keys, got: {game_codes:?}"
    );

    std::fs::remove_file(server.path(watched_rel)).unwrap();
    server.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": watched_uri, "type": 3 }] }),
    );
    let frames = drain(&mut server);
    server.stop();

    let loc_codes = publish_codes_for(&frames, "dependent_l_english.yml");
    assert!(
        loc_codes
            .last()
            .is_some_and(|codes| codes.contains(&"CW225".to_string())),
        "deleting the watched key must restore CW225, got: {loc_codes:?}"
    );
    let game_codes = publish_codes_for(&frames, "new_thing.txt");
    assert!(
        game_codes
            .last()
            .is_some_and(|codes| codes.contains(&"CW100".to_string())),
        "deleting the watched keys must restore CW100, got: {game_codes:?}"
    );
}

#[test]
fn test_watched_loc_delete_revalidates_live_overlay_dependents() {
    let source_rel = "localisation/source_l_english.yml";
    let dependent_rel = "localisation/dependent_l_english.yml";
    let dependent_text = loc(" REF_KEY:0 \"see $live_key$\"\n");
    let mut server = storm_start(
        storm_builder()
            .file(source_rel, loc(" BASE_KEY:0 \"base\"\n"))
            .file(dependent_rel, dependent_text.as_str()),
    );
    let source_uri = server.uri(source_rel);

    server.open_as(dependent_rel, "paradox", &dependent_text);
    let codes = server.diag_codes_for("dependent_l_english.yml", 1);
    assert!(codes.contains(&"CW225".to_string()), "got: {codes:?}");

    server.open_as(
        source_rel,
        "paradox",
        &loc(" BASE_KEY:0 \"base\"\n LIVE_KEY:0 \"live\"\n"),
    );
    let frames = drain(&mut server);
    let codes = publish_codes_for(&frames, "dependent_l_english.yml");
    assert!(
        codes
            .last()
            .is_some_and(|codes| !codes.contains(&"CW225".to_string())),
        "the live key must resolve the dependent, got: {codes:?}"
    );

    std::fs::remove_file(server.path(source_rel)).unwrap();
    server.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": source_uri, "type": 3 }] }),
    );
    let frames = drain(&mut server);
    server.stop();

    let codes = publish_codes_for(&frames, "dependent_l_english.yml");
    assert!(
        codes
            .last()
            .is_some_and(|codes| codes.contains(&"CW225".to_string())),
        "deleting the open source must restore CW225, got: {codes:?}"
    );
}

#[test]
fn test_genlocall_returns_missing_name_derived_localisation_keys() {
    let mut server = TestServer::builder()
        .rules(MISSING_LOC_RULES)
        .vanilla()
        .file("common/things/test.txt", "my_thing = { x = yes }\n")
        .file(
            "localisation/english/test_l_english.yml",
            loc(" existing_key:0 \"Existing\"\n"),
        )
        .spawn();
    server.initialized();
    server.wait_for_scan_done();

    let response = server.call(
        "workspace/executeCommand",
        json!({ "command": "genlocall", "arguments": [] }),
    );
    server.stop();

    assert_eq!(response["id"], 2, "got: {response}");
    let stubs = response["result"].as_array().expect("localisation stubs");
    assert_eq!(stubs.len(), 1, "got: {response}");
    assert_eq!(stubs[0]["language"], "english");
    assert_eq!(stubs[0]["filename_suggestion"], "generated_l_english.yml");
    assert_eq!(
        stubs[0]["content"], "l_english:\n my_thing:0 \"TODO\"\n my_thing_desc:0 \"TODO\"\n",
        "genlocall must return the missing type-name-derived keys: {response}"
    );
}

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
    //
    // Explicit empty vanilla dir so a real host game install (if any) can't
    // merge in loc data that would mask the missing `my_thing_desc` key.
    let rel_path = "common/things/test.txt";
    let mut server = TestServer::builder()
        .rules(MISSING_LOC_RULES)
        .vanilla()
        // Non-empty loc union (the CW100 gate), but missing `my_thing_desc`.
        .file(
            "localisation/test_l_english.yml",
            loc(" my_thing:0 \"My Thing\"\n"),
        )
        // Empty on disk: the initial scan must not see `my_thing`, so the CW100
        // this test checks for can only have come from the didChange below (the
        // keystroke path), not a diagnostic left over from the scan.
        .file(rel_path, "")
        .spawn();
    server.initialized();
    // The loc index (built from `localisation/` above) is complete by the time
    // the scan flips `index_ready` and the loading bar closes.
    server.wait_for_scan_done();

    server.open_and_wait(rel_path, "");

    // The keystroke path: didChange runs through `parse_and_validate`, not the
    // scan/dependent-sweep's `validate_parsed_with_indexes`.
    server.change(rel_path, 2, "my_thing = { x = 1 }\n");

    let codes = server.diag_codes_for(rel_path, 1);
    server.stop();

    assert!(
        codes.contains(&"CW100".to_string()),
        "a didChange (keystroke) edit must report CW100 for my_thing's missing \
         required `my_thing_desc` loc key once the loc index is non-empty, \
         got: {:?}",
        codes
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
    let rel_path = ".github/workflows/ci.yml";
    let text = "name: CI\non:\n  push:\n    branches: [main]\n";
    let mut server = TestServer::builder()
        .rules_file("r.cwt", RULES)
        .vanilla() // empty dir -> index marked complete
        .file(rel_path, text)
        .spawn();
    server.initialized();
    server.wait_for_scan_done();

    server.open_as(rel_path, "yaml", text);

    let codes = server.diag_codes_for("ci.yml", 1);
    assert!(
        codes.is_empty(),
        "a yml outside any localisation dir must report nothing, got: {codes:?}"
    );

    // Nor may it fall through to the game-script validator: the same yaml
    // sitting inside a rule-matched game dir must still report nothing rather
    // than being parsed (and indexed) as Paradox script.
    let in_tree_rel = "common/decisions/notes.yml";
    server.write_file(in_tree_rel, text);
    server.open_as(in_tree_rel, "yaml", text);
    let in_tree_codes = server.diag_codes_for(in_tree_rel, 1);
    server.stop();
    assert!(
        in_tree_codes.is_empty(),
        "a yml in a game dir is still not game script, got: {in_tree_codes:?}"
    );
}
