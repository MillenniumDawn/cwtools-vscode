//! Guards for the shared root-type dispatch (`resolve::resolve_root_child`),
//! which drives BOTH the validator (`validate_prepared`) and the navigator
//! (`rules_at_pos`). The two callers must agree on which `TypeDefinition` owns a
//! root node; the ONE intended divergence is `allow_content_fallback` (validator
//! = false, navigator = true).
//!
//! Before these tests, that divergence was unguarded: flipping the validator's
//! `allow_content_fallback` to `true` (so it content-validates an index-only
//! skip-wrapper against a sibling base type) broke no test, and the navigator's
//! content-fallback descent was likewise unexercised.

use cwtools_index::TypeIndex;
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_rules::rules_types::*;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::position::{RuleContext, rules_at_pos};
use cwtools_validation::{Prepared, build_scope_registry_arc, validate_ast};

const FILE_PATH: &str = "game/common/on_actions/test.txt";

fn pos_of(script: &str, marker: &str) -> (u32, u16) {
    let off = script
        .find(marker)
        .unwrap_or_else(|| panic!("marker {:?} not in script", marker));
    let before = &script[..off];
    let line = before.matches('\n').count() as u32 + 1;
    let col = before.rsplit('\n').next().unwrap().len() as u16;
    (line, col)
}

fn validate(
    rules: &str,
    script: &str,
    file_path: &str,
) -> Vec<cwtools_validation::ValidationError> {
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(rules, &table), &table);
    let parsed = parse_string(script, &table);
    let idx = TypeIndex::new();
    validate_ast(&parsed, &ruleset, &table, file_path, None, Some(&idx), None)
}

fn navigate(rules: &str, script: &str, file_path: &str, marker: &str) -> Option<RuleContext> {
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(rules, &table), &table);
    let parsed = parse_string(script, &table);
    let registry = build_scope_registry_arc(&ruleset, None);
    let prepared = Prepared {
        ruleset: &ruleset,
        table: &table,
        game: None,
        type_index: None,
        modifier_keys: None,
        loc_index: None,
        extra_loc_keys: None,
        inline_scripts: None,
        registry: registry.as_ref(),
        scope_checks: true,
        var_checks: false,
    };
    let (line, col) = pos_of(script, marker);
    rules_at_pos(&parsed, file_path, &prepared, line, col, false)
}

fn specific_keys(rules: &[(RuleType, Options)]) -> Vec<String> {
    rules
        .iter()
        .filter_map(|(rt, _)| match rt {
            RuleType::LeafRule {
                left: NewField::SpecificField(k),
                ..
            }
            | RuleType::NodeRule {
                left: NewField::SpecificField(k),
                ..
            } => Some(k.clone()),
            _ => None,
        })
        .collect()
}

// ── The `allow_content_fallback` divergence (the single most important property)
//
// The `on_actions = { ... }` shape: the root key `on_actions` path-matches the
// `on_weekly` skip-wrapper (skip_root_key = on_actions) which carries NO rule
// body. The actual rules live in the sibling `on_action` base type (no
// skip_root_key). The validator must SKIP such a root (it never content-validates
// an index-only match); the navigator must FALL BACK to the content-bearing
// sibling so the cursor can still descend.

// `on_action` defines on_daily but NOT on_weekly: if the validator wrongly fell
// back to `on_action` and entered it as an Entity, the script's `on_weekly` block
// would flag CW262 (unexpected block) and CW242 (missing on_daily).
const RULES_NO_WEEKLY: &str = r#"
types = {
    type[on_action] = {
        path = "game/common/on_actions"
    }
    ## type_key_filter = on_weekly
    type[on_weekly] = {
        path = "game/common/on_actions"
        skip_root_key = on_actions
    }
}
on_action = {
    on_daily = {
        effect = scalar
    }
}
"#;

// `on_action` defines on_weekly, so the navigator can resolve its rule body after
// the content fallback descends.
const RULES_WITH_WEEKLY: &str = r#"
types = {
    type[on_action] = {
        path = "game/common/on_actions"
    }
    ## type_key_filter = on_weekly
    type[on_weekly] = {
        path = "game/common/on_actions"
        skip_root_key = on_actions
    }
}
on_action = {
    on_weekly = {
        effect = scalar
    }
}
"#;

