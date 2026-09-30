//! A type whose `## severity = warning` directive is read from the comment
//! above it (not a body leaf) demotes its errors to warnings (#264).

use cwtools_game::constants::Game;
use cwtools_index::{TypeIndex, collect_type_instances};
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::{ErrorSeverity, Prepared, build_scope_registry_arc, validate_prepared};

fn assert_type_errors_are_warnings(rules: &str, input: &str) {
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(rules, &table), &table);
    let registry = build_scope_registry_arc(&ruleset, Some(Game::Hoi4));

    let path = "game/common/things/a.txt";
    let ast = parse_string(input, &table);

    let mut index = TypeIndex::new();
    index.merge(path, collect_type_instances(&ruleset, &ast, path, &table));

    let prepared = Prepared {
        ruleset: &ruleset,
        table: &table,
        game: Some(Game::Hoi4),
        type_index: Some(&index),
        modifier_keys: None,
        loc_index: None,
        extra_loc_keys: None,
        inline_scripts: None,
        registry: registry.as_ref(),
        scope_checks: false,
        var_checks: false,
    };

    let errors = validate_prepared(&ast, path, &prepared);
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
