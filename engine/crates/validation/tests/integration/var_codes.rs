//! Variable-tracking emissions:
//! - CW271 (int-only) / CW270 (3-decimal precision): local numeric checks on a
//!   `variable_field`, on regardless of any gate.
//! - CW246 (variable has not been set): a non-numeric `variable_field` value
//!   that names no defined variable. Gated behind var_checks and driven
//!   by the project variable index.

use cwtools_index::TypeIndex;
use cwtools_validation::Game;

use super::common::{Opts, codes};

const FOO_FILE: Opts<'static> = Opts {
    path: "game/common/foo/test.txt",
    game: Some(Game::Hoi4),
    ..Opts::DEFAULT
};

/// A project variable index that holds `names`.
fn vars(names: &[&str]) -> TypeIndex {
    let mut idx = TypeIndex::new();
    for name in names {
        idx.var_index.add_name(name);
    }
    idx
}

/// As [`vars`], with `vanilla` as the base game's variables.
fn vanilla_vars(vanilla: &[&str], names: &[&str]) -> TypeIndex {
    let mut idx = TypeIndex::new();
    idx.var_index
        .set_vanilla_names(vanilla.iter().map(|s| s.to_string()).collect());
    for name in names {
        idx.var_index.add_name(name);
    }
    idx
}

fn with_index(idx: &TypeIndex) -> Opts<'_> {
    Opts {
        type_index: Some(idx),
        ..FOO_FILE
    }
}

const RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
values = {
    value[variable] = {
        faction_leader
        party_popularity@<ideology>
        opinion@enum[country_tags]
    }
}
foo = {
    add = int_variable_field
    prec = variable_field_32
    ref = variable_field
    get = value[variable]
    flag = value[country_flag]
    arr = value[array]
}
"#;

#[test]
fn int_field_with_fraction_is_cw271() {
    let c = codes(RULES, "foo = { add = 5.5 }", &with_index(&vars(&[])));
    assert!(c.contains(&"CW271".to_string()), "got: {:?}", c);
}

#[test]
fn int_field_with_integer_is_clean() {
    let c = codes(RULES, "foo = { add = 5 }", &with_index(&vars(&[])));
    assert!(!c.contains(&"CW271".to_string()), "got: {:?}", c);
}

#[test]
fn precision_field_over_three_decimals_is_cw270() {
    let c = codes(RULES, "foo = { prec = 0.12345 }", &with_index(&vars(&[])));
    assert!(c.contains(&"CW270".to_string()), "got: {:?}", c);
}

#[test]
fn precision_field_three_decimals_is_clean() {
    let c = codes(RULES, "foo = { prec = 0.123 }", &with_index(&vars(&[])));
    assert!(!c.contains(&"CW270".to_string()), "got: {:?}", c);
}

