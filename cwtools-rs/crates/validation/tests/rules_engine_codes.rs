//! Tier R reconciliation: the rules engine must emit F#'s node-kind-specific
//! codes for structural/value/cardinality mismatches, not the Rust-invented
//! CW200-205.
//!
//! Mapping (F# CWTools/Rules/RuleValidationService.fs + FieldValidators.fs):
//! - unexpected `key = {...}` node      -> CW262
//! - unexpected `key = value` leaf      -> CW263
//! - unexpected bare value / leafvalue  -> CW264
//! - unexpected `{...}` value clause    -> CW265
//! - cardinality (too few / too many)   -> CW242
//! - wrong value type                   -> CW240

use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::validate_ast;

fn codes_for(cwt: &str, script: &str) -> Vec<String> {
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);
    let parsed = parse_string(script, &table);
    let errors = validate_ast(&parsed, &ruleset, &table, "test.txt", None, None, None);
    errors
        .into_iter()
        .filter_map(|e| e.code.map(String::from))
        .collect()
}

/// A required field carrying `## severity = error` must report its missing-field
/// cardinality (CW242) at Error severity, not the default Warning. Mirrors the
/// HOI4 config's `## severity = error` on `if`/`else_if`'s `limit`.
#[test]
fn missing_required_honors_severity_error() {
    use cwtools_validation::ErrorSeverity;
    const RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    name = scalar
    ## severity = error
    needed = scalar
}
"#;
    let table = StringTable::new();
    let parsed_cwt = parse_string(RULES, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);
    let parsed = parse_string("foo = { name = x }", &table);
    let errors = validate_ast(&parsed, &ruleset, &table, "test.txt", None, None, None);
    let cw242 = errors
        .iter()
        .find(|e| e.code == Some("CW242"))
        .expect("CW242 for missing required field");
    assert_eq!(
        cw242.severity,
        ErrorSeverity::Error,
        "missing required field with ## severity = error must be Error, got {:?}",
        cw242.severity
    );
}

const RULES: &str = r#"
types = {
    type[foo] = {
        path = "game/common/foo"
    }
}
foo = {
    name = scalar
    count = int
    ## cardinality = 1..1
    required_field = scalar
}
"#;

#[test]
fn unexpected_leaf_is_cw263() {
    let codes = codes_for(
        RULES,
        r#"
foo = {
    required_field = ok
    bogus_leaf = 3
}
"#,
    );
    assert!(codes.contains(&"CW263".to_string()), "got: {:?}", codes);
    assert!(!codes.iter().any(|c| c == "CW201"), "CW201 retired");
}

#[test]
fn unexpected_node_is_cw262() {
    let codes = codes_for(
        RULES,
        r#"
foo = {
    required_field = ok
    bogus_block = { x = 1 }
}
"#,
    );
    assert!(codes.contains(&"CW262".to_string()), "got: {:?}", codes);
}

#[test]
fn wrong_value_type_is_cw240() {
    let codes = codes_for(
        RULES,
        r#"
foo = {
    required_field = ok
    count = notaninteger
}
"#,
    );
    assert!(codes.contains(&"CW240".to_string()), "got: {:?}", codes);
    assert!(!codes.iter().any(|c| c == "CW202"), "CW202 retired");
}

#[test]
fn missing_required_is_cw242() {
    // `required_field` has `## cardinality = 1..1` and is omitted.
    let codes = codes_for(
        RULES,
        r#"
foo = {
    name = something
}
"#,
    );
    assert!(codes.contains(&"CW242".to_string()), "got: {:?}", codes);
    assert!(
        !codes.iter().any(|c| c == "CW203" || c == "CW204"),
        "CW203/CW204 retired"
    );
}

// ── Did-you-mean suggested fixes (Task 19): fix metadata only ─────────────────
// The suggestion lives in the fix title/edit; the diagnostic message, code, and
// position are unchanged (corpus-inert).

/// Full errors for a (cwt, script) pair, keeping ruleset+table alive so a fix can
/// be applied and the corrected text revalidated against the same rules.
fn validate_pair(
    cwt: &str,
    script: &str,
) -> (
    StringTable,
    cwtools_rules::rules_types::RuleSet,
    Vec<cwtools_validation::ValidationError>,
) {
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);
    let parsed = parse_string(script, &table);
    let errors = validate_ast(&parsed, &ruleset, &table, "test.txt", None, None, None);
    (table, ruleset, errors)
}

