use std::time::Duration;

use serde_json::{Value, json};

use crate::common::{
    DYNAMIC_RULES, GOTO_RULES, TestServer, bom, feature_request, poll_until_attempts,
};

/// A server with DYNAMIC_RULES, named scopes, `loc_files` (each: filename under
/// `localisation/`, full text including the `l_xxx:` header; a UTF-8 BOM is
/// prepended) and the one script file on disk, initialized and told so, which
/// starts the workspace scan that builds the loc index. `extra_init` is merged
/// into the init options.
fn hover_server(
    loc_files: &[(&str, &str)],
    script_rel: &str,
    script_text: &str,
    extra_init: Value,
) -> TestServer {
    let mut builder = TestServer::builder()
        .rules(DYNAMIC_RULES)
        // Named scopes so the registry resolves `country` (HOI4 has no hardcoded
        // scope table); lets the hover surface the current scope context.
        .rules_file("scopes.cwt", "scopes = { country = { } state = { } }\n")
        .file(script_rel, script_text)
        // The workspace scan (which builds the loc index) early-returns when
        // there are no game files. Real mods always have some; drop a tiny one
        // so the scan runs even when the opened document is itself a .yml.
        .file("common/_scan_trigger.txt", "# scan trigger\n")
        .init_opts(extra_init);
    for (name, content) in loc_files {
        builder = builder.file(&format!("localisation/{name}"), bom(content));
    }
    let mut server = builder.spawn();
    server.initialized();
    server
}

/// Hover at (line, character) in `rel_path` until the markdown contains `expect`,
/// or the attempts run out, and return the last markdown. A `null` hover means
/// the loc index is not built yet, so it reads as empty and the poll goes on.
fn hover_until(
    server: &mut TestServer,
    rel_path: &str,
    line: u32,
    character: u32,
    expect: &str,
) -> String {
    let uri = server.uri(rel_path);
    let mut last = String::new();
    let _ = poll_until_attempts(120, Duration::from_millis(500), || {
        let hover = server.request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            }),
        );
        last = hover["contents"]["value"]
            .as_str()
            .unwrap_or("")
            .to_string();
        last.contains(expect).then_some(())
    });
    last
}

/// Spawn a server over `loc_files` and the one script file, run the workspace
/// scan, then return hovers at (line, character) before and after each live
/// settings update.
fn hover_markdowns_with_live_settings(
    loc_files: &[(&str, &str)],
    script_rel: &str,
    script_text: &str,
    line: u32,
    character: u32,
    extra_init: Value,
    live_settings: &[(Value, &str)],
) -> Vec<String> {
    let mut server = hover_server(loc_files, script_rel, script_text, extra_init);
    server.open(script_rel, script_text);

    // Poll hover until the initial loc index is populated, then after each
    // live-setting update until its distinctive text appears. The notification
    // has no response, so a subsequent request is the observable completion.
    let mut expected_values: Vec<(Option<&Value>, &str)> = vec![(None, "Localisation")];
    expected_values.extend(
        live_settings
            .iter()
            .map(|(settings, expected)| (Some(settings), *expected)),
    );
    let mut hover_values = Vec::with_capacity(expected_values.len());
    for (settings, expected) in expected_values {
        if let Some(settings) = settings {
            server.notify(
                "workspace/didChangeConfiguration",
                json!({ "settings": settings }),
            );
        }
        hover_values.push(hover_until(
            &mut server,
            script_rel,
            line,
            character,
            expected,
        ));
    }
    server.stop();
    hover_values
}

fn hover_markdown(
    loc_files: &[(&str, &str)],
    script_rel: &str,
    script_text: &str,
    line: u32,
    character: u32,
    extra_init: Value,
) -> String {
    hover_markdowns_with_live_settings(
        loc_files,
        script_rel,
        script_text,
        line,
        character,
        extra_init,
        &[],
    )
    .into_iter()
    .next()
    .expect("one hover per probe")
}

#[test]
fn test_hover_shows_current_scope() {
    // Anything hovered inside a scoped block shows the current scope context,
    // independent of whether the rule declares a required scope. The decisions
    // file is country-scoped, so a trigger value there reads as `country`.
    let hover = hover_markdown(
        &[("test_l_english.yml", "l_english:\n my_focus:0 \"Focus\"\n")],
        "common/decisions/d.txt",
        "my_dec = {\n    allowed = {\n        has_completed_focus = my_focus\n    }\n}\n",
        2,
        32,
        serde_json::json!({}),
    );
    assert!(
        hover.contains("**Scope**: country"),
        "hover should surface the current scope, got: {hover}"
    );
}

