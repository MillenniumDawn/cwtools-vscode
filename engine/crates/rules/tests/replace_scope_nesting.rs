//! Probe: does `## replace_scope` above a nested block rule attach to that
//! rule's Options? (operations.cwt `selection_target_state` / `outcome_execute`
//! rely on it; CW104/105 false positives in country scope suggest it doesn't.)

use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_rules::rules_types::*;
use cwtools_string_table::string_table::StringTable;

#[test]
fn replace_scope_attaches_to_nested_block_rule() {
    let input = r#"
operation = {
    ## replace_scope = { THIS = state ROOT = state }
    selection_target_state = {
        alias_name[trigger] = alias_match_left[trigger]
    }
}
types = {
    type[operation] = { path = "game/common/operations" }
}
"#;
    let table = StringTable::new();
    let parsed = parse_string(input, &table);
    let ruleset = ast_to_ruleset(&parsed, &table);

    let op = ruleset
        .root_rules
        .iter()
        .find_map(|rr| match rr {
            RootRule::TypeRule(name, (rt, _)) if name == "operation" => Some(rt),
            _ => None,
        })
        .expect("operation rule not found");

    let RuleType::NodeRule { rules, .. } = op else {
        panic!("operation should be a NodeRule");
    };

    let sts = rules
        .iter()
        .find(|(rt, _)| {
            matches!(rt, RuleType::NodeRule { left: NewField::SpecificField(s), .. } if s == "selection_target_state")
        })
        .expect("selection_target_state child not found");

    let rs = sts
        .1
        .replace_scopes
        .as_ref()
        .expect("## replace_scope above a nested block must attach to its Options");
    // operations.cwt writes the keys uppercase (`THIS = state ROOT = state`);
    // parsing must be case-insensitive on the key.
    assert_eq!(
        rs.this.as_deref(),
        Some("state"),
        "THIS should map to state"
    );
    assert_eq!(
        rs.root.as_deref(),
        Some("state"),
        "ROOT should map to state"
    );
}
