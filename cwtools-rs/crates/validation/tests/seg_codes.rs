//! Tier S/G scope emissions (on by default; `CWTOOLS_NO_SCOPE_CHECKS=1` disables):
//! - CW235 zero-modifier (a known modifier set to 0)
//! - CW247 rule-wrong-scope (reconciled from the Rust-invented CW400)
//! - CW104 trigger-wrong-scope (alias scope check)
//! - root scope seeding via `## replace_scope` (state-history `state` object)

use cwtools_game::constants::Game;
use cwtools_index::{SourceLocation, TypeIndex, TypeInstance};
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::{Prepared, build_scope_registry_arc, validate_prepared};
use std::collections::{HashMap, HashSet};

fn errors_hoi4(cwt: &str, script: &str) -> Vec<cwtools_validation::ValidationError> {
    errors_hoi4_with_index(cwt, script, None)
}

fn errors_hoi4_with_index(
    cwt: &str,
    script: &str,
    type_index: Option<&TypeIndex>,
) -> Vec<cwtools_validation::ValidationError> {
    errors_hoi4_at(cwt, "game/common/foo/test.txt", script, type_index)
}

fn errors_hoi4_at(
    cwt: &str,
    path: &str,
    script: &str,
    type_index: Option<&TypeIndex>,
) -> Vec<cwtools_validation::ValidationError> {
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);
    let parsed = parse_string(script, &table);
    let registry = build_scope_registry_arc(&ruleset, Some(Game::Hoi4));
    validate_prepared(
        &parsed,
        path,
        &Prepared {
            ruleset: &ruleset,
            table: &table,
            game: Some(Game::Hoi4),
            type_index,
            modifier_keys: None,
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: true,
            var_checks: false,
        },
    )
}

fn codes_hoi4(cwt: &str, script: &str) -> Vec<String> {
    errors_hoi4(cwt, script)
        .into_iter()
        .filter_map(|e| e.code.map(String::from))
        .collect()
}

fn codes_hoi4_at(cwt: &str, path: &str, script: &str) -> Vec<String> {
    errors_hoi4_at(cwt, path, script, None)
        .into_iter()
        .filter_map(|e| e.code.map(String::from))
        .collect()
}

/// `foo` validates at the default country scope. A `## scope = state` trigger
/// used directly in it must produce CW104; a `## scope = country` one stays clean.
const SCOPE_RULES: &str = r#"
scopes = {
    Country = { aliases = { country } }
    State = { aliases = { state } }
}
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    alias_name[trigger] = alias_match_left[trigger]
}
## scope = country
alias[trigger:country_only] = bool
## scope = state
alias[trigger:state_only] = bool
"#;

#[test]
fn state_trigger_in_country_scope_is_cw104() {
    let c = codes_hoi4(SCOPE_RULES, "foo = { state_only = yes }");
    assert!(c.contains(&"CW104".to_string()), "got: {:?}", c);
}

#[test]
fn country_trigger_in_country_scope_is_clean() {
    let c = codes_hoi4(SCOPE_RULES, "foo = { country_only = yes }");
    assert!(!c.contains(&"CW104".to_string()), "got: {:?}", c);
}

const MIXED_CASE_BLOCK_RULES: &str = r#"
scopes = {
    Country = { aliases = { country } }
    State = { aliases = { state } }
}
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    alias_name[trigger] = alias_match_left[trigger]
}
## scope = country
alias[trigger:country_block] = {
    alias_name[trigger] = alias_match_left[trigger]
}
alias[trigger:scope_field] = {
    alias_name[trigger] = alias_match_left[trigger]
}
## scope = state
alias[trigger:state_only] = bool
"#;

#[test]
fn mixed_case_block_keeps_parent_scope() {
    let c = codes_hoi4(
        MIXED_CASE_BLOCK_RULES,
        "foo = { country_Block = { state_only = yes } }",
    );
    assert!(c.contains(&"CW104".to_string()), "got: {:?}", c);
}

