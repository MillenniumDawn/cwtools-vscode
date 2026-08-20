//! Tests for LocalisationField existence checking (CW100 / CW122) wired into
//! the main validation pipeline via `validate_ast_with_loc`.

use cwtools_localization::{LocIndex, LocService};
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::validate_ast_with_loc;

const CWT: &str = r#"
types = {
    type[mytype] = {
        path = "game/common/mytype"
    }
}
mytype = {
    name = localisation
    sname = localisation_synced
    iname = localisation_inline
}
"#;

fn loc_index(files: &[(&str, &str)]) -> LocIndex {
    let svc = LocService::from_files(
        files
            .iter()
            .map(|(p, t)| (p.to_string(), t.to_string()))
            .collect(),
    );
    LocIndex::build(&svc)
}

fn run(script: &str, idx: &LocIndex) -> Vec<cwtools_validation::ValidationError> {
    let table = StringTable::new();
    let parsed_cwt = parse_string(CWT, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);
    let parsed = parse_string(script, &table);
    validate_ast_with_loc(
        &parsed,
        &ruleset,
        &table,
        "test.txt",
        None,
        None,
        None,
        Some(idx),
    )
}

fn cw100s(errs: &[cwtools_validation::ValidationError]) -> usize {
    errs.iter().filter(|e| e.code == Some("CW100")).count()
}

#[test]
fn unsynced_existing_key_ok() {
    let idx = loc_index(&[("a_l_english.yml", "l_english:\n my_key: \"hi\"\n")]);
    let errs = run("mytype = {\n name = my_key\n}\n", &idx);
    assert_eq!(cw100s(&errs), 0, "existing key should not warn: {:?}", errs);
}

#[test]
fn desc_overload_ignores_non_english_commands() {
    const RULES: &str = r#"
types = {
    type[event] = {
        path = "game/events"
    }
}
event = {
    desc = localisation
    desc = {
        text = localisation
    }
}
"#;
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(RULES, &table), &table);
    let idx = loc_index(&[
        (
            "b_l_braz_por.yml",
            "l_braz_por:\n EUevent.5.d: \"[Grecia]\"\n",
        ),
        ("a_l_english.yml", "l_english:\n EUevent.5.d: \"plain\"\n"),
    ]);
    let parsed = parse_string("event = {\n desc = EUevent.5.d\n}\n", &table);
    let errs = validate_ast_with_loc(
        &parsed,
        &ruleset,
        &table,
        "events/EU_events.txt",
        None,
        None,
        None,
        Some(&idx),
    );
    assert!(
        errs.iter().all(|e| e.code != Some("CW267")),
        "English without commands must win the desc leaf, got: {errs:?}"
    );
}

#[test]
fn unsynced_missing_key_warns_cw100() {
    let idx = loc_index(&[("a_l_english.yml", "l_english:\n other: \"hi\"\n")]);
    let errs = run("mytype = {\n name = absent_key\n}\n", &idx);
    assert_eq!(
        cw100s(&errs),
        1,
        "missing key should warn CW100: {:?}",
        errs
    );
}

#[test]
fn inline_quoted_existing_key_warns_cw122() {
    use cwtools_parser::fix::apply_edits;

    let idx = loc_index(&[("a_l_english.yml", "l_english:\n my_key: \"hi\"\n")]);
    let script = "mytype = {\n iname = \"my_key\"  # unnecessary\n}\n";
    let errs = run(script, &idx);
    let cw122 = errs
        .iter()
        .find(|e| e.code == Some("CW122"))
        .expect("quoted inline key warns CW122");
    let fix = cw122
        .fix
        .as_ref()
        .expect("CW122 carries a quote-removal fix");
    assert_eq!(fix.title, "Remove unnecessary quotes");

    let fixed = apply_edits(script, &fix.edits);
    assert_eq!(fixed, "mytype = {\n iname = my_key  # unnecessary\n}\n");
    assert!(
        !run(&fixed, &idx).iter().any(|e| e.code == Some("CW122")),
        "CW122 must be gone after applying the fix"
    );
}

