//! A type whose `## severity = warning` directive is read from the comment
//! above it (not a body leaf) demotes its errors to warnings (#264).

use cwtools_game::constants::Game;
use cwtools_index::{TypeIndex, collect_type_instances};
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::{ErrorSeverity, Prepared, build_scope_registry_arc, validate_prepared};

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
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(rules, &table), &table);
    let registry = build_scope_registry_arc(&ruleset, Some(Game::Hoi4));

    let path = "game/common/things/a.txt";
    // `unexpected` matches no rule, so it is an unexpected property node (CW262)
    // and would be an Error on a normal type. The directive downgrades it.
    let ast = parse_string("thing = { unexpected = { } }\n", &table);

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

    assert!(
        errors.iter().any(|e| e.code == Some("CW262")),
        "expected the unexpected-node error, got: {errors:?}"
    );
    assert!(
        errors.iter().all(|e| e.severity == ErrorSeverity::Warning),
        "type severity directive must downgrade errors to warnings, got: {errors:?}"
    );
}
