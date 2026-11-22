use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_rules::rules_types::*;
use cwtools_string_table::string_table::StringTable;

#[test]
fn colour_rgb_loads_three_or_four_integer_channels() {
    let expected = vec![vec![channel(ValueType::Int { min: 0, max: 255 })]];
    assert_eq!(colour_bodies("rgb"), expected);
}

#[test]
fn colour_hsv_loads_three_or_four_float_channels() {
    let expected = vec![vec![channel(ValueType::Float { min: 0.0, max: 2.0 })]];
    assert_eq!(colour_bodies("hsv"), expected);
}

#[test]
fn colour_with_another_format_loads_one_overload_per_channel_form() {
    let expected = vec![
        vec![channel(ValueType::Int { min: 0, max: 255 })],
        vec![channel(ValueType::Float { min: 0.0, max: 2.0 })],
    ];
    assert_eq!(colour_bodies("something_else"), expected);
}

fn channel(value: ValueType) -> NewRule {
    (
        RuleType::LeafValueRule {
            right: NewField::ValueField(value),
        },
        Options {
            min: 3,
            max: 4,
            leafvalue: true,
            ..Options::default()
        },
    )
}

/// Loads `thing = { colour = colour[<format>] }`; returns each `colour` overload body.
fn colour_bodies(format: &str) -> Vec<Vec<NewRule>> {
    let table = StringTable::new();
    let parsed = parse_string(&format!("thing = {{ colour = colour[{format}] }}"), &table);
    let ruleset = ast_to_ruleset(&parsed, &table);

    let [RootRule::TypeRule(_, (RuleType::NodeRule { rules, .. }, _))] =
        ruleset.root_rules.as_slice()
    else {
        panic!("expected one block rule, got {:?}", ruleset.root_rules);
    };
    rules
        .iter()
        .map(|(rule_type, _)| match rule_type {
            RuleType::NodeRule { left, rules: body } => {
                assert_eq!(left, &NewField::SpecificField("colour".to_string()));
                body.to_vec()
            }
            other => panic!("expected a colour block overload, got {other:?}"),
        })
        .collect()
}