#[test]
fn inline_quoted_key_that_cannot_be_unquoted_has_no_fix() {
    let idx = loc_index(&[("a_l_english.yml", "l_english:\n foo=bar: \"hi\"\n")]);
    let errs = run("mytype = { iname = \"foo=bar\" }", &idx);
    let cw122 = errs
        .iter()
        .find(|e| e.code == Some("CW122"))
        .expect("quoted inline key warns CW122");
    assert!(
        cw122.fix.is_none(),
        "a key that needs quotes must not offer a fix: {:?}",
        cw122
    );
}

#[test]
fn inline_quoted_keys_that_change_value_kind_have_no_fix() {
    let idx = loc_index(&[(
        "a_l_english.yml",
        "l_english:\n yes: \"yes\"\n no: \"no\"\n 123: \"number\"\n -token: \"minus\"\n",
    )]);

    for key in ["yes", "no", "123", "-token"] {
        let errs = run(&format!("mytype = {{ iname = \"{key}\" }}"), &idx);
        let cw122 = errs
            .iter()
            .find(|e| e.code == Some("CW122"))
            .unwrap_or_else(|| panic!("quoted {key:?} must still warn CW122: {errs:?}"));
        assert!(
            cw122.fix.is_none(),
            "removing quotes from {key:?} changes its parsed value kind: {cw122:?}"
        );
    }
}

