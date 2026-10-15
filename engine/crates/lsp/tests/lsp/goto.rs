use serde_json::json;

use crate::common::{GOTO_RULES, TestServer, bom, definition_locations, feature_request};

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
    let mut builder = TestServer::builder().rules(rules);
    for (name, content) in loc_files {
        builder = builder.file(&format!("localisation/{name}"), bom(content));
    }
    for (rel, content) in files {
        builder = builder.file(rel, *content);
    }
    let mut server = builder.spawn();
    server.initialized();
    for (rel, content) in files {
        server.open_and_wait(rel, content);
    }
    // Loc-key goto depends on the async workspace scan populating loc_locations;
    // under parallel test load that can lag far beyond the fast no-load case, so
    // poll very generously.
    let out = definition_locations(&mut server, doc_rel, line0, char0, 200);
    server.stop();
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
fn test_goto_scripted_loc_localization_key_without_type() {
    // #725: untyped localization_key in scripted_localisation still jumps to the yml.
    let loc = &[("test_l_english.yml", "l_english:\n MY_KEY:0 \"Text\"\n")];
    let files = &[(
        "common/scripted_localisation/s.txt",
        "defined_text = {\n    name = GetFoo\n    text = {\n        localization_key = MY_KEY\n    }\n}\n",
    )];
    let locs = goto_def(
        GOTO_RULES,
        loc,
        files,
        "common/scripted_localisation/s.txt",
        3,
        30,
    );
    assert!(
        locs.iter().any(|(u, _)| u.ends_with("test_l_english.yml")),
        "goto on an untyped localization_key must resolve to the yml, got: {:?}",
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
    let cache = tempfile::tempdir().unwrap();
    // A mod decision that references a base-game focus defined only in the
    // vanilla install.
    let decision_rel = "common/decisions/d.txt";
    let decision_text = "my_dec = {\n    has_focus = VANILLA_FOCUS\n}\n";
    let mut server = TestServer::builder()
        .rules(GOTO_RULES)
        .vanilla_file(
            "common/national_focus/base.txt",
            "VANILLA_FOCUS = { x = yes }\n",
        )
        .file(decision_rel, decision_text)
        .init_opts(json!({ "cacheDir": cache.path().to_string_lossy() }))
        .spawn();
    server.initialized();
    server.open_and_wait(decision_rel, decision_text);

    // Cursor on VANILLA_FOCUS (line 1, col 16). Poll: the vanilla index lands
    // via the async workspace scan, so goto is empty until the merge completes.
    let out = definition_locations(&mut server, decision_rel, 1, 16, 50);
    server.stop();

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
