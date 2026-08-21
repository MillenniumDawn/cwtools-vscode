use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::validate_ast;

fn ruleset(cwt: &str, table: &StringTable) -> cwtools_rules::rules_types::RuleSet {
    ast_to_ruleset(&parse_string(cwt, table), table)
}

// Regression: a top-level node must be validated against the type its own key
// selects via `## type_key_filter`, not against another type that merely shares
// the path. `animation = { name file }` is a `model_animation`, NOT a `light`,
// even though both types live under `gfx` with the `.asset` extension.
#[test]
fn type_key_filter_routes_top_level_node_to_correct_type() {
    let table = StringTable::new();
    let rs = ruleset(
        r#"
types = {
    ## type_key_filter = animation
    type[model_animation] = { path = "game/gfx" path_extension = .asset name_field = "name" }
    ## type_key_filter = light
    type[light] = { path = "game/gfx" path_extension = .asset name_field = "name" }
}
model_animation = {
    name = scalar
    file = scalar
}
light = {
    name = scalar
    color = { float float float }
    radius = float
}
"#,
        &table,
    );
    // A top-level `animation` node in a .asset file: must validate as model_animation.
    let script = "animation = { name = \"a\" file = \"x.anim\" }\n";
    let parsed = parse_string(script, &table);
    let errors = validate_ast(
        &parsed,
        &rs,
        &table,
        "gfx/models/units/a.asset",
        None,
        None,
        None,
    );
    assert!(
        errors.is_empty(),
        "expected model_animation routing (no light errors), got: {:?}",
        errors
            .iter()
            .map(|e| (&e.message, e.line))
            .collect::<Vec<_>>()
    );
}

// Regression: a cardinality diagnostic for a required field/value missing from an
// EMPTY block must point at the block's opening line, not the file root (line 0).
#[test]
fn empty_block_cardinality_reports_inline_not_at_file_root() {
    let table = StringTable::new();
    let rs = ruleset(
        r#"
types = {
    type[foo] = { path = "game/common/foo" }
}
foo = {
    name = scalar
    sub = {
        ## cardinality = 1..inf
        scalar
    }
}
"#,
        &table,
    );
    // `sub` is present but empty → its required bare value is missing.
    let script = "my_foo = {\n\tname = \"x\"\n\tsub = {\n\t}\n}\n";
    let parsed = parse_string(script, &table);
    let errors = validate_ast(&parsed, &rs, &table, "common/foo/a.txt", None, None, None);
    let card: Vec<&cwtools_validation::ValidationError> = errors
        .iter()
        .filter(|e| e.message.contains("appears 0 time(s)"))
        .collect();
    assert_eq!(
        card.len(),
        1,
        "expected one cardinality diagnostic, got: {:?}",
        errors
            .iter()
            .map(|e| (&e.message, e.line))
            .collect::<Vec<_>>()
    );
    // `sub = {` is on line 3 of the script; the diagnostic must land there, not 0.
    assert_eq!(
        card[0].line, 3,
        "cardinality diagnostic should be inline on the block line, not the file root"
    );
}