#[test]
fn inline_quoted_escaped_backslash_key_unquotes_to_the_parsed_key() {
    use cwtools_parser::fix::apply_edits;

    let idx = loc_index(&[("a_l_english.yml", "l_english:\n foo\\bar: \"hi\"\n")]);
    let script = r#"mytype = { iname = "foo\\bar" }"#;
    let errs = run(script, &idx);
    let cw122 = errs
        .iter()
        .find(|e| e.code == Some("CW122"))
        .expect("quoted inline key warns CW122");
    let fix = cw122
        .fix
        .as_ref()
        .expect("a backslash is valid in a bare string key");

    let fixed = apply_edits(script, &fix.edits);
    assert_eq!(fixed, r#"mytype = { iname = foo\bar }"#);
    assert!(
        !run(&fixed, &idx).iter().any(|e| e.code == Some("CW122")),
        "the unquoted parsed key must revalidate cleanly"
    );
}

#[test]
fn inline_quoted_uppercase_boolean_key_is_safely_unquoted() {
    use cwtools_parser::fix::apply_edits;

    let idx = loc_index(&[("a_l_english.yml", "l_english:\n YES: \"hi\"\n")]);
    let script = "mytype = { iname = \"YES\" }";
    let errs = run(script, &idx);
    let cw122 = errs
        .iter()
        .find(|e| e.code == Some("CW122"))
        .expect("quoted inline key warns CW122");
    let fix = cw122
        .fix
        .as_ref()
        .expect("uppercase YES is a bare string, not a boolean");

    let fixed = apply_edits(script, &fix.edits);
    assert_eq!(fixed, "mytype = { iname = YES }");
    assert!(
        !run(&fixed, &idx).iter().any(|e| e.code == Some("CW122")),
        "the safely unquoted key must revalidate cleanly"
    );
}

#[test]
fn inline_quoted_missing_key_is_skipped() {
    let idx = loc_index(&[("a_l_english.yml", "l_english:\n other: \"hi\"\n")]);
    let errs = run("mytype = {\n iname = \"absent\"\n}\n", &idx);
    assert_eq!(
        cw100s(&errs),
        0,
        "quoted+missing inline is lenient: {:?}",
        errs
    );
}

#[test]
fn synced_missing_in_a_language_warns() {
    // english + german both present; german lacks the key
    let idx = loc_index(&[
        ("a_l_english.yml", "l_english:\n my_key: \"hi\"\n"),
        ("a_l_german.yml", "l_german:\n other: \"hallo\"\n"),
    ]);
    let errs = run("mytype = {\n sname = my_key\n}\n", &idx);
    assert_eq!(
        cw100s(&errs),
        1,
        "synced key missing in german → one CW100: {:?}",
        errs
    );
}

#[test]
fn embedded_inline_command_is_skipped() {
    // A loc value with an inline `[...]` command plus a literal suffix is a
    // dynamic, runtime-substituted string (e.g. a meta_effect variable
    // `"[?ROOT.current_party_ideology_group.GetTokenKey]_subtype"`), not a literal
    // loc key. It must not warn CW100 (cwtools-vscode#25).
    let idx = loc_index(&[("a_l_english.yml", "l_english:\n other: \"hi\"\n")]);
    let errs = run(
        "mytype = {\n name = \"[GetIdeologyToken]_subtype\"\n}\n",
        &idx,
    );
    assert_eq!(
        cw100s(&errs),
        0,
        "embedded [..] command is dynamic, not a key: {:?}",
        errs
    );
}

#[test]
fn dollar_var_reference_is_skipped() {
    let idx = loc_index(&[("a_l_english.yml", "l_english:\n other: \"hi\"\n")]);
    let errs = run("mytype = {\n name = \"$SOME_VAR$\"\n}\n", &idx);
    assert_eq!(cw100s(&errs), 0, "$VAR$ refs are not key refs: {:?}", errs);
}

/// The numeric codes and severities the localization pipeline emits for the
/// scope-independent loc-entry checks must match the validation crate's catalog.
#[test]
fn loc_pipeline_codes_match_error_catalog() {
    use cwtools_error_codes as ec;
    use cwtools_localization::{LocErrorKind, loc_error_code, loc_error_severity};

    let cases = [
        (
            LocErrorKind::UndefinedLocReference {
                other_key: "x".into(),
            },
            &ec::CW225_UNDEFINED_LOC_REFERENCE,
        ),
        (LocErrorKind::RecursiveLocRef, &ec::CW259_RECURSIVE_LOC_REF),
        (LocErrorKind::ReplaceMe, &ec::CW234_REPLACE_ME_LOC),
        (LocErrorKind::LocMissingQuote, &ec::CW268_LOC_MISSING_QUOTE),
        (LocErrorKind::LocInvalidChars, &ec::CW275_LOC_INVALID_CHARS),
        (
            LocErrorKind::LocKeyInvalidChars,
            &ec::CW276_LOC_KEY_INVALID_CHARS,
        ),
    ];

    for (kind, code) in cases {
        assert_eq!(
            loc_error_code(&kind),
            code.id,
            "code id mismatch for {kind:?}"
        );
        assert_eq!(
            loc_error_severity(&kind),
            code.severity,
            "severity mismatch for {kind:?}"
        );
    }
}

#[test]
fn overlay_key_resolves_missing_loc() {
    // Regression for cwtools-vscode#36: a loc key absent from the scanned index
    // but present in the live overlay (just typed into an open `.yml`) must NOT
    // warn CW100, so adding a key clears the diagnostic without a full rescan.
    use cwtools_validation::{Prepared, validate_prepared};
    use std::collections::HashSet;

    let table = StringTable::new();
    let parsed_cwt = parse_string(CWT, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);
    let parsed = parse_string("mytype = {\n name = just_added_key\n}\n", &table);
    // Index lacks the key.
    let idx = loc_index(&[("a_l_english.yml", "l_english:\n other: \"hi\"\n")]);

    let base = Prepared {
        ruleset: &ruleset,
        table: &table,
        game: None,
        type_index: None,
        modifier_keys: None,
        loc_index: Some(&idx),
        extra_loc_keys: None,
        inline_scripts: None,
        registry: None,
        scope_checks: false,
        var_checks: false,
    };
    // No overlay → the key is missing → CW100.
    let errs = validate_prepared(&parsed, "test.txt", &base);
    assert_eq!(
        cw100s(&errs),
        1,
        "missing key without overlay → CW100: {:?}",
        errs
    );

    // Overlay carries the (lowercased) key → resolved, no CW100.
    let mut overlay = HashSet::new();
    overlay.insert("just_added_key".to_string());
    let with_overlay = Prepared {
        extra_loc_keys: Some(&overlay),
        ..base
    };
    let errs = validate_prepared(&parsed, "test.txt", &with_overlay);
    assert_eq!(
        cw100s(&errs),
        0,
        "overlay key resolves → no CW100: {:?}",
        errs
    );
}

/// A ruleset with scopes, so the loc-command checks run, plus a `mytype` whose
/// `name` is a loc reference.
const SCOPED_CWT: &str = r#"
scopes = {
    Country = { aliases = { country } }
}
types = {
    type[mytype] = {
        path = "game/common/mytype"
    }
}
mytype = {
    name = localisation
}
"#;

/// Validate a `mytype` referencing `key`, with `variables` as the project's
/// defined script variables. Returns the emitted codes.
fn scoped_loc_codes(loc: &str, key: &str, variables: &[&str]) -> Vec<String> {
    use cwtools_validation::{Prepared, build_scope_registry_arc, validate_prepared};

    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(SCOPED_CWT, &table), &table);
    let idx = loc_index(&[("a_l_english.yml", loc)]);
    let mut type_index = cwtools_index::TypeIndex::new();
    for v in variables {
        type_index.var_index.add_name(v);
    }
    let script = format!("mytype = {{\n name = {key}\n}}\n");
    let parsed = parse_string(&script, &table);
    let registry = build_scope_registry_arc(&ruleset, Some(cwtools_game::constants::Game::Hoi4));
    validate_prepared(
        &parsed,
        "game/common/mytype/test.txt",
        &Prepared {
            ruleset: &ruleset,
            table: &table,
            game: Some(cwtools_game::constants::Game::Hoi4),
            type_index: Some(&type_index),
            modifier_keys: None,
            loc_index: Some(&idx),
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: true,
            var_checks: true,
        },
    )
    .into_iter()
    .filter_map(|e| e.code.map(String::from))
    .collect()
}

