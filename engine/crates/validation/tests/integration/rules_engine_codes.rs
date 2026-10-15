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

use super::common::{Opts, codes, validate};

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
    let errors = validate(RULES, "foo = { name = x }", &Opts::DEFAULT);
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
    let codes = codes(
        RULES,
        r#"
foo = {
    required_field = ok
    bogus_leaf = 3
}
"#,
        &Opts::DEFAULT,
    );
    assert!(codes.contains(&"CW263".to_string()), "got: {:?}", codes);
    assert!(!codes.iter().any(|c| c == "CW201"), "CW201 retired");
}

#[test]
fn unexpected_node_is_cw262() {
    let codes = codes(
        RULES,
        r#"
foo = {
    required_field = ok
    bogus_block = { x = 1 }
}
"#,
        &Opts::DEFAULT,
    );
    assert!(codes.contains(&"CW262".to_string()), "got: {:?}", codes);
}

#[test]
fn wrong_value_type_is_cw240() {
    let codes = codes(
        RULES,
        r#"
foo = {
    required_field = ok
    count = notaninteger
}
"#,
        &Opts::DEFAULT,
    );
    assert!(codes.contains(&"CW240".to_string()), "got: {:?}", codes);
    assert!(!codes.iter().any(|c| c == "CW202"), "CW202 retired");
}

#[test]
fn missing_required_is_cw242() {
    // `required_field` has `## cardinality = 1..1` and is omitted.
    let codes = codes(
        RULES,
        r#"
foo = {
    name = something
}
"#,
        &Opts::DEFAULT,
    );
    assert!(codes.contains(&"CW242".to_string()), "got: {:?}", codes);
    assert!(
        !codes.iter().any(|c| c == "CW203" || c == "CW204"),
        "CW203/CW204 retired"
    );
}

#[test]
fn unexpected_value_clause_is_cw265() {
    let codes = codes(
        RULES,
        r#"
foo = {
    required_field = ok
    { x = 1 }
}
"#,
        &Opts::DEFAULT,
    );
    assert!(codes.contains(&"CW265".to_string()), "got: {:?}", codes);
}

const VALUE_CLAUSE_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    {
        id = int
    }
}
"#;

#[test]
fn allowed_value_clause_is_not_cw265() {
    let codes = codes(VALUE_CLAUSE_RULES, "foo = { { id = 1 } }", &Opts::DEFAULT);
    assert!(codes.is_empty(), "got: {:?}", codes);
}

#[test]
fn value_clause_children_are_validated() {
    let codes = codes(
        VALUE_CLAUSE_RULES,
        "foo = { { id = notaninteger } }",
        &Opts::DEFAULT,
    );
    assert!(codes.contains(&"CW240".to_string()), "got: {:?}", codes);
    assert!(!codes.contains(&"CW265".to_string()), "got: {:?}", codes);
}

#[test]
fn too_few_value_clauses_is_cw242() {
    const RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    ## cardinality = 2..3
    {
        id = int
    }
}
"#;
    let errors = validate(RULES, "foo = { { id = 1 } }", &Opts::DEFAULT);
    let err = errors
        .iter()
        .find(|e| e.code == Some("CW242"))
        .expect("CW242 emitted");
    assert_eq!(
        err.message,
        "ValueClause appears 1 time(s), expected at least 2"
    );
}

#[test]
fn too_many_value_clauses_is_cw242() {
    const RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    ## cardinality = 0..2
    {
        id = int
    }
}
"#;
    let errors = validate(
        RULES,
        "foo = { { id = 1 } { id = 2 } { id = 3 } }",
        &Opts::DEFAULT,
    );
    let err = errors
        .iter()
        .find(|e| e.code == Some("CW242"))
        .expect("CW242 emitted");
    assert_eq!(
        err.message,
        "ValueClause appears 3 time(s), expected at most 2"
    );
}

// ── Did-you-mean suggested fixes (Task 19): fix metadata only ─────────────────
// The suggestion lives in the fix title/edit; the diagnostic message, code, and
// position are unchanged (corpus-inert).

