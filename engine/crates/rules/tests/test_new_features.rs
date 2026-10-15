use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_rules::rules_types::*;
use cwtools_string_table::string_table::StringTable;

#[test]
fn values_block_collects_its_members() {
    let ruleset = parse_ruleset(
        r#"
values = {
    value[my_values] = {
        alpha
        beta
        gamma
    }
}
"#,
    );

    assert_eq!(ruleset.values.len(), 1);
    let my_vals = ruleset
        .values
        .get("my_values")
        .expect("my_values not found");
    assert_eq!(
        my_vals.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        vec!["alpha", "beta", "gamma"]
    );
}

#[test]
fn complex_enum_is_collected() {
    let ruleset = parse_ruleset(
        r#"
enums = {
    complex_enum[my_complex] = {
        path = game/common/complex
        name = {
            some_key = enum_name
        }
        start_from_root = yes
    }
}
"#,
    );

    assert_eq!(ruleset.complex_enums.len(), 1);
    assert_eq!(ruleset.complex_enums[0].name, "my_complex");
    assert!(ruleset.complex_enums[0].start_from_root);
    assert!(matches!(
        ruleset.complex_enums[0].name_tree,
        ComplexEnumNameTree::Entries(_)
    ));
}

#[test]
fn enum_description_comes_from_the_triple_hash_comment() {
    let ruleset = parse_ruleset(
        r#"
enums = {
    ### Power Type enum
    enum[power_types] = {
        civic
        military
    }
}
"#,
    );

    let pe = ruleset
        .enums
        .iter()
        .find(|e| e.key == "power_types")
        .unwrap();
    assert_eq!(pe.description, "Power Type enum");
}

#[test]
fn type_metadata_is_read() {
    let ruleset = parse_ruleset(
        r#"
## type_key_filter <> ship barrier
## graph_related_types = { country character }
types = {
    type[my_type] = {
        path = game/common/things
        path_strict = yes
        starts_with = my_
        type_key_prefix = MY_
        severity = warning
        should_be_used = yes
        unique = yes
    }
}
"#,
    );

    let mt = ruleset.types.iter().find(|t| t.name == "my_type").unwrap();
    assert!(mt.path_options.path_strict);
    assert!(mt.warning_only);
    assert!(mt.should_be_referenced);
    assert!(mt.unique);
    assert_eq!(mt.starts_with, Some("my_".to_string()));
    assert_eq!(mt.key_prefix, Some("MY_".to_string()));
}

#[test]
fn exact_cardinality_is_strict() {
    let (_, opts) =
        first_alias_rule("    ## cardinality = 1..1\n    name = value_set[local_variable]");

    assert_eq!(opts.min, 1);
    assert_eq!(opts.max, 1);
    assert!(opts.strict_min);
}

#[test]
fn cardinality_comment_without_a_space_is_read() {
    let (_, opts) = first_alias_rule("    ##cardinality = 0..1\n    pct = percentage_field");

    assert_eq!(opts.min, 0);
    assert_eq!(opts.max, 1);
}

#[test]
fn percentage_field_is_a_percent_value() {
    assert!(matches!(
        parse_field("percentage_field"),
        NewField::ValueField(ValueType::Percent)
    ));
}

#[test]
fn filepath_field_carries_prefix_and_extension() {
    assert!(matches!(
        parse_field("filepath[gfx,dds]"),
        NewField::FilepathField {
            prefix: Some(_),
            extension: Some(_)
        }
    ));
}

#[test]
fn scope_field_is_a_scope_field() {
    assert!(matches!(
        parse_field("scope[country]"),
        NewField::ScopeField(_)
    ));
}

fn parse_ruleset(input: &str) -> RuleSet {
    let table = StringTable::new();
    let parsed = parse_string(input, &table);
    ast_to_ruleset(&parsed, &table)
}

// Parse `body` as the clause of an alias and return its first rule.
fn first_alias_rule(body: &str) -> (RuleType, Options) {
    let ruleset = parse_ruleset(&format!("alias[effect:test] = {{\n{body}\n}}"));
    let (_, (rule, _)) = ruleset
        .aliases
        .first()
        .expect("expected at least one alias");
    let RuleType::NodeRule { rules, .. } = rule else {
        panic!("expected a node rule, got {rule:?}");
    };
    rules[0].clone()
}

// Parse a single bounded field form through ast_to_ruleset and return its
// right-hand NewField. The form is the value of a leaf inside an alias clause.
fn parse_field(input: &str) -> NewField {
    let (field_rule, _) = first_alias_rule(&format!("    field = {input}"));
    let RuleType::LeafRule { right, .. } = field_rule else {
        panic!("expected a leaf rule, got {field_rule:?}");
    };
    right
}

fn assert_variable_field(input: &str, is_int: bool, is_32bit: bool, min: f64, max: f64) {
    match parse_field(input) {
        NewField::VariableField {
            is_int: i,
            is_32bit: b,
            min: mn,
            max: mx,
        } => assert_eq!((i, b, mn, mx), (is_int, is_32bit, min, max), "for {input}"),
        other => panic!("{input}: expected VariableField, got {other:?}"),
    }
}

fn assert_value_scope_marker(input: &str, is_int: bool, min: f64, max: f64) {
    match parse_field(input) {
        NewField::ValueScopeMarkerField {
            is_int: i,
            min: mn,
            max: mx,
        } => assert_eq!((i, mn, mx), (is_int, min, max), "for {input}"),
        other => panic!("{input}: expected ValueScopeMarkerField, got {other:?}"),
    }
}