#[test]
fn chain_reading_a_defined_variable_is_clean() {
    let codes = scoped_loc_codes(
        "l_english:\n my_key: \"[?ROOT.war_support|1]\"\n",
        "my_key",
        &["war_support"],
    );
    assert!(
        !codes.contains(&"CW226".to_string()),
        "a chain reading a defined variable must not warn: {codes:?}"
    );
}

#[test]
fn chain_reading_an_undefined_variable_warns_cw226() {
    let codes = scoped_loc_codes(
        "l_english:\n my_key: \"[?ROOT.war_suport|1]\"\n",
        "my_key",
        &["war_support"],
    );
    assert!(
        codes.contains(&"CW226".to_string()),
        "a misspelt variable must warn CW226: {codes:?}"
    );
}

#[test]
fn chain_reading_a_variable_without_an_index_is_lenient() {
    // No variables collected yet (an unscanned workspace): the registry can
    // vouch for nothing, so the chain stays exempt rather than warning.
    let codes = scoped_loc_codes(
        "l_english:\n my_key: \"[?ROOT.war_suport|1]\"\n",
        "my_key",
        &[],
    );
    assert!(
        !codes.contains(&"CW226".to_string()),
        "without a variable index the chain is exempt: {codes:?}"
    );
}

#[test]
fn no_loc_index_is_lenient() {
    let table = StringTable::new();
    let parsed_cwt = parse_string(CWT, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);
    let parsed = parse_string("mytype = {\n name = absent_key\n}\n", &table);
    let errs = validate_ast_with_loc(
        &parsed, &ruleset, &table, "test.txt", None, None, None, None,
    );
    assert_eq!(cw100s(&errs), 0, "no loc loaded → accept: {:?}", errs);
}

const SCOPED_WITH_CMDS_CWT: &str = r#"
scopes = { Country = { aliases = { country } } }
localisation_commands = { GetName = any GetTag = any }
types = { type[mytype] = { path = "game/common/mytype" } }
mytype = { name = localisation }
"#;

