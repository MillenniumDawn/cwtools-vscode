//! `inline_script` call sites are checked against the body they pull in.
//!
//! The body is validated against the rules and scope in force at the call site,
//! its diagnostics are reported on the call site (naming the script line they
//! came from), and CW274 stands in when the body cannot be pulled in at all.

use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::{InlineScripts, Prepared, ValidationError, validate_prepared};

const RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    ## cardinality = 1..1
    id = scalar
    ## cardinality = 0..inf
    add = int
}
"#;

const FILE: &str = "game/common/foo/test.txt";

/// Validate `script` with `scripts` registered as the mod's inline scripts.
fn run(script: &str, scripts: &[(&str, &str)]) -> Vec<ValidationError> {
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(RULES, &table), &table);
    let mut registry = InlineScripts::default();
    for (path, body) in scripts {
        assert!(
            registry.insert(path, parse_string(body, &table)),
            "{path} is not an inline-script path"
        );
    }
    let parsed = parse_string(script, &table);
    validate_prepared(
        &parsed,
        FILE,
        &Prepared {
            ruleset: &ruleset,
            table: &table,
            game: None,
            type_index: None,
            modifier_keys: None,
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: Some(&registry),
            registry: None,
            scope_checks: false,
            var_checks: false,
        },
    )
}

/// As [`run`], but with no registry at all — the LSP and single-file paths.
fn run_without_registry(script: &str) -> Vec<ValidationError> {
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(RULES, &table), &table);
    let parsed = parse_string(script, &table);
    validate_prepared(
        &parsed,
        FILE,
        &Prepared {
            ruleset: &ruleset,
            table: &table,
            game: None,
            type_index: None,
            modifier_keys: None,
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: None,
            scope_checks: false,
            var_checks: false,
        },
    )
}

fn codes(errors: &[ValidationError]) -> Vec<&str> {
    errors.iter().filter_map(|e| e.code).collect()
}

#[test]
fn a_clean_body_reports_nothing() {
    let errors = run(
        "foo = {\n    id = x\n    inline_script = { script = bonus }\n}\n",
        &[("common/inline_scripts/bonus.txt", "add = 5\n")],
    );
    assert!(errors.is_empty(), "got: {errors:?}");
}

#[test]
fn a_key_the_caller_has_no_rule_for_is_reported_on_the_call_site() {
    let errors = run(
        "foo = {\n    id = x\n    inline_script = { script = bonus }\n}\n",
        &[("common/inline_scripts/bonus.txt", "\nnot_a_field = 5\n")],
    );
    assert_eq!(codes(&errors), ["CW263"], "got: {errors:?}");
    let error = &errors[0];
    // Anchored on the call site, not on line 2 of the script.
    assert_eq!(error.line, 3, "got: {error:?}");
    assert!(
        error
            .message
            .contains("(in common/inline_scripts/bonus.txt:2)"),
        "message should name the script line: {}",
        error.message
    );
    // The body's key-rename fix edits a span in the script file, which is not
    // the file this diagnostic now lands in.
    assert!(error.fix.is_none(), "got: {error:?}");
}

#[test]
fn a_parameter_is_substituted_before_the_body_is_checked() {
    let script = "common/inline_scripts/bonus.txt";
    let body = "$WHICH$ = 5\n";
    let clean = run(
        "foo = {\n    id = x\n    inline_script = { script = bonus WHICH = add }\n}\n",
        &[(script, body)],
    );
    assert!(clean.is_empty(), "'add' is a known field, got: {clean:?}");

    let broken = run(
        "foo = {\n    id = x\n    inline_script = { script = bonus WHICH = nope }\n}\n",
        &[(script, body)],
    );
    assert_eq!(codes(&broken), ["CW263"], "got: {broken:?}");
}

