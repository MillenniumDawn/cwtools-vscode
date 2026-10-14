//! A type whose `## severity = warning` directive is read from the comment
//! above it (not a body leaf) demotes its errors to warnings (#264).

use cwtools_index::{TypeIndex, collect_type_instances};
use cwtools_parser::parser::parse_string;
use cwtools_validation::{ErrorSeverity, Game};

use super::common::{Opts, ruleset, validate_with};

fn assert_type_errors_are_warnings(rules: &str, input: &str) {
    let (table, ruleset) = ruleset(rules);

    let path = "game/common/things/a.txt";
    let ast = parse_string(input, &table);

    let mut index = TypeIndex::new();
    index.merge(path, collect_type_instances(&ruleset, &ast, path, &table));

    let opts = Opts {
        path,
        game: Some(Game::Hoi4),
        type_index: Some(&index),
        scope_checks: false,
        var_checks: false,
        ..Opts::DEFAULT
    };
    let errors = validate_with(&table, &ruleset, input, &opts);
    let unexpected_node_errors: Vec<_> = errors
        .iter()
        .filter(|error| error.code == Some("CW262"))
        .collect();

    assert!(
        !unexpected_node_errors.is_empty(),
        "expected the unexpected-node error, got: {errors:?}"
    );
    assert!(
        unexpected_node_errors
            .iter()
            .all(|error| error.severity == ErrorSeverity::Warning),
        "type severity directive must downgrade the unexpected-node error, got: {errors:?}"
    );
    assert!(
        errors
            .iter()
            .all(|error| error.severity == ErrorSeverity::Warning),
        "type severity directive must downgrade errors to warnings, got: {errors:?}"
    );
}

#[test]
fn comment_declared_severity_warning_demotes_type_errors() {
    let rules = r#"
types = {
    ## severity = warning
    type[thing] = {
        path = "game/common/things"
    }
}
thing = { x = scalar }
"#;
    // Keep the original no-subtype CW262 regression.
    let input = "thing = { unexpected = { } }\n";

    assert_type_errors_are_warnings(rules, input);
}

#[test]
fn comment_declared_severity_warning_demotes_active_subtype_errors() {
    let rules = r#"
types = {
    ## severity = warning
    type[thing] = {
        path = "game/common/things"
        subtype[has_special] = { kind = special }
    }
}
thing = {
    kind = scalar
    x = scalar
    subtype[has_special] = { special = scalar }
}
"#;
    let input = "thing = { kind = special x = valid special = valid unexpected = { } }\n";

    assert_type_errors_are_warnings(rules, input);
}