#[test]
fn indexed_instance_block_remains_lenient() {
    let mut index = TypeIndex::new();
    index.merge(
        "file://characters.txt",
        HashMap::from([(
            "character".to_string(),
            vec![TypeInstance {
                name: "RUS_known_character".to_string(),
                location: SourceLocation {
                    line: 1,
                    col: 0,
                    end: (1, 0),
                },
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            }],
        )]),
    );
    let c = errors_hoi4_with_index(
        MIXED_CASE_BLOCK_RULES,
        "foo = { RUS_known_character = { state_only = yes } }",
        Some(&index),
    )
    .into_iter()
    .filter_map(|e| e.code.map(String::from))
    .collect::<Vec<_>>();
    assert!(!c.contains(&"CW104".to_string()), "got: {:?}", c);
}

/// A block-form trigger in the wrong scope: the complaint names the trigger
/// key, so the squiggle covers the key token, not the whole block it opens.
const SCOPE_BLOCK_RULES: &str = r#"
scopes = {
    Country = { aliases = { country } }
    State = { aliases = { state } }
}
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    alias_name[trigger] = alias_match_left[trigger]
}
## scope = state
alias[trigger:state_block] = {
    x = bool
}
"#;

#[test]
fn cw104_underlines_only_the_trigger_key() {
    let errs = errors_hoi4(
        SCOPE_BLOCK_RULES,
        "foo = {\n    state_block = {\n        x = yes\n    }\n}\n",
    );
    let err = errs
        .iter()
        .find(|e| e.code == Some("CW104"))
        .expect("CW104 emitted");
    assert_eq!((err.line, err.col), (2, 4));
    assert_eq!(
        err.end,
        Some((2, 4 + "state_block".len() as u16)),
        "CW104 must span only the key"
    );
}

/// A type whose root rule seeds state scope via `## replace_scope` should make a
/// state-only effect inside it clean (mirrors history/states `state` object).
const REPLACE_SCOPE_RULES: &str = r#"
scopes = {
    Country = { aliases = { country } }
    State = { aliases = { state } }
}
types = { type[st] = { path = "game/common/foo" } }
## replace_scope = { this = state root = state }
st = {
    inner = {
        alias_name[effect] = alias_match_left[effect]
    }
}
## scope = state
alias[effect:state_fx] = bool
"#;

#[test]
fn replace_scope_seeds_root_state_scope() {
    // state_fx (## scope = state) inside a replace_scope=state type: no CW105.
    let c = codes_hoi4(REPLACE_SCOPE_RULES, "st = { inner = { state_fx = yes } }");
    assert!(!c.contains(&"CW105".to_string()), "got: {:?}", c);
}

/// A `scope[country]` target field. Resolving the chain from the default country
/// scope catches a target that lands in the wrong scope (CW243) or uses a link in
/// the wrong scope (CW245).
const TARGET_RULES: &str = r#"
scopes = {
    Country = { aliases = { country } }
    State = { aliases = { state } }
}
links = {
    capital_scope = { output_scope = state input_scopes = country }
    controller = { output_scope = country input_scopes = state }
    faction_leader = { output_scope = country input_scopes = country }
}
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    tgt = scope[country]
}
"#;

#[test]
fn target_resolves_to_wrong_scope_is_cw243() {
    // capital_scope: country -> state, but the field wants country.
    let c = codes_hoi4(TARGET_RULES, "foo = { tgt = capital_scope }");
    assert!(c.contains(&"CW243".to_string()), "got: {:?}", c);
}

#[test]
fn link_used_in_wrong_scope_is_cw245() {
    // controller is only valid in state scope; used here from country.
    let c = codes_hoi4(TARGET_RULES, "foo = { tgt = controller }");
    assert!(c.contains(&"CW245".to_string()), "got: {:?}", c);
}

#[test]
fn target_resolving_to_country_is_clean() {
    // faction_leader: country -> country, matches the field. No target error.
    let c = codes_hoi4(TARGET_RULES, "foo = { tgt = faction_leader }");
    assert!(!c.contains(&"CW243".to_string()), "got: {:?}", c);
    assert!(!c.contains(&"CW245".to_string()), "got: {:?}", c);
}

