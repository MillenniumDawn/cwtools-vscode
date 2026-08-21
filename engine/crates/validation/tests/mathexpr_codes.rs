//! Strict math-expression validation (`math_expr` value type).
//!
//! HOI4 variable-math blocks (`set_variable = { x = { value = a subtract = b } }`)
//! are written as recursive math expressions: a `value` base plus a sequence of
//! operator keys (`add`/`subtract`/`multiply`/…). Before `math_expr` these were
//! modelled with a `single_alias_right[mathexprnest]` clause that sat next to a
//! permissive `= scalar` overload; the disjunction in `pick_best_candidate`
//! accepted the `scalar` branch on any block and the strict branch's
//! unexpected-key error was discarded, so a mis-typed operator silently became a
//! variable assignment. `math_expr` is validated authoritatively — outside that
//! disjunction — so a bogus operator key is flagged (CW263).

use cwtools_game::constants::Game;
use cwtools_index::TypeIndex;
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::{Prepared, build_scope_registry_arc, validate_prepared};

const RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    calc = math_expr
    ## cardinality = 0..1
    plain = scalar
}
alias[mathexpr:add] = math_expr
alias[mathexpr:subtract] = math_expr
alias[mathexpr:multiply] = math_expr
alias[mathexpr:clamp] = {
    min = variable_field
    max = variable_field
}
alias[mathexpr:round] = bool
"#;

fn codes(script: &str) -> Vec<String> {
    let table = StringTable::new();
    let parsed_cwt = parse_string(RULES, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);
    let parsed = parse_string(script, &table);
    let idx = TypeIndex::new();
    let registry = build_scope_registry_arc(&ruleset, Some(Game::Hoi4));
    let errors = validate_prepared(
        &parsed,
        "game/common/foo/test.txt",
        &Prepared {
            ruleset: &ruleset,
            table: &table,
            game: Some(Game::Hoi4),
            type_index: Some(&idx),
            modifier_keys: None,
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: false,
            var_checks: false,
        },
    );
    errors
        .into_iter()
        .filter_map(|e| e.code.map(String::from))
        .collect()
}

#[test]
fn valid_math_block_is_clean() {
    let c = codes("foo = { calc = { value = some_var subtract = other_var multiply = 2 } }");
    assert!(
        !c.contains(&"CW263".to_string()) && !c.contains(&"CW262".to_string()),
        "valid math operators must not flag, got: {:?}",
        c
    );
}

#[test]
fn typo_operator_is_cw263() {
    // `subtrac` is not a registered mathexpr operator, not `value`/`tooltip`.
    let c = codes("foo = { calc = { value = some_var subtrac = other_var } }");
    assert!(
        c.contains(&"CW263".to_string()),
        "mis-typed operator `subtrac` must flag CW263, got: {:?}",
        c
    );
}

#[test]
fn bogus_key_in_math_block_is_cw263() {
    let c = codes("foo = { calc = { add = 5 bogus_operator = 3 } }");
    assert!(
        c.contains(&"CW263".to_string()),
        "bogus key must flag CW263, got: {:?}",
        c
    );
}

#[test]
fn leaf_math_operand_is_clean() {
    // A bare number or variable reference is a valid math operand.
    let c = codes("foo = { calc = 5 }");
    assert!(c.is_empty(), "leaf operand must be clean, got: {:?}", c);
}

#[test]
fn nested_math_block_typo_is_cw263() {
    let c = codes("foo = { calc = { value = a add = { value = b subtrac = c } } }");
    assert!(
        c.contains(&"CW263".to_string()),
        "typo in a nested operand block must flag, got: {:?}",
        c
    );
}

// The real config models the direct `name = { math }` form as
// `value_set[variable] = math_expr` sitting NEXT TO a permissive
// `value_set[variable] = scalar` catch-all. The scalar overload accepts a block
// with zero errors, so without the authoritative bypass `pick_best_candidate`
// would discard the strict unexpected-key diagnostic. This guards that path.
const RULES_VALUE_SET: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    ## cardinality = 0..inf
    value_set[variable] = math_expr
    ## cardinality = 0..inf
    value_set[variable] = scalar
}
alias[mathexpr:add] = math_expr
alias[mathexpr:subtract] = math_expr
"#;

fn codes_value_set(script: &str) -> Vec<String> {
    let table = StringTable::new();
    let parsed_cwt = parse_string(RULES_VALUE_SET, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);
    let parsed = parse_string(script, &table);
    let idx = TypeIndex::new();
    let registry = build_scope_registry_arc(&ruleset, Some(Game::Hoi4));
    let errors = validate_prepared(
        &parsed,
        "game/common/foo/test.txt",
        &Prepared {
            ruleset: &ruleset,
            table: &table,
            game: Some(Game::Hoi4),
            type_index: Some(&idx),
            modifier_keys: None,
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: false,
            var_checks: false,
        },
    );
    errors
        .into_iter()
        .filter_map(|e| e.code.map(String::from))
        .collect()
}

#[test]
fn value_set_math_block_typo_flags_despite_scalar_overload() {
    // `t = { value=a subtrac=b }`: scalar overload accepts the block, but the
    // authoritative math_expr path must still flag the typo.
    let c = codes_value_set("foo = { t = { value = a subtrac = b } }");
    assert!(
        c.contains(&"CW263".to_string()),
        "typo must flag even with a competing `= scalar` overload, got: {:?}",
        c
    );
}

