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
    if let Some((_name, (rule, _opts))) = ruleset.aliases.first()
        && let RuleType::NodeRule { rules, .. } = rule
    {
        // name: 1..1 strict
        let (_, name_opts) = &rules[0];
        assert_eq!(name_opts.min, 1);
        assert_eq!(name_opts.max, 1);
        assert!(name_opts.strict_min);

        // count: int_value_field
        let (count_rule, _) = &rules[1];
        if let RuleType::LeafRule { right, .. } = count_rule {
            assert!(matches!(
                right,
                NewField::ValueScopeMarkerField { is_int: true, .. }
            ));
        }

        // pct: ##cardinality= (no space) + percentage_field
        let (_, pct_opts) = &rules[3];
        assert_eq!(pct_opts.min, 0);
        assert_eq!(pct_opts.max, 1);
        let (pct_rule, _) = &rules[3];
        if let RuleType::LeafRule { right, .. } = pct_rule {
            assert!(matches!(right, NewField::ValueField(ValueType::Percent)));
        }

        // filepath[gfx,dds]
        let (fp_rule, _) = &rules[5];
        if let RuleType::LeafRule { right, .. } = fp_rule {
            assert!(matches!(
                right,
                NewField::FilepathField {
                    prefix: Some(_),
                    extension: Some(_)
                }
            ));
        }

        // scope[country]
        let (sc_rule, _) = &rules[6];
        if let RuleType::LeafRule { right, .. } = sc_rule {
            assert!(matches!(right, NewField::ScopeField(_)));
        }

        // variable_field
        let (vf_rule, _) = &rules[7];
        if let RuleType::LeafRule { right, .. } = vf_rule {
            assert!(matches!(
                right,
                NewField::VariableField {
                    is_int: false,
                    is_32bit: false,
                    ..
                }
            ));
        }

        // int_variable_field
        let (ivf_rule, _) = &rules[8];
        if let RuleType::LeafRule { right, .. } = ivf_rule {
            assert!(matches!(
                right,
                NewField::VariableField {
                    is_int: true,
                    is_32bit: false,
                    ..
                }
            ));
        }
    }
}