/// `Character is_subscope_of { country }`, so a `## scope = country` trigger is
/// valid inside a character scope and must NOT produce CW104.
const SUBSCOPE_RULES: &str = r#"
scopes = {
    Country = { aliases = { country } }
    Character = { aliases = { character } is_subscope_of = { country } }
}
links = {
    character = { output_scope = character input_scopes = country }
}
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    ## push_scope = character
    char_block = { alias_name[trigger] = alias_match_left[trigger] }
}
## scope = country
alias[trigger:country_only] = bool
"#;

#[test]
fn country_trigger_in_character_subscope_is_clean() {
    let c = codes_hoi4(
        SUBSCOPE_RULES,
        "foo = { char_block = { country_only = yes } }",
    );
    assert!(!c.contains(&"CW104".to_string()), "got: {:?}", c);
}

#[test]
fn zero_known_modifier_is_cw235() {
    let table = StringTable::new();
    // A rules file with a foo type whose body takes no fixed fields, so the
    // modifier key falls through to the dynamic-modifier accept path.
    let cwt = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    dummy = scalar
}
"#;
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    let mut modifiers = HashSet::new();
    modifiers.insert("attack_factor".to_string());

    let script = r#"
foo = {
    attack_factor = 0
}
"#;
    let parsed = parse_string(script, &table);
    let registry = build_scope_registry_arc(&ruleset, Some(Game::Hoi4));
    let errors = validate_prepared(
        &parsed,
        "game/common/foo/test.txt",
        &Prepared {
            ruleset: &ruleset,
            table: &table,
            game: Some(Game::Hoi4),
            type_index: None,
            modifier_keys: Some(&modifiers),
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: true,
            var_checks: false,
        },
    );
    let codes: Vec<String> = errors
        .into_iter()
        .filter_map(|e| e.code.map(String::from))
        .collect();
    assert!(codes.contains(&"CW235".to_string()), "got: {:?}", codes);
}

#[test]
fn nonzero_known_modifier_is_clean() {
    let table = StringTable::new();
    let cwt = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    dummy = scalar
}
"#;
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    let mut modifiers = HashSet::new();
    modifiers.insert("attack_factor".to_string());

    let script = r#"
foo = {
    attack_factor = 0.05
}
"#;
    let parsed = parse_string(script, &table);
    let registry = build_scope_registry_arc(&ruleset, Some(Game::Hoi4));
    let errors = validate_prepared(
        &parsed,
        "game/common/foo/test.txt",
        &Prepared {
            ruleset: &ruleset,
            table: &table,
            game: Some(Game::Hoi4),
            type_index: None,
            modifier_keys: Some(&modifiers),
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: true,
            var_checks: false,
        },
    );
    let codes: Vec<String> = errors
        .into_iter()
        .filter_map(|e| e.code.map(String::from))
        .collect();
    assert!(!codes.contains(&"CW235".to_string()), "got: {:?}", codes);
}

/// Validate `script` against a content-free `foo` type with `modifiers` as the
/// dynamic-modifier set. Returns the emitted codes. The modifier set is the
/// lowercase canonical form the loader builds.
fn modifier_codes(modifiers: &[&str], script: &str) -> Vec<String> {
    let cwt = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    dummy = scalar
}
"#;
    modifier_codes_with_rules(cwt, modifiers, script)
}

/// As [`modifier_codes`], but against a caller-supplied ruleset, for the cases
/// where the modifier key has to match a rule.
fn modifier_codes_with_rules(cwt: &str, modifiers: &[&str], script: &str) -> Vec<String> {
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(cwt, &table), &table);
    let mods: HashSet<String> = modifiers.iter().map(|m| m.to_string()).collect();
    let parsed = parse_string(script, &table);
    let registry = build_scope_registry_arc(&ruleset, Some(Game::Hoi4));
    validate_prepared(
        &parsed,
        "game/common/foo/test.txt",
        &Prepared {
            ruleset: &ruleset,
            table: &table,
            game: Some(Game::Hoi4),
            type_index: None,
            modifier_keys: Some(&mods),
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: true,
            var_checks: false,
        },
    )
    .into_iter()
    .filter_map(|e| e.code.map(String::from))
    .collect()
}