#[test]
fn test_hover_nested_loc_key_in_yml() {
    // Hovering a `$MY_KEY$` reference inside a .yml loc value resolves to the
    // referenced loc entry's text (nested loc keys / dynamic bindings).
    let hover = hover_markdown(
        &[("test_l_english.yml", "l_english:\n MY_KEY:0 \"My Value\"\n")],
        "localisation/english/ref_l_english.yml",
        "\u{FEFF}l_english:\n OTHER:0 \"see $MY_KEY$\"\n",
        1,
        17,
        serde_json::json!({}),
    );
    assert!(
        hover.contains("My Value"),
        "hover on $MY_KEY$ should resolve to the loc entry text, got: {hover}"
    );
}

#[test]
fn test_hover_shows_localisation() {
    // A reference: the loc key appears as a leaf value (`name = my_idea`).
    let hover = hover_markdown(
        &[(
            "test_l_english.yml",
            "l_english:\n my_idea:0 \"My Awesome Idea\"\n",
        )],
        "common/countries/test.txt",
        "my_country = {\n    name = my_idea\n}\n",
        1,
        14,
        serde_json::json!({}),
    );
    assert!(
        hover.contains("Localisation"),
        "hover should include localisation section, got: {hover}"
    );
    assert!(
        hover.contains("My Awesome Idea"),
        "hover should include loc text, got: {hover}"
    );
    assert!(
        hover.contains("English"),
        "hover should include language label, got: {hover}"
    );
}

#[test]
fn test_loc_edit_updates_hover_without_a_rescan() {
    // #53 / #292: loc_text is patched on a loc didChange. A revert that only
    // rebuilt it on the workspace scan would keep serving the old hover.
    let loc_rel = "localisation/test_l_english.yml";
    let loc_old = "l_english:\n my_idea:0 \"Old Name\"\n other_idea:0 \"Other Text\"\n";
    let loc_new = "l_english:\n my_idea:0 \"New Name\"\n other_idea:0 \"Other Text\"\n";
    let script_rel = "common/countries/test.txt";
    let script_text = "my_country = {\n    name = my_idea\n    title = other_idea\n}\n";

    let mut server = hover_server(
        &[("test_l_english.yml", loc_old)],
        script_rel,
        script_text,
        json!({}),
    );
    server.wait_for_scan_done();
    server.open_and_wait(script_rel, script_text);

    let before = hover_until(&mut server, script_rel, 1, 14, "Old Name");
    assert!(
        before.contains("Old Name"),
        "scan-built loc_text must show the on-disk loc, got: {before}"
    );

    server.open_and_wait(loc_rel, loc_old);
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": server.uri(loc_rel), "version": 2 },
            "contentChanges": [{ "text": loc_new }],
        }),
    );
    server.wait_for_diagnostics(loc_rel);

    let after = hover_until(&mut server, script_rel, 1, 14, "New Name");
    assert!(
        after.contains("New Name"),
        "a loc didChange must patch hover without a rescan, got: {after}"
    );
    assert!(
        !after.contains("Old Name"),
        "the pre-edit loc text must not linger after the patch, got: {after}"
    );

    let neighbour = hover_until(&mut server, script_rel, 2, 16, "Other Text");
    server.stop();
    assert!(
        neighbour.contains("Other Text"),
        "editing one key must leave an untouched key's hover alone, got: {neighbour}"
    );
}

