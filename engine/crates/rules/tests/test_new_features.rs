use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_rules::rules_types::*;
use cwtools_string_table::string_table::StringTable;

#[test]
fn test_new_field_variants() {
    let input = r#"
alias[effect:test] = {
    ## cardinality = 1..1
    name = value_set[local_variable]
    ## cardinality = 0..1
    count = int_value_field
    ## cardinality = 0..inf
    value = value_field
    ##cardinality = 0..1
    pct = percentage_field
    loc = localisation
    p2 = filepath[gfx,dds]
    scope_test = scope[country]
    var_test = variable_field
    int_var = int_variable_field
    enum_test = enum[power_types]
}

enums = {
    ### Power Type enum
    enum[power_types] = {
        civic
        military
    }
    
    complex_enum[my_complex] = {
        path = game/common/complex
        name = {
            some_key = enum_name
        }
        start_from_root = yes
    }
}

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

values = {
    value[my_values] = {
        alpha
        beta
        gamma
    }
}
"#;

    let table = StringTable::new();
    let parsed = parse_string(input, &table);
    let ruleset = ast_to_ruleset(&parsed, &table);

    // G: values block
    assert_eq!(ruleset.values.len(), 1);
    let my_vals = ruleset
        .values
        .get("my_values")
        .expect("my_values not found");
    assert_eq!(
        my_vals.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        vec!["alpha", "beta", "gamma"]
    );

    // F: complex_enum
    assert_eq!(ruleset.complex_enums.len(), 1);
    assert_eq!(ruleset.complex_enums[0].name, "my_complex");
    assert!(ruleset.complex_enums[0].start_from_root);
    assert!(matches!(
        ruleset.complex_enums[0].name_tree,
        ComplexEnumNameTree::Entries(_)
    ));

    // F: enum description from ###
    let pe = ruleset
        .enums
        .iter()
        .find(|e| e.key == "power_types")
        .unwrap();
    assert_eq!(pe.description, "Power Type enum");

    // C: type metadata
    let mt = ruleset.types.iter().find(|t| t.name == "my_type").unwrap();
    assert!(mt.path_options.path_strict);
    assert!(mt.warning_only);
    assert!(mt.should_be_referenced);
    assert!(mt.unique);
    assert_eq!(mt.starts_with, Some("my_".to_string()));
    assert_eq!(mt.key_prefix, Some("MY_".to_string()));

    // B: cardinality parsing
    let (_, (rule, _opts)) = ruleset
        .aliases
        .first()
        .expect("expected at least one alias");
    let RuleType::NodeRule { rules, .. } = rule else {
        panic!("expected a node rule for the alias, got {rule:?}");
    };

    // name: 1..1 strict
    let (_, name_opts) = &rules[0];
    assert_eq!(name_opts.min, 1);
    assert_eq!(name_opts.max, 1);
    assert!(name_opts.strict_min);

    // count: int_value_field
    let (count_rule, _) = &rules[1];
    let RuleType::LeafRule { right, .. } = count_rule else {
        panic!("expected a leaf rule for count, got {count_rule:?}");
    };
    assert!(matches!(
        right,
        NewField::ValueScopeMarkerField { is_int: true, .. }
    ));

    // pct: ##cardinality= (no space) + percentage_field
    let (_, pct_opts) = &rules[3];
    assert_eq!(pct_opts.min, 0);
    assert_eq!(pct_opts.max, 1);
    let (pct_rule, _) = &rules[3];
    let RuleType::LeafRule { right, .. } = pct_rule else {
        panic!("expected a leaf rule for pct, got {pct_rule:?}");
    };
    assert!(matches!(right, NewField::ValueField(ValueType::Percent)));

    // filepath[gfx,dds]
    let (fp_rule, _) = &rules[5];
    let RuleType::LeafRule { right, .. } = fp_rule else {
        panic!("expected a leaf rule for filepath, got {fp_rule:?}");
    };
    assert!(matches!(
        right,
        NewField::FilepathField {
            prefix: Some(_),
            extension: Some(_)
        }
    ));

    // scope[country]
    let (sc_rule, _) = &rules[6];
    let RuleType::LeafRule { right, .. } = sc_rule else {
        panic!("expected a leaf rule for scope, got {sc_rule:?}");
    };
    assert!(matches!(right, NewField::ScopeField(_)));

    // variable_field
    let (vf_rule, _) = &rules[7];
    let RuleType::LeafRule { right, .. } = vf_rule else {
        panic!("expected a leaf rule for variable_field, got {vf_rule:?}");
    };
    assert!(matches!(
        right,
        NewField::VariableField {
            is_int: false,
            is_32bit: false,
            ..
        }
    ));

    // int_variable_field
    let (ivf_rule, _) = &rules[8];
    let RuleType::LeafRule { right, .. } = ivf_rule else {
        panic!("expected a leaf rule for int_variable_field, got {ivf_rule:?}");
    };
    assert!(matches!(
        right,
        NewField::VariableField {
            is_int: true,
            is_32bit: false,
            ..
        }
    ));
}

// Parse a single bounded field form through ast_to_ruleset and return its
// right-hand NewField. The form is the value of a leaf inside an alias clause.
fn parse_field(input: &str) -> NewField {
    let cwt = format!("alias[effect:test] = {{\n    field = {input}\n}}");
    let table = StringTable::new();
    let parsed = parse_string(&cwt, &table);
    let ruleset = ast_to_ruleset(&parsed, &table);
    let (_, (rule, _)) = ruleset
        .aliases
        .first()
        .expect("expected at least one alias");
    let RuleType::NodeRule { rules, .. } = rule else {
        panic!("expected a node rule, got {rule:?}");
    };
    let (field_rule, _) = &rules[0];
    let RuleType::LeafRule { right, .. } = field_rule else {
        panic!("expected a leaf rule, got {field_rule:?}");
    };
    right.clone()
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
