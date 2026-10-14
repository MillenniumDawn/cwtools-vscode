//! `## default_bool = yes|no` (cwtools-vscode #26): a bool field annotated with
//! its engine default emits an info-level hint (CW282) when it is explicitly set
//! to that default value, so the redundant line can be omitted.

use cwtools_validation::ErrorSeverity;

use super::common::{Opts, codes, validate};

const RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    name = scalar
    ## default_bool = yes
    some_bool_field = bool
}
"#;

#[test]
fn emits_cw282_when_set_to_default() {
    assert!(
        codes(
            RULES,
            "foo = { name = x some_bool_field = yes }",
            &Opts::DEFAULT
        )
        .contains(&"CW282".to_string()),
        "expected CW282 when bool field is set to its default"
    );
}

#[test]
fn cw282_is_information_severity() {
    let err = validate(
        RULES,
        "foo = { name = x some_bool_field = yes }",
        &Opts::DEFAULT,
    )
    .into_iter()
    .find(|e| e.code == Some("CW282"))
    .expect("CW282 emitted");
    assert_eq!(err.severity, ErrorSeverity::Information);
}

#[test]
fn no_hint_when_set_to_non_default() {
    assert!(
        !codes(
            RULES,
            "foo = { name = x some_bool_field = no }",
            &Opts::DEFAULT
        )
        .contains(&"CW282".to_string()),
        "CW282 must not fire when the field is set to a non-default value"
    );
}

#[test]
fn cw282_fix_deletes_redundant_leaf() {
    use cwtools_parser::fix::apply_edits;
    let src = "foo = { name = x some_bool_field = yes }";
    let errors = validate(RULES, src, &Opts::DEFAULT);

    let err = errors
        .iter()
        .find(|e| e.code == Some("CW282"))
        .expect("CW282 emitted");
    let fix = err.fix.as_ref().expect("CW282 carries a fix");
    let fixed = apply_edits(src, &fix.edits);
    assert_eq!(fixed, "foo = { name = x }");

    // Revalidation of the fixed text no longer emits CW282.
    let errors2 = validate(RULES, &fixed, &Opts::DEFAULT);
    assert!(
        !errors2.iter().any(|e| e.code == Some("CW282")),
        "CW282 must be gone after applying the fix"
    );
}

#[test]
fn no_hint_without_directive() {
    // A bool field with no `## default_bool` annotation never emits CW282.
    const PLAIN: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    name = scalar
    some_bool_field = bool
}
"#;
    let errors = validate(
        PLAIN,
        "foo = { name = x some_bool_field = yes }",
        &Opts::DEFAULT,
    );
    assert!(
        !errors.iter().any(|e| e.code == Some("CW282")),
        "CW282 must not fire without the directive"
    );
}
