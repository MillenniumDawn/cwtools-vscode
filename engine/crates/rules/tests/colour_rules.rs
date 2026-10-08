use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_rules::rules_types::*;
use cwtools_string_table::string_table::StringTable;

#[test]
fn colour_rgb_loads_three_or_four_integer_channels() {
    let expected = vec![channel(ValueType::Int { min: 0, max: 255 })];
    assert_eq!(colour_body("rgb"), expected);
}

#[test]
fn colour_hsv_loads_three_or_four_float_channels() {
    let expected = vec![channel(ValueType::Float { min: 0.0, max: 2.0 })];
    assert_eq!(colour_body("hsv"), expected);
}

#[test]
fn colour_with_another_format_loads_both_channel_forms() {
    let expected = vec![
        channel(ValueType::Int { min: 0, max: 255 }),
        channel(ValueType::Float { min: 0.0, max: 2.0 }),
    ];
    assert_eq!(colour_body("something_else"), expected);
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

/// Loads `thing = { colour = colour[<format>] }` and returns the rules
/// under `colour`, which the loader turns into a block of channel values.
fn colour_body(format: &str) -> Vec<NewRule> {
    let table = StringTable::new();
    let parsed = parse_string(&format!("thing = {{ colour = colour[{format}] }}"), &table);
    let ruleset = ast_to_ruleset(&parsed, &table);

    let [RootRule::TypeRule(_, (RuleType::NodeRule { rules, .. }, _))] =
        ruleset.root_rules.as_slice()
    else {
        panic!("expected one block rule, got {:?}", ruleset.root_rules);
    };
    let [(RuleType::NodeRule { left, rules: body }, _)] = &rules[..] else {
        panic!("expected one rule under the block, got {rules:?}");
    };
    assert_eq!(left, &NewField::SpecificField("colour".to_string()));
    body.to_vec()
}