#[test]
fn undefined_variable_is_cw246() {
    // Index has a different variable, so it is non-empty but lacks `mystery`.
    let c = codes(
        RULES,
        "foo = { ref = mystery }",
        &with_index(&vars(&["something_else"])),
    );
    assert!(c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn defined_variable_is_clean() {
    let c = codes(
        RULES,
        "foo = { ref = my_var }",
        &with_index(&vars(&["my_var"])),
    );
    assert!(!c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn builtin_variable_field_is_clean() {
    // `faction_leader` is a config-declared built-in variable (a `value[variable]`
    // member). It's valid in a variable_field even without the `var:` prefix and
    // is never "set", so it must not flag CW246.
    let c = codes(
        RULES,
        "foo = { ref = faction_leader }",
        &with_index(&vars(&["something_else"])),
    );
    assert!(!c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn builtin_with_scope_suffix_is_clean() {
    // variables.cwt declares the whole family as `party_popularity@<ideology>`,
    // and a read carries its own suffix (`party_popularity@social_democrat`), so
    // the match has to compare the base name on both sides of the `@`.
    let idx = vars(&["something_else"]);
    let c = codes(
        RULES,
        "foo = { ref = party_popularity@social_democrat }",
        &with_index(&idx),
    );
    assert!(!c.contains(&"CW246".to_string()), "got: {:?}", c);
    let c = codes(RULES, "foo = { get = opinion@RUS }", &with_index(&idx));
    assert!(!c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn builtin_variable_get_is_clean() {
    let c = codes(
        RULES,
        "foo = { get = faction_leader }",
        &with_index(&vars(&["something_else"])),
    );
    assert!(!c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn var_prefixed_reference_is_clean() {
    // The explicit `var:` form is already accepted; keep it that way.
    let c = codes(
        RULES,
        "foo = { ref = var:faction_leader }",
        &with_index(&vars(&["something_else"])),
    );
    assert!(!c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn numeric_value_is_never_cw246() {
    let c = codes(
        RULES,
        "foo = { ref = 3 }",
        &with_index(&vars(&["something_else"])),
    );
    assert!(!c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn at_variable_is_never_cw246() {
    let c = codes(
        RULES,
        "foo = { ref = @my_const }",
        &with_index(&vars(&["something_else"])),
    );
    assert!(!c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn macro_parameter_name_is_never_cw246() {
    // A scripted effect builds the name from its `$ARG$` parameters, so the
    // written token is not the runtime name and can never be looked up.
    let idx = vars(&["something_else"]);
    let c = codes(RULES, "foo = { ref = $TAG$_stability }", &with_index(&idx));
    assert!(!c.contains(&"CW246".to_string()), "got: {:?}", c);
    let c = codes(RULES, "foo = { get = prefix_$ARG$ }", &with_index(&idx));
    assert!(!c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn variable_get_field_defined_is_clean() {
    let c = codes(
        RULES,
        "foo = { get = my_var }",
        &with_index(&vars(&["my_var"])),
    );
    assert!(!c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn variable_get_field_undefined_is_cw246() {
    let c = codes(
        RULES,
        "foo = { get = mystery }",
        &with_index(&vars(&["something_else"])),
    );
    assert!(c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn other_value_namespace_is_never_cw246() {
    // `value[country_flag]` reads a country flag, not a variable. The variable
    // index only ever holds `value_set[variable]` names, so checking any other
    // namespace against it flags every flag/array/token read in the corpus.
    let c = codes(
        RULES,
        "foo = { flag = my_flag }",
        &with_index(&vars(&["something_else"])),
    );
    assert!(!c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn array_namespace_is_never_cw246() {
    let c = codes(
        RULES,
        "foo = { arr = my_array }",
        &with_index(&vars(&["something_else"])),
    );
    assert!(!c.contains(&"CW246".to_string()), "got: {:?}", c);
}

// ── Issue #31: `?<default>` selector on a variable-defining key ────────────────
// `subtract_from_variable = { my_var?150 = { ... } }` (and the pre-TAOG `= 100`
// form) must validate clean. The `?150` null-coalescing default used to split
// the key at the `?` in the parser, so `my_var?150` became an orphan bare value
// (CW264) and the `{...}` an orphan value clause (CW265). The bare `my_var` form
// was always clean. This pins the selector form to the bare form's behavior.

const VARSET_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    subtract_from_variable = {
        ## cardinality = 0..inf
        value_set[variable] = variable_field
    }
}
"#;

#[test]
fn varset_bare_key_is_clean() {
    // Baseline: the un-suffixed key must not flag (it never did).
    let c = codes(
        VARSET_RULES,
        "foo = { subtract_from_variable = { war_propaganda_decision_cost = { value = 150 } } }",
        &with_index(&vars(&[])),
    );
    assert!(!c.contains(&"CW264".to_string()), "got: {:?}", c);
    assert!(!c.contains(&"CW265".to_string()), "got: {:?}", c);
}

#[test]
fn varset_question_selector_clause_form_is_clean() {
    let c = codes(
        VARSET_RULES,
        "foo = { subtract_from_variable = { war_propaganda_decision_cost?150 = { value = 150 } } }",
        &with_index(&vars(&[])),
    );
    assert!(
        !c.contains(&"CW264".to_string()) && !c.contains(&"CW265".to_string()),
        "the `?150` selector form must validate like the bare form, got: {:?}",
        c
    );
}

#[test]
fn varset_question_selector_pretaog_leaf_form_is_clean() {
    // Pre-TAOG shorthand: `my_var?150 = 100`.
    let c = codes(
        VARSET_RULES,
        "foo = { subtract_from_variable = { war_propaganda_decision_cost?150 = 100 } }",
        &with_index(&vars(&[])),
    );
    assert!(
        !c.contains(&"CW264".to_string()) && !c.contains(&"CW265".to_string()),
        "the pre-TAOG `?150 = 100` form must validate clean, got: {:?}",
        c
    );
}

// ── Issue #30: loop effects expose implicit element/index/break variables ──────
// `for_each_loop = { array = ... value = v ... }` exposes a `value_set[variable]`
// the loop body can read bare (default `v`, or whatever `value = NAME` declares).
// Without seeding those names into the per-block known-variable set, a bare read
// in the body flags CW246. The explicit `var:NAME` form already resolves; this
// pins the bare form to it. Scoped to the loop body only.

// Run with a non-empty variable index that holds `my_existing_var` (so CW246
// *can* fire) but none of the loop locals.
const LOOP_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    alias_name[effect] = alias_match_left[effect]
}
## scope = any
alias[effect:for_each_loop] = {
    array = scalar
    ## cardinality = 0..1
    value = value_set[variable]
    ## cardinality = 0..1
    index = value_set[variable]
    ## cardinality = 0..1
    break = value_set[variable]
    alias_name[effect] = alias_match_left[effect]
}
## scope = any
alias[effect:while_loop] = {
    ## cardinality = 0..1
    value = value_set[variable]
    ## cardinality = 0..1
    index = value_set[variable]
    alias_name[effect] = alias_match_left[effect]
}
## scope = any
alias[effect:use_var] = variable_field
"#;

#[test]
fn loop_default_value_var_is_clean() {
    // Bare `v` (the default element variable) inside the loop body must not flag.
    let c = codes(
        LOOP_RULES,
        "foo = { for_each_loop = { array = my_arr use_var = v } }",
        &with_index(&vars(&["my_existing_var"])),
    );
    assert!(!c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn loop_default_index_var_is_clean() {
    // Bare `i` (the default index variable) inside the loop body must not flag.
    let c = codes(
        LOOP_RULES,
        "foo = { for_each_loop = { array = my_arr use_var = i } }",
        &with_index(&vars(&["my_existing_var"])),
    );
    assert!(!c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn loop_explicit_value_name_is_clean() {
    // `value = my_elem` rebinds the element var; reading `my_elem` must be clean.
    let c = codes(
        LOOP_RULES,
        "foo = { for_each_loop = { array = my_arr value = my_elem use_var = my_elem } }",
        &with_index(&vars(&["my_existing_var"])),
    );
    assert!(!c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn loop_undefined_var_still_flags_inside_body() {
    // A genuinely undefined variable inside the loop still flags — seeding the
    // loop locals must not blanket-accept everything in the body.
    let c = codes(
        LOOP_RULES,
        "foo = { for_each_loop = { array = my_arr use_var = nope_xyz } }",
        &with_index(&vars(&["my_existing_var"])),
    );
    assert!(c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn loop_var_outside_loop_still_flags() {
    // The implicit `v` must NOT be globally whitelisted: used outside any loop it
    // is an undefined variable and must still flag CW246.
    let c = codes(
        LOOP_RULES,
        "foo = { use_var = v }",
        &with_index(&vars(&["my_existing_var"])),
    );
    assert!(c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn loop_explicit_name_does_not_leak_to_sibling() {
    // `value = my_elem` is scoped to the loop body; a sibling `use_var = my_elem`
    // outside the loop must still flag.
    let c = codes(
        LOOP_RULES,
        "foo = { for_each_loop = { array = my_arr value = my_elem } use_var = my_elem }",
        &with_index(&vars(&["my_existing_var"])),
    );
    assert!(c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn other_loop_variant_seeds_implicit_vars() {
    // A different loop effect with the same `value_set[variable]` shape (here
    // `while_loop`) must seed its implicit `v`/`i` the same way.
    let c = codes(
        LOOP_RULES,
        "foo = { while_loop = { use_var = v } }",
        &with_index(&vars(&["my_existing_var"])),
    );
    assert!(!c.contains(&"CW246".to_string()), "got: {:?}", c);
}

// ── Issue #306: base-game variables must resolve in the LSP (#306) ───────────
// The vanilla cache carries `var_names` and the LSP stages them into
// `var_index.vanilla_names`. A read of a vanilla-only variable must not flag
// CW246, even though no mod file defines it.

#[test]
fn vanilla_only_variable_is_clean_for_cw246() {
    let c = codes(
        RULES,
        "foo = { ref = vanilla_var }",
        &with_index(&vanilla_vars(&["vanilla_var"], &[])),
    );
    assert!(
        !c.contains(&"CW246".to_string()),
        "vanilla var must resolve, got: {:?}",
        c
    );
}

#[test]
fn vanilla_var_case_insensitive_is_clean() {
    let c = codes(
        RULES,
        "foo = { ref = VANILLA_VAR }",
        &with_index(&vanilla_vars(&["vanilla_var"], &[])),
    );
    assert!(
        !c.contains(&"CW246".to_string()),
        "vanilla lookup is case-insensitive, got: {:?}",
        c
    );
}

#[test]
fn undefined_still_flags_when_only_other_vanilla_var_exists() {
    let c = codes(
        RULES,
        "foo = { ref = mystery }",
        &with_index(&vanilla_vars(&["other_vanilla"], &[])),
    );
    assert!(c.contains(&"CW246".to_string()), "got: {:?}", c);
}

#[test]
fn mod_and_vanilla_both_resolve_and_dedup() {
    let c = codes(
        RULES,
        "foo = { ref = shared_var }",
        &with_index(&vanilla_vars(&["shared_var"], &["shared_var"])),
    );
    assert!(!c.contains(&"CW246".to_string()), "got: {:?}", c);
}
