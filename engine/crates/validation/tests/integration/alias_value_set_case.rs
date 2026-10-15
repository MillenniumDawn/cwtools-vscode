//! `value[...]` alias-name patterns resolve regardless of ASCII case, like the
//! adjacent `enum[...]` arm (#483); a non-member middle still fails with CW263.

use cwtools_validation::Game;

use super::common::{Opts, codes};

const FOO_FILE: Opts<'static> = Opts {
    path: "game/common/foo/test.txt",
    game: Some(Game::Hoi4),
    ..Opts::DEFAULT
};

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
        &FOO_FILE,
    );
    assert!(!c.contains(&"CW263".to_string()), "got: {:?}", c);
}

#[test]
fn value_set_alias_pattern_matches_mixed_case_query() {
    let c = codes(
        ALIAS_VALUE_RULES,
        "foo = { POLITICAL_POWER_cost_factor = 5.0 }",
        &FOO_FILE,
    );
    assert!(!c.contains(&"CW263".to_string()), "got: {:?}", c);
}

#[test]
fn value_set_alias_pattern_matches_mixed_case_member() {
    let member = codes(
        ALIAS_VALUE_RULES,
        "foo = { Stability_Handicap_cost_factor = 5.0 }",
        &FOO_FILE,
    );
    assert!(!member.contains(&"CW263".to_string()), "got: {member:?}");
    let query = codes(
        ALIAS_VALUE_RULES,
        "foo = { STABILITY_HANDICAP_cost_factor = 5.0 }",
        &FOO_FILE,
    );
    assert!(!query.contains(&"CW263".to_string()), "got: {query:?}");
}

#[test]
fn value_set_alias_pattern_rejects_non_member_still() {
    let c = codes(
        ALIAS_VALUE_RULES,
        "foo = { command_power_cost_factor = 5.0 }",
        &FOO_FILE,
    );
    assert!(c.contains(&"CW263".to_string()), "got: {:?}", c);
}