#[test]
fn a_parameter_is_substituted_into_values_too() {
    let errors = run(
        "foo = {\n    id = x\n    inline_script = { script = bonus AMOUNT = nine }\n}\n",
        &[("common/inline_scripts/bonus.txt", "add = $AMOUNT$\n")],
    );
    // `int` rejects the substituted word, so the caller's rule is what decides.
    assert!(!errors.is_empty(), "expected the value to be checked");
}

#[test]
fn a_field_the_body_supplies_counts_toward_the_callers_cardinality() {
    let errors = run(
        "foo = {\n    inline_script = { script = ident }\n}\n",
        &[("common/inline_scripts/ident.txt", "id = x\n")],
    );
    assert!(
        errors.is_empty(),
        "the required `id` is supplied by the body, got: {errors:?}"
    );
}

#[test]
fn a_nested_call_is_expanded_too() {
    let errors = run(
        "foo = {\n    id = x\n    inline_script = { script = outer }\n}\n",
        &[
            (
                "common/inline_scripts/outer.txt",
                "inline_script = { script = inner }\n",
            ),
            ("common/inline_scripts/inner.txt", "not_a_field = 1\n"),
        ],
    );
    assert_eq!(codes(&errors), ["CW263"], "got: {errors:?}");
    // Innermost first: the key is in inner.txt, reached through outer.txt.
    assert_eq!(
        errors[0].message,
        "Unexpected field 'not_a_field' (in common/inline_scripts/inner.txt:1) \
         (in common/inline_scripts/outer.txt:1)"
    );
}

#[test]
fn a_script_that_does_not_exist_is_cw274() {
    let errors = run(
        "foo = {\n    id = x\n    inline_script = { script = nope }\n}\n",
        &[],
    );
    assert_eq!(codes(&errors), ["CW274"], "got: {errors:?}");
    assert_eq!(errors[0].line, 3, "got: {errors:?}");
    assert!(
        errors[0].message.contains("'nope'"),
        "got: {}",
        errors[0].message
    );
}

#[test]
fn a_call_with_no_script_field_is_cw274() {
    let errors = run(
        "foo = {\n    id = x\n    inline_script = { WHICH = add }\n}\n",
        &[],
    );
    assert_eq!(codes(&errors), ["CW274"], "got: {errors:?}");
}

#[test]
fn a_script_that_calls_itself_is_cw274() {
    let errors = run(
        "foo = {\n    id = x\n    inline_script = { script = loop }\n}\n",
        &[(
            "common/inline_scripts/loop.txt",
            "inline_script = { script = loop }\n",
        )],
    );
    assert_eq!(codes(&errors), ["CW274"], "got: {errors:?}");
    assert!(
        errors[0].message.contains("calls itself"),
        "got: {}",
        errors[0].message
    );
}

#[test]
fn a_chain_past_the_depth_limit_is_cw274() {
    // Six links, one past the limit: the sixth call is what gets reported.
    let bodies: Vec<(String, String)> = (0..6)
        .map(|i| {
            (
                format!("common/inline_scripts/s{i}.txt"),
                format!("inline_script = {{ script = s{} }}\n", i + 1),
            )
        })
        .collect();
    let mut scripts: Vec<(&str, &str)> = bodies
        .iter()
        .map(|(path, body)| (path.as_str(), body.as_str()))
        .collect();
    scripts.push(("common/inline_scripts/s6.txt", "id = x\n"));

    let errors = run(
        "foo = {\n    id = x\n    inline_script = { script = s0 }\n}\n",
        &scripts,
    );
    assert_eq!(codes(&errors), ["CW274"], "got: {errors:?}");
    assert!(
        errors[0].message.contains("nests more than"),
        "got: {}",
        errors[0].message
    );
}

/// Without a registry there is no body to judge, so the call stands as written
/// rather than being reported as a field the rules don't know.
#[test]
fn a_call_is_accepted_when_no_scripts_are_loaded() {
    let errors =
        run_without_registry("foo = {\n    id = x\n    inline_script = { script = bonus }\n}\n");
    assert!(errors.is_empty(), "got: {errors:?}");
}
