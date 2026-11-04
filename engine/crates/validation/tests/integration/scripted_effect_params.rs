//! `enum[scripted_effect_params]` is filled from the scripted effect and trigger
//! files, so a call may only pass parameters some scripted effect or trigger
//! declares (#1008).
//!
//! The trigger rules mirror the Stellaris config: a bare `bool` overload beside
//! the block. The effect rule has only the block, unlike the config's
//! `alias[effect:<scripted_effect>] = yes`, which accepts any block first.

use cwtools_index::{TypeIndex, collect_type_instances, dynamic_values};
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_types::RuleSet;
use cwtools_string_table::string_table::StringTable;

use super::common::{Opts, ruleset, validate_with};

const TYPES: &str = r#"
types = {
    type[scripted_effect] = {
        path = "game/common/scripted_effects"
    }
    type[scripted_trigger] = {
        path = "game/common/scripted_triggers"
    }
    type[evt] = {
        path = "game/events"
    }
}

evt = {
    ## cardinality = 0..1
    effect = {
        alias_name[effect] = alias_match_left[effect]
    }
    ## cardinality = 0..1
    trigger = {
        alias_name[trigger] = alias_match_left[trigger]
    }
}

alias[effect:<scripted_effect>] = {
    ## cardinality = 1..inf
    enum[scripted_effect_params] = scalar
}

alias[trigger:<scripted_trigger>] = bool

alias[trigger:<scripted_trigger>] = {
    ## cardinality = 1..inf
    enum[scripted_effect_params] = scalar
}
"#;

// Six parameters between them: a config enum this large would accept any key.
const EFFECTS: &str = r#"
first_effect = {
    add_x = $ALPHA$
    add_y = $BETA$
    add_z = "$GAMMA|0$"
}
second_effect = {
    $DELTA$ = { add_x = $EPSILON$ }
}
conditional_effect = {
    [[ONLY_IN_EFFECT] add_x = 1 ]
    [[!NEGATED] add_y = 1 ]
}
"#;

const TRIGGERS: &str = r#"
first_trigger = {
    has_flag = $ZETA$
}
conditional_trigger = {
    [[ONLY_IN_TRIGGER] has_flag = yes ]
}
"#;

const FILES: [(&str, &str, &str); 2] = [
    (
        EFFECTS,
        "game/common/scripted_effects/effects.txt",
        "common/scripted_effects/effects.txt",
    ),
    (
        TRIGGERS,
        "game/common/scripted_triggers/triggers.txt",
        "common/scripted_triggers/triggers.txt",
    ),
];

fn index(table: &StringTable, ruleset: &RuleSet, complete: bool) -> TypeIndex {
    let mut index = TypeIndex::new();
    index.complete = complete;
    for (source, path, logical_path) in FILES {
        let parsed = parse_string(source, table);
        let uri = format!("file://{logical_path}");
        index.merge(&uri, collect_type_instances(ruleset, &parsed, path, table));
        index.complex_enum_values.merge_file(
            &uri,
            dynamic_values::collect_complex_enum_values(ruleset, &parsed, logical_path, table),
        );
    }
    index
}

/// What validating `call` inside the event's `block` (`effect` or `trigger`) reports,
/// with the base game loaded.
fn call_errors(cwt: &str, block: &str, call: &str) -> Vec<String> {
    call_errors_with(cwt, block, call, true)
}

fn call_errors_with(cwt: &str, block: &str, call: &str, complete: bool) -> Vec<String> {
    let (table, ruleset) = ruleset(cwt);
    let index = index(&table, &ruleset, complete);
    let script = format!("evt = {{ {block} = {{ {call} }} }}");
    let opts = Opts {
        path: "game/events/test.txt",
        type_index: Some(&index),
        ..Opts::DEFAULT
    };
    validate_with(&table, &ruleset, &script, &opts)
        .into_iter()
        .map(|error| error.message)
        .collect()
}

#[test]
fn a_call_may_pass_any_declared_parameter() {
    let effect = "first_effect = { ALPHA = 1 gamma = 2 EPSILON = 3 $DELTA$ = 4 }";
    let errors = call_errors(TYPES, "effect", effect);
    assert!(errors.is_empty(), "got {errors:?}");
    let errors = call_errors(TYPES, "trigger", "first_trigger = { ZETA = 1 alpha = 2 }");
    assert!(errors.is_empty(), "got {errors:?}");
}

#[test]
fn an_effect_call_with_an_undeclared_parameter_is_reported() {
    let errors = call_errors(TYPES, "effect", "first_effect = { ALPHA = 1 BOGUS = 2 }");
    assert_eq!(errors.len(), 1, "got {errors:?}");
    assert!(errors[0].contains("BOGUS"), "got {errors:?}");
}

#[test]
fn a_trigger_call_with_an_undeclared_parameter_is_reported() {
    let errors = call_errors(TYPES, "trigger", "first_trigger = { ZETA = 1 BOGUS = 2 }");
    assert_eq!(errors.len(), 1, "got {errors:?}");
}

#[test]
fn a_call_may_pass_a_parameter_used_only_in_a_conditional_block() {
    let errors = call_errors(
        TYPES,
        "effect",
        "conditional_effect = { ONLY_IN_EFFECT = 1 NEGATED = 2 }",
    );
    assert!(errors.is_empty(), "got {errors:?}");
    let errors = call_errors(
        TYPES,
        "trigger",
        "conditional_trigger = { ONLY_IN_TRIGGER = 1 }",
    );
    assert!(errors.is_empty(), "got {errors:?}");
}

#[test]
fn calls_are_not_checked_without_the_base_game() {
    // A declared callee passing a name only the base game could declare.
    for (block, call) in [
        ("effect", "first_effect = { ALPHA = 1 BOGUS = 2 }"),
        ("trigger", "first_trigger = { ZETA = 1 BOGUS = 2 }"),
    ] {
        let errors = call_errors_with(TYPES, block, call, false);
        assert!(errors.is_empty(), "{call}: got {errors:?}");
    }
    // A base-game callee is not indexed, so it never reaches the parameter rule.
    let errors = call_errors_with(
        TYPES,
        "trigger",
        "base_game_trigger = { BASE_PARAM = 1 }",
        false,
    );
    assert!(
        errors.iter().all(|error| !error.contains("BASE_PARAM")),
        "got {errors:?}"
    );
}

#[test]
fn a_config_enum_of_that_name_is_not_replaced() {
    let config_enum = format!("{TYPES}\nenums = {{ enum[scripted_effect_params] = {{ BOGUS }} }}");
    let errors = call_errors(&config_enum, "effect", "first_effect = { BOGUS = 1 }");
    assert!(errors.is_empty(), "got {errors:?}");
    let errors = call_errors(&config_enum, "effect", "first_effect = { ALPHA = 1 }");
    assert_eq!(errors.len(), 1, "got {errors:?}");
    assert!(errors[0].contains("ALPHA"), "got {errors:?}");
}