#[test]
fn mixed_case_modifier_key_accepted_silently() {
    // The modifier set is lowercase; a mixed-case leaf key with no candidate rule
    // must be lowercased before the membership test. After `key_lower` went lazy
    // (computed only in the no-candidate branch), a mixed-case key would flag
    // CW262/CW263 if the lowercasing were dropped — so this pins it.
    let c = modifier_codes(&["attack_factor"], "foo = { Attack_Factor = 0.05 }");
    assert!(
        !c.contains(&"CW263".to_string()) && !c.contains(&"CW262".to_string()),
        "a mixed-case known modifier must be accepted silently (lazy key_lower must \
         still lowercase), got: {:?}",
        c
    );
}

#[test]
fn mixed_case_zero_modifier_still_fires_cw235() {
    // CW235 fires on a confirmed modifier set to 0. With a mixed-case key this only
    // works if the lazy lowercase form is matched against the (lowercase) modifier
    // set — proving the lowercase path survived the lazy refactor.
    let c = modifier_codes(&["attack_factor"], "foo = { Attack_Factor = 0 }");
    assert!(
        c.contains(&"CW235".to_string()),
        "a mixed-case zero modifier must still fire CW235, got: {:?}",
        c
    );
}

/// A `modifier = { ... }` block whose contents are matched through the
/// `modifier` alias, plus a rule field of the type's own that shares a modifier
/// name. The shape every real modifier block in the configs has.
const MODIFIER_BLOCK_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    modifier = {
        alias_name[modifier] = alias_match_left[modifier]
    }
    factor = float
}
alias[modifier:attack_factor] = float
alias[modifier:factor] = float
"#;

#[test]
fn rule_matched_zero_modifier_is_cw235() {
    // The key matches the `alias_name[modifier]` rule, so it never reached the
    // no-candidate branch the check used to live in.
    let c = modifier_codes_with_rules(
        MODIFIER_BLOCK_RULES,
        &["attack_factor", "factor"],
        "foo = { modifier = { attack_factor = 0 } }",
    );
    assert!(
        c.contains(&"CW235".to_string()),
        "a rule-matched zero modifier must fire CW235, got: {:?}",
        c
    );
}

#[test]
fn rule_matched_nonzero_modifier_is_clean() {
    let c = modifier_codes_with_rules(
        MODIFIER_BLOCK_RULES,
        &["attack_factor", "factor"],
        "foo = { modifier = { attack_factor = 0.05 } }",
    );
    assert!(!c.contains(&"CW235".to_string()), "got: {:?}", c);
}

#[test]
fn zero_rule_field_sharing_a_modifier_name_is_clean() {
    // `factor` is a field of `foo`'s own rules AND a registered modifier. It
    // matched a SpecificField rule, not the modifier alias, so a zero is a
    // legitimate value here and must not read as a no-op modifier.
    let c = modifier_codes_with_rules(
        MODIFIER_BLOCK_RULES,
        &["attack_factor", "factor"],
        "foo = { factor = 0 }",
    );
    assert!(
        !c.contains(&"CW235".to_string()),
        "a zero rule field must not fire CW235, got: {:?}",
        c
    );
}

/// A trigger that matches a key ONLY via an unpopulated game-derived enum must
/// not inherit that alias's `## scope`. Regression for the resource-in-state
/// false positive: `oil` matched an empty `enum[equipment_category]` (scope
/// unit_leader/combat) when resources weren't indexed, flagging a bogus CW104.
const EMPTY_ENUM_RULES: &str = r#"
scopes = {
    Country = { aliases = { country } }
    State = { aliases = { state } }
    "Unit Leader" = { aliases = { unit_leader } }
}
enums = { enum[empty_e] = { } }
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    alias_name[trigger] = alias_match_left[trigger]
}
## scope = { unit_leader }
alias[trigger:enum[empty_e]] = int
## scope = { unit_leader }
alias[trigger:skill] = int
"#;

#[test]
fn empty_enum_only_match_does_not_cw104() {
    // `oil` matches only the empty enum (permissively) -> no confident overload
    // -> no scope check -> no false CW104 in country scope.
    let c = codes_hoi4(EMPTY_ENUM_RULES, "foo = { oil = 1 }");
    assert!(!c.contains(&"CW104".to_string()), "got: {:?}", c);
}