fn assert_int_field(input: &str, min: i32, max: i32) {
    match parse_field(input) {
        NewField::ValueField(ValueType::Int { min: mn, max: mx }) => {
            assert_eq!((mn, mx), (min, max), "for {input}")
        }
        other => panic!("{input}: expected Int field, got {other:?}"),
    }
}

fn assert_float_field(input: &str, min: f64, max: f64) {
    match parse_field(input) {
        NewField::ValueField(ValueType::Float { min: mn, max: mx }) => {
            assert_eq!((mn, mx), (min, max), "for {input}")
        }
        other => panic!("{input}: expected Float field, got {other:?}"),
    }
}

#[test]
fn test_bounded_variable_field_forms() {
    // defaults
    assert_variable_field("variable_field", false, false, -1e12, 1e12);
    assert_variable_field(
        "int_variable_field",
        true,
        false,
        -2147483648.0,
        2147483647.0,
    );
    assert_variable_field("variable_field_32", false, true, -1e12, 1e12);
    assert_variable_field(
        "int_variable_field_32",
        true,
        true,
        -2147483648.0,
        2147483647.0,
    );

    // -inf/inf sentinels
    assert_variable_field("variable_field[-inf..inf]", false, false, -1e12, 1e12);
    assert_variable_field(
        "int_variable_field[-inf..inf]",
        true,
        false,
        -2147483648.0,
        2147483647.0,
    );
    assert_variable_field("variable_field_32[-inf..inf]", false, true, -1e12, 1e12);
    assert_variable_field(
        "int_variable_field_32[-inf..inf]",
        true,
        true,
        -2147483648.0,
        2147483647.0,
    );

    // zero, negative, and finite bounds
    assert_variable_field("variable_field[0..100]", false, false, 0.0, 100.0);
    assert_variable_field("int_variable_field[0..100]", true, false, 0.0, 100.0);
    assert_variable_field("variable_field[-5..5]", false, false, -5.0, 5.0);
    assert_variable_field("int_variable_field[-5..5]", true, false, -5.0, 5.0);
    assert_variable_field("variable_field[0..inf]", false, false, 0.0, 1e12);
    assert_variable_field(
        "int_variable_field[-inf..0]",
        true,
        false,
        -2147483648.0,
        0.0,
    );
    assert_variable_field("variable_field_32[0..100]", false, true, 0.0, 100.0);
    assert_variable_field("int_variable_field_32[-5..5]", true, true, -5.0, 5.0);

    // off-by-one neighboring finite bounds
    assert_variable_field(
        "variable_field[999999999999..1000000000000]",
        false,
        false,
        999999999999.0,
        1000000000000.0,
    );
    assert_variable_field(
        "int_variable_field[2147483646..2147483647]",
        true,
        false,
        2147483646.0,
        2147483647.0,
    );
    assert_variable_field(
        "int_variable_field[-2147483648..-2147483647]",
        true,
        false,
        -2147483648.0,
        -2147483647.0,
    );
}

#[test]
fn test_bounded_value_scope_marker_forms() {
    // defaults
    assert_value_scope_marker("value_field", false, -1e12, 1e12);
    assert_value_scope_marker("int_value_field", true, -2147483648.0, 2147483647.0);

    // -inf/inf sentinels
    assert_value_scope_marker("value_field[-inf..inf]", false, -1e12, 1e12);
    assert_value_scope_marker(
        "int_value_field[-inf..inf]",
        true,
        -2147483648.0,
        2147483647.0,
    );

    // zero, negative, and finite bounds
    assert_value_scope_marker("value_field[0..100]", false, 0.0, 100.0);
    assert_value_scope_marker("int_value_field[0..100]", true, 0.0, 100.0);
    assert_value_scope_marker("value_field[-5..5]", false, -5.0, 5.0);
    assert_value_scope_marker("int_value_field[-5..5]", true, -5.0, 5.0);
    assert_value_scope_marker("value_field[0..inf]", false, 0.0, 1e12);
    assert_value_scope_marker("int_value_field[-inf..0]", true, -2147483648.0, 0.0);

    // off-by-one neighboring finite bounds
    assert_value_scope_marker(
        "int_value_field[2147483646..2147483647]",
        true,
        2147483646.0,
        2147483647.0,
    );
}

#[test]
fn test_bounded_int_forms() {
    // default
    assert_int_field("int", -2147483648, 2147483647);

    // -inf/inf sentinels
    assert_int_field("int[-inf..inf]", -2147483648, 2147483647);

    // zero, negative, and finite bounds
    assert_int_field("int[0..100]", 0, 100);
    assert_int_field("int[-5..5]", -5, 5);
    assert_int_field("int[0..inf]", 0, 2147483647);
    assert_int_field("int[-inf..0]", -2147483648, 0);

    // off-by-one neighboring finite bounds
    assert_int_field("int[2147483646..2147483647]", 2147483646, 2147483647);
    assert_int_field("int[-2147483648..-2147483647]", -2147483648, -2147483647);
}

#[test]
fn test_bounded_float_forms() {
    // default
    assert_float_field("float", -1e12, 1e12);

    // -inf/inf sentinels
    assert_float_field("float[-inf..inf]", -1e12, 1e12);

    // zero, negative, and finite bounds
    assert_float_field("float[0..100]", 0.0, 100.0);
    assert_float_field("float[-5..5]", -5.0, 5.0);
    assert_float_field("float[0..inf]", 0.0, 1e12);
    assert_float_field("float[-inf..0]", -1e12, 0.0);

    // off-by-one neighboring finite bounds
    assert_float_field(
        "float[999999999999..1000000000000]",
        999999999999.0,
        1000000000000.0,
    );
    assert_float_field(
        "float[-1000000000000..-999999999999]",
        -1000000000000.0,
        -999999999999.0,
    );
}