fn scoped_with_cmds_loc_codes(loc: &str, scripted_locs: &[&str]) -> Vec<String> {
    use cwtools_index::{SourceLocation, TypeInstance};
    use cwtools_validation::{Prepared, build_scope_registry_arc, validate_prepared};
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(SCOPED_WITH_CMDS_CWT, &table), &table);
    let idx = loc_index(&[("a_l_english.yml", loc)]);
    let mut type_index = cwtools_index::TypeIndex::new();
    for sl in scripted_locs {
        let mut map = std::collections::HashMap::new();
        map.insert(
            "scripted_loc".to_string(),
            vec![TypeInstance {
                name: sl.to_string(),
                location: SourceLocation {
                    line: 1,
                    col: 0,
                    end: (1, 0),
                },
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            }],
        );
        type_index.merge("game/common/scripted_localisation/defs.txt", map);
    }
    let parsed = parse_string("mytype = {\n name = my_key\n}\n", &table);
    let registry = build_scope_registry_arc(&ruleset, Some(cwtools_game::constants::Game::Hoi4));
    validate_prepared(
        &parsed,
        "game/common/mytype/test.txt",
        &Prepared {
            ruleset: &ruleset,
            table: &table,
            game: Some(cwtools_game::constants::Game::Hoi4),
            type_index: Some(&type_index),
            modifier_keys: None,
            loc_index: Some(&idx),
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: true,
            var_checks: true,
        },
    )
    .into_iter()
    .filter_map(|e| e.code.map(String::from))
    .collect()
}

/// As above, but the scripted localisations arrive the way a real run gets them:
/// collected from a `common/scripted_localisation` file, not injected as
/// instances of a ruleset type. `SCOPED_WITH_CMDS_CWT` declares no
/// `type[scripted_loc]` at all — exactly like the HOI4 config, whose one
/// declaration points at a folder that does not exist there (#348).
fn loc_codes_with_scripted_loc_file(loc: &str, defs: &str) -> Vec<String> {
    use cwtools_validation::{Prepared, build_scope_registry_arc, validate_prepared};
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(SCOPED_WITH_CMDS_CWT, &table), &table);
    let idx = loc_index(&[("a_l_english.yml", loc)]);
    let mut type_index = cwtools_index::TypeIndex::new();
    let defs_path = "game/common/scripted_localisation/00_defs.txt";
    type_index.scripted_loc_index.merge_file(
        defs_path,
        cwtools_index::collect_scripted_loc_names(&parse_string(defs, &table), defs_path, &table),
    );
    let parsed = parse_string("mytype = {\n name = my_key\n}\n", &table);
    let registry = build_scope_registry_arc(&ruleset, Some(cwtools_game::constants::Game::Hoi4));
    validate_prepared(
        &parsed,
        "game/common/mytype/test.txt",
        &Prepared {
            ruleset: &ruleset,
            table: &table,
            game: Some(cwtools_game::constants::Game::Hoi4),
            type_index: Some(&type_index),
            modifier_keys: None,
            loc_index: Some(&idx),
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: true,
            var_checks: true,
        },
    )
    .into_iter()
    .filter_map(|e| e.code.map(String::from))
    .collect()
}

const DEFINED_TEXT: &str = r#"
defined_text = {
	name = Western_Autocracy_L
	text = { localization_key = a_key }
}
"#;

#[test]
fn scripted_loc_from_its_folder_clears_the_chain_command() {
    let codes = loc_codes_with_scripted_loc_file(
        "l_english:\n my_key: \"[ROOT.Western_Autocracy_L]\"\n",
        DEFINED_TEXT,
    );
    assert!(
        !codes.contains(&"CW226".to_string()),
        "a defined_text used in a chain must not warn: {codes:?}"
    );
}

#[test]
fn scripted_loc_from_its_folder_clears_the_bare_command() {
    let codes = loc_codes_with_scripted_loc_file(
        "l_english:\n my_key: \"[Western_Autocracy_L]\"\n",
        DEFINED_TEXT,
    );
    assert!(
        !codes.contains(&"CW266".to_string()),
        "a defined_text used bare must not warn: {codes:?}"
    );
}

#[test]
fn a_typo_beside_a_known_scripted_loc_still_warns() {
    let chain = loc_codes_with_scripted_loc_file(
        "l_english:\n my_key: \"[ROOT.Western_Autocracy_Typo]\"\n",
        DEFINED_TEXT,
    );
    assert!(
        chain.contains(&"CW226".to_string()),
        "the check stays alive for real mistakes: {chain:?}"
    );
    let bare = loc_codes_with_scripted_loc_file(
        "l_english:\n my_key: \"[Western_Autocracy_Typo]\"\n",
        DEFINED_TEXT,
    );
    assert!(
        bare.contains(&"CW266".to_string()),
        "and on the bare path too: {bare:?}"
    );
}