#[test]
fn test_hover_idea_definition_shows_name_and_desc() {
    // A definition key: the idea token IS the loc key, with `<key>_desc` for the
    // description. Hover the key itself (not a value reference).
    let hover = hover_markdown(
        &[(
            "test_l_english.yml",
            "l_english:\n my_great_idea:0 \"Great Idea\"\n my_great_idea_desc:0 \"It is great.\"\n",
        )],
        "common/ideas/test.txt",
        "my_great_idea = {\n    cost = 5\n}\n",
        0,
        3,
        serde_json::json!({}),
    );
    assert!(
        hover.contains("Great Idea"),
        "hover on an idea key should show its name loc, got: {hover}"
    );
    assert!(
        hover.contains(r#"It is great\."#),
        "hover on an idea key should show its _desc loc, got: {hover}"
    );
}

#[test]
fn test_hover_value_field_previews_localisation() {
    // #317: a leaf whose right-hand is `value[...]` (e.g. a script variable
    // reference like `has_country_flag = "my_war_flag"`) must preview the
    // localisation the value resolves to, the same way a `name = my_idea`
    // (LocalisationField) reference does. The right-hand classification is
    // `VariableGetField("country_flag")` here, not the scalar/localisation path.
    let hover = hover_markdown(
        &[(
            "test_l_english.yml",
            "l_english:\n my_war_flag:0 \"War Flag\"\n",
        )],
        "common/decisions/test.txt",
        "my_dec = {\n    allowed = { has_country_flag = \"my_war_flag\" }\n    cost = 1\n}\n",
        1,
        36,
        serde_json::json!({}),
    );
    assert!(
        hover.contains("War Flag"),
        "hover on a value[...] flag must preview the loc key, got: {hover}"
    );
}

#[test]
fn test_hover_default_hides_other_languages() {
    // Default (hoverShowAllLanguages off): only the primary language is shown.
    let hover = hover_markdown(
        &[
            (
                "test_l_english.yml",
                "l_english:\n my_idea:0 \"English Name\"\n",
            ),
            (
                "test_l_french.yml",
                "l_french:\n my_idea:0 \"Nom Francais\"\n",
            ),
        ],
        "common/countries/test.txt",
        "my_country = {\n    name = my_idea\n}\n",
        1,
        14,
        serde_json::json!({}),
    );
    assert!(
        hover.contains("English Name"),
        "hover should show the primary (English) loc, got: {hover}"
    );
    assert!(
        !hover.contains("Nom Francais"),
        "hover should not show other languages by default, got: {hover}"
    );
}

#[test]
fn test_hover_show_all_languages_flag() {
    // With hoverShowAllLanguages on, every collected language is shown.
    let hover = hover_markdown(
        &[
            (
                "test_l_english.yml",
                "l_english:\n my_idea:0 \"English Name\"\n",
            ),
            (
                "test_l_french.yml",
                "l_french:\n my_idea:0 \"Nom Francais\"\n",
            ),
        ],
        "common/countries/test.txt",
        "my_country = {\n    name = my_idea\n}\n",
        1,
        14,
        serde_json::json!({ "hoverShowAllLanguages": true }),
    );
    assert!(
        hover.contains("English Name"),
        "hover should show English loc, got: {hover}"
    );
    assert!(
        hover.contains("Nom Francais"),
        "hover with the flag on should show French loc too, got: {hover}"
    );
}

#[test]
fn test_live_configuration_rebuilds_localisation_hover() {
    let hovers = hover_markdowns_with_live_settings(
        &[
            (
                "test_l_english.yml",
                "l_english:\n my_idea:0 \"English Name\"\n",
            ),
            (
                "test_l_french.yml",
                "l_french:\n my_idea:0 \"Nom Francais\"\n",
            ),
        ],
        "common/countries/test.txt",
        "my_country = {\n    name = my_idea\n}\n",
        1,
        14,
        serde_json::json!({ "localisationLanguages": ["English"] }),
        &[
            (
                serde_json::json!({
                    "localisationLanguages": ["French"],
                    "hoverShowAllLanguages": false,
                }),
                "Nom Francais",
            ),
            (
                serde_json::json!({
                    "localisationLanguages": ["French"],
                    "hoverShowAllLanguages": true,
                }),
                "English Name",
            ),
        ],
    );
    assert!(
        hovers[0].contains("English Name") && !hovers[0].contains("Nom Francais"),
        "initial hover should use English only, got: {:?}",
        hovers[0]
    );
    assert!(
        hovers[1].contains("Nom Francais") && !hovers[1].contains("English Name"),
        "changing languages should rebuild the hover map, got: {:?}",
        hovers[1]
    );
    assert!(
        hovers[2].contains("English Name") && hovers[2].contains("Nom Francais"),
        "enabling all-language hovers should rebuild the hover map, got: {:?}",
        hovers[2]
    );
}

#[test]
fn test_cwt_hover_type_reference_describes_type() {
    let files = &[("extra/my.cwt", "my_block = {\n    slot = <focus>\n}\n")];
    let result = feature_request(
        GOTO_RULES,
        files,
        &["extra/my.cwt"],
        serde_json::json!({}),
        "extra/my.cwt",
        "textDocument/hover",
        serde_json::json!({ "position": { "line": 1, "character": 14 } }),
    );
    let md = result["contents"]["value"].as_str().unwrap_or("");
    assert!(
        md.contains("focus") && md.contains("type"),
        "hover must describe the type, got: {}",
        result
    );
    assert!(
        md.contains("common/national_focus"),
        "hover must list the type's path, got: {}",
        result
    );
}
