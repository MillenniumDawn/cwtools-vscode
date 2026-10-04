//! `value[...]` alias-name patterns resolve regardless of ASCII case, like the
//! adjacent `enum[...]` arm (#483); a non-member middle still fails with CW263.

use cwtools_game::constants::Game;
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::{ValidationError, validate_ast};

fn validate(rules: &str, script: &str) -> Vec<ValidationError> {
    let table = StringTable::new();
    let parsed_cwt = parse_string(rules, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);
    let parsed = parse_string(script, &table);
    validate_ast(
        &parsed,
        &ruleset,
        &table,
        "game/common/foo/test.txt",
        Some(Game::Hoi4),
        None,
        None,
    )
}

fn codes(rules: &str, script: &str) -> Vec<String> {
    validate(rules, script)
        .into_iter()
        .filter_map(|e| e.code.map(String::from))
        .collect()
}

// Shaped after `alias[modifier:value[idea_slot]_cost_factor]` in temp_modifiers.cwt.
const ALIAS_VALUE_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
values = {
    value[idea_slot] = {
        political_power
        Stability_Handicap
    }
}
alias[modifier:value[idea_slot]_cost_factor] = float
foo = {
    alias_name[modifier] = alias_match_left[modifier]
}
"#;

#[test]
fn value_set_alias_pattern_matches_original_case_key() {
    let c = codes(
        ALIAS_VALUE_RULES,
        "foo = { political_power_cost_factor = 5.0 }",
    );
    assert!(!c.contains(&"CW263".to_string()), "got: {:?}", c);
}

#[test]
fn value_set_alias_pattern_matches_mixed_case_query() {
    let c = codes(
        ALIAS_VALUE_RULES,
        "foo = { POLITICAL_POWER_cost_factor = 5.0 }",
    );
    assert!(!c.contains(&"CW263".to_string()), "got: {:?}", c);
}

#[test]
fn value_set_alias_pattern_matches_mixed_case_member() {
    let member = codes(
        ALIAS_VALUE_RULES,
        "foo = { Stability_Handicap_cost_factor = 5.0 }",
    );
    assert!(!member.contains(&"CW263".to_string()), "got: {member:?}");
    let query = codes(
        ALIAS_VALUE_RULES,
        "foo = { STABILITY_HANDICAP_cost_factor = 5.0 }",
    );
    assert!(!query.contains(&"CW263".to_string()), "got: {query:?}");
}

#[test]
fn value_set_alias_pattern_rejects_non_member_still() {
    let c = codes(
        ALIAS_VALUE_RULES,
        "foo = { command_power_cost_factor = 5.0 }",
    );
    assert!(c.contains(&"CW263".to_string()), "got: {:?}", c);
}