fn loc_codes_with_scripted_gui_file(loc: &str, defs: Option<&str>) -> Vec<String> {
    use cwtools_validation::{Prepared, build_scope_registry_arc, validate_prepared};
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(SCOPED_WITH_CMDS_CWT, &table), &table);
    let idx = loc_index(&[("a_l_english.yml", loc)]);
    let mut type_index = cwtools_index::TypeIndex::new();
    if let Some(defs) = defs {
        let defs_path = "game/common/scripted_guis/00_defs.txt";
        type_index.scripted_gui_index.merge_file(
            defs_path,
            cwtools_index::collect_scripted_gui_callback_names(
                &parse_string(defs, &table),
                defs_path,
                &table,
            ),
        );
    }
    let parsed = parse_string("mytype = {\n name = my_key\n}\n", &table);
    let registry = build_scope_registry_arc(&ruleset, Some(cwtools_game::constants::Game::Hoi4));
    validate_prepared(
        &parsed,
        "game/common/mytype/test.txt",
        &Prepared {
            ruleset: &ruleset,
            table: &table,
            game: Some(cwtools_game::constants::Game::Hoi4),
            type_index: Some(&type_index),
            modifier_keys: None,
            loc_index: Some(&idx),
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: true,
            var_checks: true,
        },
    )
    .into_iter()
    .filter_map(|error| error.code.map(String::from))
    .collect()
}

const SCRIPTED_GUI: &str = r#"
scripted_gui = {
    topbar = {
        effects = { Topbar_Icon_Click = { } }
    }
}
"#;

#[test]
fn scripted_gui_callback_from_its_folder_is_clean_case_insensitively() {
    let codes = loc_codes_with_scripted_gui_file(
        "l_english:\n my_key: \"[!topbar_icon_click]\"\n",
        Some(SCRIPTED_GUI),
    );
    assert!(codes.is_empty(), "known callback must be clean: {codes:?}");
}

#[test]
fn unknown_scripted_gui_callback_only_reports_cw283() {
    for command in ["!missing_click", "Root.!missing_click"] {
        let loc = format!("l_english:\n my_key: \"[{command}]\"\n");
        let codes = loc_codes_with_scripted_gui_file(&loc, Some(SCRIPTED_GUI));
        assert_eq!(codes, vec!["CW283"], "unexpected codes for {command}");
    }
}

#[test]
fn scripted_gui_callback_without_registry_is_lenient() {
    let codes =
        loc_codes_with_scripted_gui_file("l_english:\n my_key: \"[!missing_click]\"\n", None);
    assert!(codes.is_empty(), "absent registry stays lenient: {codes:?}");
}

#[test]
fn command_chain_with_terminal_is_clean() {
    let codes = scoped_with_cmds_loc_codes("l_english:\n my_key: \"[ROOT.GetName]\"\n", &[]);
    assert!(
        !codes.contains(&"CW226".to_string()),
        "terminal command tail must not warn: {codes:?}"
    );
}

#[test]
fn command_chain_typo_warns_cw226() {
    let codes = scoped_with_cmds_loc_codes(
        "l_english:\n my_key: \"[ROOT.TotallyUnknown]\"\n",
        &["AST_GetNavyName"],
    );
    assert!(
        codes.contains(&"CW226".to_string()),
        "typo command tail must warn CW226: {codes:?}"
    );
}

/// The project defines no scripted localisation the index can see, so an unknown
/// tail could be one it missed. Judging it a typo is what flagged every HOI4
/// `defined_text` (#348).
#[test]
fn command_chain_typo_is_lenient_without_scripted_locs() {
    let codes = scoped_with_cmds_loc_codes("l_english:\n my_key: \"[ROOT.TotallyUnknown]\"\n", &[]);
    assert!(
        !codes.contains(&"CW226".to_string()),
        "no scripted-loc data must leave the tail unjudged: {codes:?}"
    );
}