#[test]
fn cw263_close_match_attaches_did_you_mean_fix() {
    use cwtools_parser::fix::apply_edits;
    let script = "foo = {\n    required_field = ok\n    cont = 3\n}\n";
    let (table, ruleset, errors) = validate_pair(RULES, script);

    let err = errors
        .iter()
        .find(|e| e.code == Some("CW263"))
        .expect("CW263 emitted");
    // Message is UNCHANGED — the suggestion is fix metadata, not diagnostic text.
    assert_eq!(err.message, "Unexpected field 'cont'");
    let fix = err.fix.as_ref().expect("CW263 carries a did-you-mean fix");
    assert_eq!(fix.title, "Did you mean 'count'?");

    let fixed = apply_edits(script, &fix.edits);
    assert_eq!(
        fixed,
        "foo = {\n    required_field = ok\n    count = 3\n}\n"
    );

    // The corrected key matches `count = int`; CW263 is gone on revalidation.
    let ast2 = parse_string(&fixed, &table);
    let errors2 = validate_ast(&ast2, &ruleset, &table, "test.txt", None, None, None);
    assert!(
        !errors2.iter().any(|e| e.code == Some("CW263")),
        "CW263 must be gone after applying the fix"
    );
}

const ENUM_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
enums = { enum[mode] = { historic fantasy sandbox } }
foo = { mode = enum[mode] }
"#;

#[test]
fn cw240_enum_close_match_attaches_did_you_mean_fix() {
    use cwtools_parser::fix::apply_edits;
    let script = "foo = {\n    mode = histroic  # typo\n}\n";
    let (table, ruleset, errors) = validate_pair(ENUM_RULES, script);

    let err = errors
        .iter()
        .find(|e| e.code == Some("CW240"))
        .expect("CW240 emitted");
    let fix = err.fix.as_ref().expect("CW240 carries a did-you-mean fix");
    assert_eq!(fix.title, "Did you mean 'historic'?");

    let fixed = apply_edits(script, &fix.edits);
    assert_eq!(fixed, "foo = {\n    mode = \"historic\"  # typo\n}\n");

    let ast2 = parse_string(&fixed, &table);
    let errors2 = validate_ast(&ast2, &ruleset, &table, "test.txt", None, None, None);
    assert!(
        !errors2.iter().any(|e| e.code == Some("CW240")),
        "CW240 must be gone after applying the fix"
    );
}

#[test]
fn cw240_quoted_enum_value_replaces_the_quotes_too() {
    use cwtools_parser::fix::apply_edits;

    let script = "foo = { mode = \"histroic\" }";
    let (_table, _ruleset, errors) = validate_pair(ENUM_RULES, script);
    let err = errors
        .iter()
        .find(|e| e.code == Some("CW240"))
        .expect("CW240 emitted");
    let fix = err.fix.as_ref().expect("CW240 carries a did-you-mean fix");

    assert_eq!(
        apply_edits(script, &fix.edits),
        "foo = { mode = \"historic\" }"
    );
}

const SPACED_ENUM_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
enums = { enum[mode] = { "historic mode" fantasy sandbox } }
foo = { mode = enum[mode] }
"#;

#[test]
fn cw240_spaced_enum_value_keeps_its_quotes() {
    use cwtools_parser::fix::apply_edits;

    let script = "foo = { mode = \"histroic mode\" }";
    let (_table, _ruleset, errors) = validate_pair(SPACED_ENUM_RULES, script);
    let err = errors
        .iter()
        .find(|e| e.code == Some("CW240"))
        .expect("CW240 emitted");
    let fix = err.fix.as_ref().expect("CW240 carries a did-you-mean fix");

    assert_eq!(
        apply_edits(script, &fix.edits),
        "foo = { mode = \"historic mode\" }"
    );
}

const BOOLEAN_ENUM_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
enums = { enum[mode] = { yes fantasy sandbox } }
foo = { mode = enum[mode] }
"#;

#[test]
fn cw240_boolean_enum_suggestion_stays_a_string() {
    use cwtools_parser::fix::apply_edits;

    let script = "foo = { mode = yess }";
    let (table, ruleset, errors) = validate_pair(BOOLEAN_ENUM_RULES, script);
    let err = errors
        .iter()
        .find(|e| e.code == Some("CW240"))
        .expect("CW240 emitted");
    let fix = err.fix.as_ref().expect("CW240 carries a did-you-mean fix");

    let fixed = apply_edits(script, &fix.edits);
    assert_eq!(fixed, "foo = { mode = \"yes\" }");
    let ast = parse_string(&fixed, &table);
    assert!(
        !validate_ast(&ast, &ruleset, &table, "test.txt", None, None, None)
            .iter()
            .any(|e| e.code == Some("CW240")),
        "quoted boolean enum member must revalidate cleanly"
    );
}

const ESCAPED_ENUM_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
enums = { enum[mode] = { "hist\"oric\\mode" fantasy sandbox } }
foo = { mode = enum[mode] }
"#;