#[test]
fn confident_literal_trigger_still_cw104() {
    // `skill` is an exact (confident) unit_leader trigger; in country scope it
    // must still fire CW104 — the fix only suppresses uncertain matches.
    let c = codes_hoi4(EMPTY_ENUM_RULES, "foo = { skill = 1 }");
    assert!(c.contains(&"CW104".to_string()), "got: {:?}", c);
}

/// A modifier's `## scope` is its CATEGORY (where it takes effect), not a
/// write-location constraint: a country idea/national-spirit `modifier = {}`
/// block legitimately carries state-category modifiers that cascade to the
/// country's owned states. So a `## scope = state` modifier used in a country
/// scope must NOT fire CW106 — unlike a same-scoped trigger, which still does.
const MODIFIER_SCOPE_RULES: &str = r#"
scopes = {
    Country = { aliases = { country } }
    State = { aliases = { state } }
}
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    alias_name[modifier] = alias_match_left[modifier]
    alias_name[trigger] = alias_match_left[trigger]
}
## scope = state
alias[modifier:state_only_mod] = int
## scope = state
alias[trigger:state_only_trig] = bool
"#;

#[test]
fn state_modifier_in_country_scope_is_not_cw106() {
    let c = codes_hoi4(MODIFIER_SCOPE_RULES, "foo = { state_only_mod = 5 }");
    assert!(!c.contains(&"CW106".to_string()), "got: {:?}", c);
}

#[test]
fn state_trigger_in_country_scope_still_cw104() {
    // The modifier exemption must not leak into trigger scope checking.
    let c = codes_hoi4(MODIFIER_SCOPE_RULES, "foo = { state_only_trig = yes }");
    assert!(c.contains(&"CW104".to_string()), "got: {:?}", c);
}

/// A bare integer scope block (`129 = { ... }`) is a HOI4 state scope, so a
/// state-only trigger inside it is clean and a country-only one is CW104. A
/// numeric key matched as an explicit `int` field (random_list weight) keeps the
/// current scope instead.
const NUMERIC_STATE_RULES: &str = r#"
scopes = {
    Country = { aliases = { country } }
    State = { aliases = { state } }
}
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    alias_name[effect] = alias_match_left[effect]
}
alias[effect:scope_field] = {
    alias_name[trigger] = alias_match_left[trigger]
    alias_name[effect] = alias_match_left[effect]
}
alias[effect:random_list] = {
    int = {
        alias_name[trigger] = alias_match_left[trigger]
        alias_name[effect] = alias_match_left[effect]
    }
}
## scope = state
alias[trigger:state_only] = bool
## scope = country
alias[trigger:country_only] = bool
"#;

#[test]
fn numeric_block_is_state_scope() {
    // 129 -> state, so a state-only trigger inside is clean.
    let c = codes_hoi4(NUMERIC_STATE_RULES, "foo = { 129 = { state_only = yes } }");
    assert!(
        !c.contains(&"CW104".to_string()),
        "state_only in 129 should be clean: {:?}",
        c
    );
}

#[test]
fn country_trigger_in_numeric_state_block_is_cw104() {
    // 129 -> state, so a country-only trigger inside is wrong-scope.
    let c = codes_hoi4(
        NUMERIC_STATE_RULES,
        "foo = { 129 = { country_only = yes } }",
    );
    assert!(
        c.contains(&"CW104".to_string()),
        "country_only in state 129 should be CW104: {:?}",
        c
    );
}

#[test]
fn random_list_weight_keeps_current_scope() {
    // The int weight bucket is NOT a state scope; a country-only trigger inside
    // stays valid at the country root.
    let c = codes_hoi4(
        NUMERIC_STATE_RULES,
        "foo = { random_list = { 10 = { country_only = yes } } }",
    );
    assert!(
        !c.contains(&"CW104".to_string()),
        "country_only in random_list weight should be clean: {:?}",
        c
    );
}