#[test]
fn cw263_close_match_attaches_did_you_mean_fix() {
    use cwtools_parser::fix::apply_edits;
    let script = "foo = {\n    required_field = ok\n    cont = 3\n}\n";
    let errors = validate(RULES, script, &Opts::DEFAULT);

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
    let errors2 = validate(RULES, &fixed, &Opts::DEFAULT);
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
    let errors = validate(ENUM_RULES, script, &Opts::DEFAULT);

    let err = errors
        .iter()
        .find(|e| e.code == Some("CW240"))
        .expect("CW240 emitted");
    let fix = err.fix.as_ref().expect("CW240 carries a did-you-mean fix");
    assert_eq!(fix.title, "Did you mean 'historic'?");

    let fixed = apply_edits(script, &fix.edits);
    assert_eq!(fixed, "foo = {\n    mode = \"historic\"  # typo\n}\n");

    let errors2 = validate(ENUM_RULES, &fixed, &Opts::DEFAULT);
    assert!(
        !errors2.iter().any(|e| e.code == Some("CW240")),
        "CW240 must be gone after applying the fix"
    );
}

#[test]
fn cw240_quoted_enum_value_replaces_the_quotes_too() {
    use cwtools_parser::fix::apply_edits;

    let script = "foo = { mode = \"histroic\" }";
    let errors = validate(ENUM_RULES, script, &Opts::DEFAULT);
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
    let errors = validate(SPACED_ENUM_RULES, script, &Opts::DEFAULT);
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
    let errors = validate(BOOLEAN_ENUM_RULES, script, &Opts::DEFAULT);
    let err = errors
        .iter()
        .find(|e| e.code == Some("CW240"))
        .expect("CW240 emitted");
    let fix = err.fix.as_ref().expect("CW240 carries a did-you-mean fix");

    let fixed = apply_edits(script, &fix.edits);
    assert_eq!(fixed, "foo = { mode = \"yes\" }");
    assert!(
        !validate(BOOLEAN_ENUM_RULES, &fixed, &Opts::DEFAULT)
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
    let errors = validate(ESCAPED_ENUM_RULES, script, &Opts::DEFAULT);
    let err = errors
        .iter()
        .find(|e| e.code == Some("CW240"))
        .expect("CW240 emitted");
    let fix = err.fix.as_ref().expect("CW240 carries a did-you-mean fix");

    assert_eq!(fix.title, "Did you mean 'hist\"oric\\mode'?");
    let fixed = apply_edits(script, &fix.edits);
    assert_eq!(fixed, r#"foo = { mode = "hist\"oric\\mode" }"#);
    assert!(
        !validate(ESCAPED_ENUM_RULES, &fixed, &Opts::DEFAULT)
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
    let errors = validate(ENUM_TIE_RULES, "foo = { mode = dask }", &Opts::DEFAULT);
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
    let errors = validate(NODE_RULES, script, &Opts::DEFAULT);

    let err = errors
        .iter()
        .find(|e| e.code == Some("CW262"))
        .expect("CW262 emitted");
    assert_eq!(err.message, "Unexpected block 'setings'");
    let fix = err.fix.as_ref().expect("CW262 carries a did-you-mean fix");
    assert_eq!(fix.title, "Did you mean 'settings'?");

    let fixed = apply_edits(script, &fix.edits);
    assert_eq!(fixed, "foo = {\n    settings = { x = 1 }\n}\n");

    let errors2 = validate(NODE_RULES, &fixed, &Opts::DEFAULT);
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
    let errors = validate(NODE_RULES, script, &Opts::DEFAULT);
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
    let errors = validate(RULES, script, &Opts::DEFAULT);
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
    let errors = validate(TIE_RULES, script, &Opts::DEFAULT);
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
    let errors = validate(TINY_RULES, script, &Opts::DEFAULT);
    let err = errors
        .iter()
        .find(|e| e.code == Some("CW263"))
        .expect("CW263 emitted");
    assert!(err.fix.is_none(), "candidate shorter than 3 chars → no fix");
}