#[test]
fn value_set_scalar_assignment_is_clean() {
    // `t = 5` is a plain variable assignment (the scalar/leaf path), not a block.
    let c = codes_value_set("foo = { t = 5 }");
    assert!(
        c.is_empty(),
        "scalar assignment must stay clean, got: {:?}",
        c
    );
}

// The real config reaches math_expr through alias expansion: `set_variable` is
// an `alias[effect:set_variable]` with TWO overloads (direct `name = {math}` and
// explicit `var=N value={math}`), each carrying a permissive `= scalar`. This
// mirrors that exactly to guard the alias-usage disjunction path.
const RULES_ALIAS: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    alias_name[effect] = alias_match_left[effect]
}
alias[effect:set_variable] = {
    ## cardinality = 1..2
    value_set[variable] = variable_field
    ## cardinality = 1..2
    value_set[variable] = scope_field
    ## cardinality = 1..2
    value_set[variable] = scalar
    ## cardinality = 0..inf
    value_set[variable] = math_expr
}
alias[effect:set_variable] = {
    var = value_set[variable]
    value = variable_field
    value = scope_field
    value = math_expr
    value = scalar
}
alias[mathexpr:add] = math_expr
alias[mathexpr:subtract] = math_expr
alias[mathexpr:multiply] = math_expr
"#;

fn codes_alias(script: &str) -> Vec<String> {
    let table = StringTable::new();
    let parsed_cwt = parse_string(RULES_ALIAS, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);
    let parsed = parse_string(script, &table);
    let idx = TypeIndex::new();
    let registry = build_scope_registry_arc(&ruleset, Some(Game::Hoi4));
    let errors = validate_prepared(
        &parsed,
        "game/common/foo/test.txt",
        &Prepared {
            ruleset: &ruleset,
            table: &table,
            game: Some(Game::Hoi4),
            type_index: Some(&idx),
            modifier_keys: None,
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: false,
            var_checks: false,
        },
    );
    errors
        .into_iter()
        .filter_map(|e| e.code.map(String::from))
        .collect()
}

#[test]
fn alias_effect_direct_math_block_typo_is_cw263() {
    let c = codes_alias("foo = { set_variable = { t = { value = a subtrac = b } } }");
    assert!(
        c.contains(&"CW263".to_string()),
        "typo in alias-reached direct math block must flag, got: {:?}",
        c
    );
}

#[test]
fn alias_effect_explicit_value_math_block_typo_is_cw263() {
    let c = codes_alias("foo = { set_variable = { var = v value = { value = a subtrac = b } } }");
    assert!(
        c.contains(&"CW263".to_string()),
        "typo in alias-reached explicit value math block must flag, got: {:?}",
        c
    );
}

#[test]
fn alias_effect_valid_math_is_clean() {
    let c = codes_alias("foo = { set_variable = { t = { value = a add = b multiply = 2 } } }");
    assert!(
        !c.contains(&"CW263".to_string()) && !c.contains(&"CW262".to_string()),
        "valid alias-reached math must be clean, got: {:?}",
        c
    );
}

// A trigger whose whole body is a math expression (`check_expr = math_expr`),
// reached through `alias_name[trigger]`, must validate strictly even when a
// permissive sibling overload (here `= scalar`, standing in for the unpopulated
// `enum[...]` pattern overload that matches every trigger key in the real
// config) would accept the block cleanly. Guards the validate_alias_usage
// authoritative bypass.
const RULES_TRIGGER: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    alias_name[trigger] = alias_match_left[trigger]
}
alias[trigger:check_expr] = math_expr
alias[trigger:check_expr] = scalar
alias[mathexpr:add] = math_expr
alias[mathexpr:subtract] = math_expr
"#;

fn codes_trigger(script: &str) -> Vec<String> {
    let table = StringTable::new();
    let parsed_cwt = parse_string(RULES_TRIGGER, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);
    let parsed = parse_string(script, &table);
    let idx = TypeIndex::new();
    let registry = build_scope_registry_arc(&ruleset, Some(Game::Hoi4));
    let errors = validate_prepared(
        &parsed,
        "game/common/foo/test.txt",
        &Prepared {
            ruleset: &ruleset,
            table: &table,
            game: Some(Game::Hoi4),
            type_index: Some(&idx),
            modifier_keys: None,
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: false,
            var_checks: false,
        },
    );
    errors
        .into_iter()
        .filter_map(|e| e.code.map(String::from))
        .collect()
}

#[test]
fn math_expr_trigger_typo_flags_despite_permissive_overload() {
    let c = codes_trigger("foo = { check_expr = { value = a subtrac = b } }");
    assert!(
        c.contains(&"CW263".to_string()),
        "typo in a math_expr trigger must flag despite a permissive sibling overload, got: {:?}",
        c
    );
}

#[test]
fn math_expr_trigger_valid_is_clean() {
    let c = codes_trigger("foo = { check_expr = { value = a subtract = b } }");
    assert!(
        !c.contains(&"CW263".to_string()),
        "valid math_expr trigger must be clean, got: {:?}",
        c
    );
}

#[test]
fn operator_with_explicit_clause_arg_is_clean() {
    // `clamp` takes an explicit `{ min max }` clause, not a math expression.
    let c = codes("foo = { calc = { value = a clamp = { min = lo max = hi } } }");
    assert!(
        !c.contains(&"CW263".to_string()) && !c.contains(&"CW262".to_string()),
        "clamp's explicit clause must validate, got: {:?}",
        c
    );
}