#[test]
fn cw240_enum_suggestion_escapes_quotes_and_backslashes() {
    use cwtools_parser::fix::apply_edits;

    let script = r#"foo = { mode = "histr\"oric\\mode" }"#;
    let (table, ruleset, errors) = validate_pair(ESCAPED_ENUM_RULES, script);
    let err = errors
        .iter()
        .find(|e| e.code == Some("CW240"))
        .expect("CW240 emitted");
    let fix = err.fix.as_ref().expect("CW240 carries a did-you-mean fix");

    assert_eq!(fix.title, "Did you mean 'hist\"oric\\mode'?");
    let fixed = apply_edits(script, &fix.edits);
    assert_eq!(fixed, r#"foo = { mode = "hist\"oric\\mode" }"#);
    let ast = parse_string(&fixed, &table);
    assert!(
        !validate_ast(&ast, &ruleset, &table, "test.txt", None, None, None)
            .iter()
            .any(|e| e.code == Some("CW240")),
        "escaped enum member must revalidate cleanly"
    );
}

const ENUM_TIE_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
enums = { enum[mode] = { dark dusk } }
foo = { mode = enum[mode] }
"#;

#[test]
fn cw240_enum_ambiguous_match_has_no_fix() {
    let (_t, _r, errors) = validate_pair(ENUM_TIE_RULES, "foo = { mode = dask }");
    let err = errors
        .iter()
        .find(|e| e.code == Some("CW240"))
        .expect("CW240 emitted");
    assert!(
        err.fix.is_none(),
        "ambiguous suggestion must not carry a fix"
    );
}

const NODE_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    settings = { x = int }
    name = scalar
}
"#;

#[test]
fn cw262_close_match_attaches_did_you_mean_fix() {
    use cwtools_parser::fix::apply_edits;
    let script = "foo = {\n    setings = { x = 1 }\n}\n";
    let (table, ruleset, errors) = validate_pair(NODE_RULES, script);

    let err = errors
        .iter()
        .find(|e| e.code == Some("CW262"))
        .expect("CW262 emitted");
    assert_eq!(err.message, "Unexpected block 'setings'");
    let fix = err.fix.as_ref().expect("CW262 carries a did-you-mean fix");
    assert_eq!(fix.title, "Did you mean 'settings'?");

    let fixed = apply_edits(script, &fix.edits);
    assert_eq!(fixed, "foo = {\n    settings = { x = 1 }\n}\n");

    let ast2 = parse_string(&fixed, &table);
    let errors2 = validate_ast(&ast2, &ruleset, &table, "test.txt", None, None, None);
    assert!(
        !errors2.iter().any(|e| e.code == Some("CW262")),
        "CW262 must be gone after applying the fix"
    );
}

// The complaint is the key, and the did-you-mean edit renames only the key
// token. The squiggle must agree with it instead of covering the whole block
// the unexpected key opens.
#[test]
fn cw262_underlines_only_the_unexpected_key() {
    let script = "foo = {\n    setings = {\n        x = 1\n    }\n}\n";
    let (_t, _r, errors) = validate_pair(NODE_RULES, script);
    let err = errors
        .iter()
        .find(|e| e.code == Some("CW262"))
        .expect("CW262 emitted");
    assert_eq!((err.line, err.col), (2, 4));
    assert_eq!(
        err.end,
        Some((2, 4 + "setings".len() as u16)),
        "CW262 must span only the key"
    );
}

#[test]
fn cw263_no_close_match_has_no_fix() {
    let script = "foo = {\n    required_field = ok\n    xyzzy = 3\n}\n";
    let (_t, _r, errors) = validate_pair(RULES, script);
    let err = errors
        .iter()
        .find(|e| e.code == Some("CW263"))
        .expect("CW263 emitted");
    assert!(
        err.fix.is_none(),
        "no candidate within edit distance → no fix"
    );
}

const TIE_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    cat = scalar
    bat = scalar
}
"#;

#[test]
fn cw263_ambiguous_tie_has_no_fix() {
    // "rat" is distance 1 from both "cat" and "bat": ambiguous → no fix.
    let script = "foo = {\n    rat = 1\n}\n";
    let (_t, _r, errors) = validate_pair(TIE_RULES, script);
    let err = errors
        .iter()
        .find(|e| e.code == Some("CW263"))
        .expect("CW263 emitted");
    assert!(err.fix.is_none(), "equal-distance tie → no fix");
}

const TINY_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    ab = scalar
}
"#;

#[test]
fn cw263_tiny_candidate_has_no_fix() {
    // "ba" -> "ab" is distance 2, but the only candidate is 2 chars: too short.
    let script = "foo = {\n    ba = 1\n}\n";
    let (_t, _r, errors) = validate_pair(TINY_RULES, script);
    let err = errors
        .iter()
        .find(|e| e.code == Some("CW263"))
        .expect("CW263 emitted");
    assert!(err.fix.is_none(), "candidate shorter than 3 chars → no fix");
}
