//! `## error_if_only_match = <msg>` (CW272): when the directive-carrying rule is
//! the ONLY overload whose value matches, its match is turned into a custom error
//! instead of a clean accept. If another overload matches cleanly, or the
//! directive is absent, nothing changes. Mirrors F# `errorIfOnlyMatch`
//! (RuleValidationService.applyLeafRule -> FromRulesCustomError, gated in
//! lazyErrorMerge on there being no directive-free clean match).

use cwtools_game::constants::Game;
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::{ErrorSeverity, ValidationError, validate_ast};

fn validate(rules: &str, script: &str) -> Vec<ValidationError> {
    let table = StringTable::new();
    let parsed_cwt = parse_string(rules, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);
    let parsed = parse_string(script, &table);
    validate_ast(
        &parsed,
        &ruleset,
        &table,
        "game/common/foo/test.txt",
        Some(Game::Hoi4),
        None,
        None,
    )
}

fn codes(rules: &str, script: &str) -> Vec<String> {
    validate(rules, script)
        .into_iter()
        .filter_map(|e| e.code.map(String::from))
        .collect()
}

// ---- Plain overloaded key (pick_best_candidate in children.rs) ----

// `val` has two overloads: an `## error_if_only_match` scalar (matches anything)
// and a plain `int`. A non-int value matches only the directive rule; an int
// value also matches the directive-free `int` overload.
const PLAIN_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    name = scalar
    ## error_if_only_match = deprecated, use amount
    val = scalar
    val = int
}
"#;

#[test]
fn plain_only_match_emits_cw272() {
    let c = codes(PLAIN_RULES, "foo = { name = x val = hello }");
    assert!(c.contains(&"CW272".to_string()), "got: {:?}", c);
}

#[test]
fn plain_other_candidate_matches_no_cw272() {
    // `val = 5` matches the directive-free `int` overload cleanly -> accept.
    let c = codes(PLAIN_RULES, "foo = { name = x val = 5 }");
    assert!(!c.contains(&"CW272".to_string()), "got: {:?}", c);
}

#[test]
fn plain_cw272_message_is_verbatim() {
    let err = validate(PLAIN_RULES, "foo = { name = x val = hello }")
        .into_iter()
        .find(|e| e.code == Some("CW272"))
        .expect("CW272 emitted");
    assert_eq!(err.message, "deprecated, use amount");
}

#[test]
fn plain_cw272_default_severity_is_error() {
    let err = validate(PLAIN_RULES, "foo = { name = x val = hello }")
        .into_iter()
        .find(|e| e.code == Some("CW272"))
        .expect("CW272 emitted");
    assert_eq!(err.severity, ErrorSeverity::Error);
}

// `## severity = info` overrides the CW272 default of Error (the `random` /
// `set_state_owner` config sites carry `## severity = info`).
const PLAIN_SEV_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    name = scalar
    ## error_if_only_match = deprecated
    ## severity = info
    val = scalar
    val = int
}
"#;

#[test]
fn plain_severity_directive_overrides_error() {
    let err = validate(PLAIN_SEV_RULES, "foo = { name = x val = hello }")
        .into_iter()
        .find(|e| e.code == Some("CW272"))
        .expect("CW272 emitted");
    assert_eq!(err.severity, ErrorSeverity::Information);
}

const NO_DIRECTIVE_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    name = scalar
    val = scalar
    val = int
}
"#;

#[test]
fn plain_no_directive_no_cw272() {
    let c = codes(NO_DIRECTIVE_RULES, "foo = { name = x val = hello }");
    assert!(!c.contains(&"CW272".to_string()), "got: {:?}", c);
}

// ---- Alias overloads (validate_alias_usage in alias.rs) ----

// Mirrors the F# fixture: two `alias[effect:pick]` overloads, the second carrying
// the directive. Plus a directive-free `safe_fx` control.
const ALIAS_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    alias_name[effect] = alias_match_left[effect]
}
alias[effect:pick] = alpha
## error_if_only_match = Test error
alias[effect:pick] = beta

alias[effect:safe_fx] = bool
"#;

#[test]
fn alias_only_match_emits_cw272() {
    // `pick = beta` matches only the directive-carrying overload -> CW272.
    let c = codes(ALIAS_RULES, "foo = { pick = beta }");
    assert!(c.contains(&"CW272".to_string()), "got: {:?}", c);
}

#[test]
fn alias_other_overload_matches_no_cw272() {
    // `pick = alpha` matches the directive-free overload cleanly -> accept.
    let c = codes(ALIAS_RULES, "foo = { pick = alpha }");
    assert!(!c.contains(&"CW272".to_string()), "got: {:?}", c);
}

#[test]
fn alias_no_directive_no_cw272() {
    let c = codes(ALIAS_RULES, "foo = { safe_fx = yes }");
    assert!(!c.contains(&"CW272".to_string()), "got: {:?}", c);
}

#[test]
fn alias_cw272_message_is_verbatim() {
    let err = validate(ALIAS_RULES, "foo = { pick = beta }")
        .into_iter()
        .find(|e| e.code == Some("CW272"))
        .expect("CW272 emitted");
    assert_eq!(err.message, "Test error");
}