#[test]
fn command_chain_with_scripted_loc_is_clean() {
    let codes = scoped_with_cmds_loc_codes(
        "l_english:\n my_key: \"[ROOT.AST_GetNavyName]\"\n",
        &["AST_GetNavyName"],
    );
    assert!(
        !codes.contains(&"CW226".to_string()),
        "scripted_loc tail must not warn when index has it: {codes:?}"
    );
}

#[test]
fn command_chain_scripted_loc_typo_warns_when_index_populated() {
    let codes = scoped_with_cmds_loc_codes(
        "l_english:\n my_key: \"[ROOT.AST_Typo]\"\n",
        &["AST_GetNavyName"],
    );
    assert!(
        codes.contains(&"CW226".to_string()),
        "scripted_loc typo must warn when index populated: {codes:?}"
    );
}

// ── Issue #306: vanilla variables in loc chains ─────────────────────────────
fn scoped_loc_codes_with_vanilla(loc: &str, key: &str, vanilla_vars: &[&str]) -> Vec<String> {
    use cwtools_validation::{Prepared, build_scope_registry_arc, validate_prepared};
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(SCOPED_CWT, &table), &table);
    let idx = loc_index(&[("a_l_english.yml", loc)]);
    let mut type_index = cwtools_index::TypeIndex::new();
    type_index
        .var_index
        .set_vanilla_names(vanilla_vars.iter().map(|s| s.to_string()).collect());
    let script = format!("mytype = {{\n name = {key}\n}}\n");
    let parsed = parse_string(&script, &table);
    let registry = build_scope_registry_arc(&ruleset, Some(cwtools_game::constants::Game::Hoi4));
    validate_prepared(
        &parsed,
        "game/common/mytype/test.txt",
        &Prepared {
            ruleset: &ruleset,
            table: &table,
            game: Some(cwtools_game::constants::Game::Hoi4),
            type_index: Some(&type_index),
            modifier_keys: None,
            loc_index: Some(&idx),
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: true,
            var_checks: true,
        },
    )
    .into_iter()
    .filter_map(|e| e.code.map(String::from))
    .collect()
}

#[test]
fn chain_reading_vanilla_variable_is_clean() {
    let codes = scoped_loc_codes_with_vanilla(
        "l_english:\n my_key: \"[?ROOT.vanilla_morale|1]\"\n",
        "my_key",
        &["vanilla_morale"],
    );
    assert!(
        !codes.contains(&"CW226".to_string()),
        "vanilla var in chain must not warn: {codes:?}"
    );
}

#[test]
fn chain_reading_undefined_still_flags_with_other_vanilla() {
    let codes = scoped_loc_codes_with_vanilla(
        "l_english:\n my_key: \"[?ROOT.mystery_var|1]\"\n",
        "my_key",
        &["other_vanilla"],
    );
    assert!(
        codes.contains(&"CW226".to_string()),
        "undefined var must still flag even with other vanilla present: {codes:?}"
    );
}

#[test]
fn chain_reading_vanilla_case_insensitive_is_clean() {
    let codes = scoped_loc_codes_with_vanilla(
        "l_english:\n my_key: \"[?ROOT.VANILLA_MORALE|1]\"\n",
        "my_key",
        &["vanilla_morale"],
    );
    assert!(
        !codes.contains(&"CW226".to_string()),
        "vanilla case-insensitive must not warn: {codes:?}"
    );
}

#[test]
fn chain_with_empty_vanilla_is_lenient() {
    let codes = scoped_loc_codes_with_vanilla(
        "l_english:\n my_key: \"[?ROOT.mystery_var|1]\"\n",
        "my_key",
        &[],
    );
    assert!(
        !codes.contains(&"CW226".to_string()),
        "empty var_index gates lenient, must not warn: {codes:?}"
    );
}

#[test]
fn chain_reading_vanilla_cross_form_is_clean() {
    let codes = scoped_loc_codes_with_vanilla(
        "l_english:\n my_key: \"[?ROOT.my_var|1]\"\n",
        "my_key",
        &["My_Var@GER"],
    );
    assert!(
        !codes.contains(&"CW226".to_string()),
        "vanilla My_Var@GER must resolve as my_var: {codes:?}"
    );
}
