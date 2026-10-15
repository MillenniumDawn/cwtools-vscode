use cwtools_index::TypeIndex;
use cwtools_parser::parser::parse_string;
use cwtools_validation::Game;

use super::common::{Opts, ruleset, validate, validate_with};

#[test]
fn test_enum_keyed_rule_matches_key() {
    // Regression: a rule keyed by `enum[x] = value` must match keys that are
    // members of enum x (HOI4 `research = { enum[tech_category] = float }`).
    let cwt = r#"
types = {
    type[ai_strategy_plan] = {
        path = "game/common/ai_strategy_plans"
    }
}

ai_strategy_plan = {
    ## cardinality = 0..1
    research = {
        enum[tech_category] = float
    }
}

enums = {
    enum[tech_category] = { CAT_inf }
}
"#;
    let script = r#"
my_plan = {
    research = {
        CAT_inf = 10.0
    }
}
"#;
    let errors = validate(
        cwt,
        script,
        &Opts {
            path: "game/common/ai_strategy_plans/test.txt",
            game: Some(Game::Hoi4),
            ..Opts::DEFAULT
        },
    );
    assert!(
        errors.is_empty(),
        "Expected no errors but got: {:?}",
        errors
    );
}

#[test]
fn test_quoted_key_matches_same_rule_as_unquoted() {
    // Regression: a quoted key like `"AST" = { ... }` is just an alternate spelling
    // of the bare key and must match the same rule. The parser keeps the quotes in
    // `key.normal`, so the validator unquotes the key before matching (mirroring how
    // values are unquoted). Real case: `"LOG" = { has_war_with = CAR }` inside a
    // trigger block was flagged "Unexpected field" while bare `LOG` was accepted.
    let cwt = r#"
types = {
    type[diplo] = {
        path = "game/common/diplo"
    }
}

diplo = {
    ## cardinality = 0..inf
    enum[country_tags] = {
        value = int
    }
}

enums = {
    enum[country_tags] = { AST LOG USA }
}
"#;
    let opts = Opts {
        path: "game/common/diplo/test.txt",
        game: Some(Game::Hoi4),
        ..Opts::DEFAULT
    };

    // Both quoted and unquoted tag keys must validate cleanly.
    let script = r#"
diplo = {
    "AST" = { value = 1 }
    LOG = { value = 2 }
}
"#;
    let errors = validate(cwt, script, &opts);
    assert!(
        errors.is_empty(),
        "Expected no errors but got: {:?}",
        errors
    );

    // Unquoting must not make matching blanket-permissive: a key that is not an
    // enum member (quoted or not) is still unexpected.
    let bad = r#"
diplo = {
    "XYZ" = { value = 1 }
}
"#;
    let errors = validate(cwt, bad, &opts);
    assert!(
        errors.iter().any(|e| e.message.contains("Unexpected")),
        "Expected an unexpected-field error for a non-member tag, got: {:?}",
        errors
    );
}

#[test]
fn test_named_scope_link_is_valid_scope_key() {
    // Regression: a from-data scope link declared in links.cwt (e.g. `character`)
    // can appear as a scope-switching key in an effect block. The validator reads
    // the `links = { ... }` block so `character = { ... }` matches the
    // `alias[effect:scope_field]` overload instead of reading as "Unexpected field".
    // Real case: HOI4 on_actions effect bodies with `character = { ... }`.
    let cwt = r#"
links = {
    character = {
        output_scope = character
        input_scopes = country
        from_data = yes
        data_source = <character>
    }
}

types = {
    type[evt] = {
        path = "game/events"
    }
}

evt = {
    effect = {
        alias_name[effect] = alias_match_left[effect]
    }
}

alias[effect:scope_field] = {
    alias_name[effect] = alias_match_left[effect]
}

alias[effect:add_attack] = int
"#;
    let (table, ruleset) = ruleset(cwt);
    assert!(
        ruleset.scope_links.contains("character"),
        "expected `character` to be collected as a scope link"
    );

    let script = r#"
evt = {
    effect = {
        character = {
            add_attack = 1
        }
    }
}
"#;
    let errors = validate_with(
        &table,
        &ruleset,
        script,
        &Opts {
            path: "game/events/test.txt",
            game: Some(Game::Hoi4),
            ..Opts::DEFAULT
        },
    );
    assert!(
        !errors.iter().any(|e| e.message.contains("character")),
        "Expected no 'Unexpected field character' error, got: {:?}",
        errors
    );
}

/// Regression (issue #29): a character created by `generate_character` via
/// `token_base = <name>` is collected into the `character_token` value-set, and a
/// `from_data` scope link (`character_token`, `data_source = value[character_token]`)
/// lets that name open its own scope. So `<name> = { set_character_flag = ... }`
/// must NOT flag CW262 "Unexpected block". The trigger form `has_character = <name>`
/// was never flagged; only the scope use was.
#[test]
fn test_value_set_member_is_valid_scope_key() {
    // Mirrors the real HOI4 config: generate_character binds `token_base` to the
    // `character_token` value-set, and links.cwt declares a from-data scope link
    // whose data_source is `value[character_token]`.
    let cwt = r#"
links = {
    character_token = {
        output_scope = character
        input_scopes = country
        from_data = yes
        data_source = value[character_token]
    }
}

types = {
    type[evt] = {
        path = "game/events"
    }
}

evt = {
    effect = {
        alias_name[effect] = alias_match_left[effect]
    }
}

alias[effect:scope_field] = {
    alias_name[effect] = alias_match_left[effect]
}

alias[effect:generate_character] = {
    token_base = value_set[character_token]
}

alias[effect:set_character_flag] = value_set[character_flag]
"#;
    let (table, mut ruleset) = ruleset(cwt);
    ruleset.reindex();

    // (a) Collect the value-set member from `token_base = empowered_legislative`,
    // exactly as the indexing pass feeds TypeIndex.value_set_values at runtime.
    let define = parse_string(
        "evt = { effect = { generate_character = { token_base = empowered_legislative } } }\n",
        &table,
    );
    let members =
        cwtools_index::dynamic_values::collect_value_set_members(&ruleset, &define, &table);
    assert!(
        members
            .get("character_token")
            .is_some_and(|v| v.iter().any(|m| m == "empowered_legislative")),
        "expected `empowered_legislative` collected into the `character_token` \
         value-set, got: {members:?}",
    );

    let mut idx = TypeIndex::new();
    idx.value_set_values
        .merge_file("file://define.txt", members);
    let opts = Opts {
        path: "game/events/test.txt",
        game: Some(Game::Hoi4),
        type_index: Some(&idx),
        ..Opts::DEFAULT
    };

    // (b) The scope use `empowered_legislative = { ... }` must validate (no CW262).
    let script = r#"
evt = {
    effect = {
        empowered_legislative = {
            set_character_flag = is_generic
        }
    }
}
"#;
    let errors = validate_with(&table, &ruleset, script, &opts);
    assert!(
        !errors
            .iter()
            .any(|e| e.message.contains("empowered_legislative") || e.code == Some("CW262")),
        "value-set member used as a scope key must not flag, got: {errors:?}",
    );

    // (c) Negative: a key that is NOT a value-set member, scope command, link, or
    // type instance still flags as an unexpected block.
    let bad = r#"
evt = {
    effect = {
        notathing = {
            set_character_flag = is_generic
        }
    }
}
"#;
    let errors_bad = validate_with(&table, &ruleset, bad, &opts);
    assert!(
        errors_bad.iter().any(|e| e.message.contains("notathing")),
        "a truly unknown scope key must still flag, got: {errors_bad:?}",
    );
}