#[test]
fn validator_does_not_content_fallback_into_sibling_base_type() {
    // The validator must NOT validate the index-only skip-wrapper's grandchildren
    // against the sibling `on_action` base type. With allow_content_fallback wrongly
    // flipped to true, this emits CW262 "Unexpected block 'on_weekly'" + CW242.
    let script = "on_actions = {\n\ton_weekly = {\n\t\teffect = boom\n\t}\n}\n";
    let errors = validate(RULES_NO_WEEKLY, script, FILE_PATH);
    assert!(
        errors.is_empty(),
        "validator must skip the index-only skip-wrapper root, not content-validate \
         it against the `on_action` sibling; got: {:?}",
        errors
            .iter()
            .map(|e| (e.code, e.message.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn navigator_content_falls_back_into_sibling_base_type() {
    // The cursor inside `on_weekly = { effect = | }` must resolve the `on_action`
    // sibling's rules (it defines `on_weekly = { effect = scalar }`). Without the
    // navigator's content fallback, rules_at_pos returns None here.
    let script = "on_actions = {\n\ton_weekly = {\n\t\teffect = boom\n\t}\n}\n";
    let ctx = navigate(RULES_WITH_WEEKLY, script, FILE_PATH, "effect = boom")
        .expect("navigator must content-fall-back so the cursor can descend");
    assert!(
        specific_keys(&ctx.child_rules).contains(&"effect".to_string()),
        "the on_action sibling's `effect` rule must be in scope, got: {:?}",
        specific_keys(&ctx.child_rules)
    );
}

// ── The validator skips an index-only path type (guards allow_content_fallback=false)
//
// A `type[x] = { path = ... }` with no rule body exists only to index instances.
// Its root nodes are not content-validated: every field would otherwise read as
// "unexpected".

const INDEX_ONLY_RULES: &str = r#"
types = {
    type[sprite] = {
        path = "game/gfx"
    }
}
"#;

#[test]
fn validator_skips_index_only_path_type() {
    // `sprite` has a path but NO rule body. The root `my_sprite = { ... }` must
    // validate clean — the validator skips it rather than flagging its fields.
    let script = "my_sprite = {\n\ttexturefile = x.dds\n\tanything = 5\n}\n";
    let errors = validate(INDEX_ONLY_RULES, script, "game/gfx/test.gfx");
    assert!(
        errors.is_empty(),
        "an index-only type[x] must not content-validate its instances; got: {:?}",
        errors
            .iter()
            .map(|e| (e.code, e.message.as_str()))
            .collect::<Vec<_>>()
    );
}

// ── Entity vs Wrapper resolution
//
// A plain typed root (no skip_root_key) is an Entity: validate the node itself.
// A skip_root_key wrapper is a Wrapper: validate its GRANDCHILDREN as instances.

const ENTITY_VS_WRAPPER_RULES: &str = r#"
types = {
    type[plain] = {
        path = "game/common/things"
    }
    type[wrapped] = {
        path = "game/common/things"
        skip_root_key = wrapper
    }
}
plain = {
    foo = int
}
wrapped = {
    bar = int
}
"#;

#[test]
fn entity_root_validates_the_node_itself() {
    // `plain` has no skip_root_key: the root node IS the instance, so its own
    // fields are validated. A bad field flags; a good one does not.
    let good = validate(
        ENTITY_VS_WRAPPER_RULES,
        "my_plain = {\n\tfoo = 5\n}\n",
        "game/common/things/test.txt",
    );
    assert!(
        good.is_empty(),
        "a valid Entity field must not flag, got: {:?}",
        good.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    let bad = validate(
        ENTITY_VS_WRAPPER_RULES,
        "my_plain = {\n\tnope = 5\n}\n",
        "game/common/things/test.txt",
    );
    assert!(
        bad.iter().any(|e| e.message.contains("nope")),
        "the Entity node's own fields must be validated (unknown `nope` flags), got: {:?}",
        bad.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn wrapper_root_validates_grandchildren_not_the_node() {
    // `wrapper = { ... }` is a skip_root_key wrapper for `wrapped`: the wrapper key
    // itself is not flagged, and the GRANDCHILDREN (instances) are validated
    // against `wrapped`'s rules. A bad grandchild field flags; a good one does not.
    let good = validate(
        ENTITY_VS_WRAPPER_RULES,
        "wrapper = {\n\tinst = {\n\t\tbar = 5\n\t}\n}\n",
        "game/common/things/test.txt",
    );
    assert!(
        good.is_empty(),
        "a valid Wrapper grandchild must not flag, got: {:?}",
        good.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    let bad = validate(
        ENTITY_VS_WRAPPER_RULES,
        "wrapper = {\n\tinst = {\n\t\tnope = 5\n\t}\n}\n",
        "game/common/things/test.txt",
    );
    assert!(
        bad.iter().any(|e| e.message.contains("nope")),
        "the Wrapper's grandchildren are validated against `wrapped` (unknown `nope` \
         flags), got: {:?}",
        bad.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}