/// An event's `## push_scope` must seed ROOT, not just the current scope: a
/// `ROOT = { … }` block inside a `unit_leader_event` (push_scope = any, for
/// HOI4's hybrid country/leader event scope) must not be scope-checked against
/// the country default. Regression for the CW105 false positive on
/// `ROOT = { add_max_trait = 1 }` (issue #152); `state_event` shares the hybrid
/// and the bug.
const EVENT_ROOT_SCOPE_RULES: &str = r#"
scopes = {
    Country = { aliases = { country } }
    State = { aliases = { state } }
    Character = { aliases = { character } }
    "Unit Leader" = { aliases = { unit_leader } }
}
types = {
    type[event] = {
        path = "game/events"
        name_field = "id"
        ## type_key_filter = country_event
        ## push_scope = country
        subtype[country_event] = {
        }
        ## type_key_filter = unit_leader_event
        ## push_scope = any
        subtype[unit_leader_event] = {
        }
        ## type_key_filter = state_event
        ## push_scope = any
        subtype[state_event] = {
        }
        subtype[hidden] = {
            hidden = yes
        }
        ## only_if_not = hidden
        subtype[visible] = {
        }
    }
}
event = {
    id = scalar
    subtype[hidden] = {
        title = scalar
        desc = scalar
    }
    subtype[visible] = {
        title = localisation
        desc = localisation
    }
    ## cardinality = 0..1
    is_triggered_only = yes
    ## cardinality = 0..inf
    option = {
        alias_name[effect] = alias_match_left[effect]
    }
}
## scope = { character unit_leader }
alias[effect:add_max_trait] = int
## scope = country
alias[effect:country_effect] = bool
## scope = state
alias[effect:state_effect] = bool
alias[effect:scope_field] = {
    alias_name[effect] = alias_match_left[effect]
}
"#;

#[test]
fn unit_leader_event_root_is_lenient() {
    // unit_leader_event: push_scope = any -> ROOT is the wildcard, so a
    // unit_leader-only effect inside ROOT must stay clean (was "In country").
    let c = codes_hoi4_at(
        EVENT_ROOT_SCOPE_RULES,
        "game/events/test.txt",
        "unit_leader_event = {\n\tid = evt.1\n\tis_triggered_only = yes\n\toption = {\n\t\tROOT = {\n\t\t\tadd_max_trait = 1\n\t\t}\n\t}\n}\n",
    );
    assert!(!c.contains(&"CW105".to_string()), "got: {:?}", c);
}

#[test]
fn state_event_root_is_lenient() {
    // state_event shares the hybrid: ROOT must be the wildcard, not country.
    let c = codes_hoi4_at(
        EVENT_ROOT_SCOPE_RULES,
        "game/events/test.txt",
        "state_event = {\n\tid = evt.2\n\tis_triggered_only = yes\n\toption = {\n\t\tROOT = {\n\t\t\tstate_effect = yes\n\t\t}\n\t}\n}\n",
    );
    assert!(!c.contains(&"CW105".to_string()), "got: {:?}", c);
}

#[test]
fn country_event_root_still_checked() {
    // country_event: push_scope = country -> ROOT = country, so a
    // unit_leader-only effect inside ROOT must still fire CW105.
    let c = codes_hoi4_at(
        EVENT_ROOT_SCOPE_RULES,
        "game/events/test.txt",
        "country_event = {\n\tid = evt.3\n\tis_triggered_only = yes\n\toption = {\n\t\tROOT = {\n\t\t\tadd_max_trait = 1\n\t\t}\n\t}\n}\n",
    );
    assert!(c.contains(&"CW105".to_string()), "got: {:?}", c);
}

#[test]
fn hybrid_event_accepts_country_effects_at_root() {
    // The hybrid event scope is lenient both ways: a country-only effect inside
    // ROOT of a unit_leader_event is valid too.
    let c = codes_hoi4_at(
        EVENT_ROOT_SCOPE_RULES,
        "game/events/test.txt",
        "unit_leader_event = {\n\tid = evt.4\n\tis_triggered_only = yes\n\toption = {\n\t\tcountry_effect = yes\n\t\tROOT = {\n\t\t\tcountry_effect = yes\n\t\t}\n\t}\n}\n",
    );
    assert!(!c.contains(&"CW105".to_string()), "got: {:?}", c);
}
