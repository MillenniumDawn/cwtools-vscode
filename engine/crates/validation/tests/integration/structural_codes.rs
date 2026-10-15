//! Tier B structural hints (F# CommonValidation.fs + STLValidation.fs):
//! - CW121 empty if/else_if
//! - CW223 NOT with multiple children
//! - CW251 redundant AND-in-AND / OR-in-OR
//! - CW236/237/238 Stellaris if/else (2.1)
//! - CW253 deprecated set_empire_name/set_planet_name
//!
//! These run inside `per_game::run_game_validators`, so a `game` must be set.

use cwtools_index::{TypeIndex, collect_type_instances};
use cwtools_parser::parser::parse_string;
use cwtools_validation::Game;

use super::common::{Opts, codes, ruleset, validate, validate_with};

const HOI4: Opts<'static> = Opts {
    game: Some(Game::Hoi4),
    ..Opts::DEFAULT
};

const STELLARIS: Opts<'static> = Opts {
    game: Some(Game::Stellaris),
    ..Opts::DEFAULT
};

// The structural pass is rules-independent.
const NO_RULES: &str = "";

#[test]
fn not_with_multiple_children_is_cw223() {
    let c = codes(
        NO_RULES,
        r#"
foo = {
    NOT = {
        a = 1
        b = 2
    }
}
"#,
        &HOI4,
    );
    assert!(c.contains(&"CW223".to_string()), "got: {:?}", c);
}

#[test]
fn cw223_message_is_game_specific() {
    // HOI4 has no NOR/NAND triggers, so its message must not advise them.
    let script = "foo = { NOT = { a = 1\n b = 2 } }";

    let msg = |opts| {
        validate(NO_RULES, script, opts)
            .into_iter()
            .find(|e| e.code == Some("CW223"))
            .map(|e| e.message)
            .unwrap_or_default()
    };

    let hoi4 = msg(&HOI4);
    assert!(hoi4.contains("acts as NOR"), "hoi4 msg: {hoi4}");
    assert!(
        !hoi4.contains("NAND to avoid"),
        "hoi4 must not keep old text: {hoi4}"
    );

    // Stellaris keeps the NOR/NAND wording (those are valid triggers there).
    let stel = msg(&STELLARIS);
    assert!(stel.contains("NOR or NAND"), "stellaris msg: {stel}");
}

#[test]
fn not_with_single_child_is_clean() {
    let c = codes(
        NO_RULES,
        r#"
foo = {
    NOT = { a = 1 }
}
"#,
        &HOI4,
    );
    assert!(!c.contains(&"CW223".to_string()), "got: {:?}", c);
}

#[test]
fn empty_if_is_cw121() {
    let c = codes(
        NO_RULES,
        r#"
foo = {
    if = {
        limit = { a = 1 }
    }
}
"#,
        &HOI4,
    );
    assert!(c.contains(&"CW121".to_string()), "got: {:?}", c);
}

#[test]
fn if_with_effect_is_clean() {
    let c = codes(
        NO_RULES,
        r#"
foo = {
    if = {
        limit = { a = 1 }
        b = 2
    }
}
"#,
        &HOI4,
    );
    assert!(!c.contains(&"CW121".to_string()), "got: {:?}", c);
}

#[test]
fn empty_limit_is_cw281() {
    let c = codes(
        NO_RULES,
        r#"
foo = {
    every_country = {
        limit = { }
        b = 2
    }
}
"#,
        &HOI4,
    );
    assert!(c.contains(&"CW281".to_string()), "got: {:?}", c);
}

#[test]
fn non_empty_limit_is_clean() {
    let c = codes(
        NO_RULES,
        r#"
foo = {
    every_country = {
        limit = { has_war = yes }
        b = 2
    }
}
"#,
        &HOI4,
    );
    assert!(!c.contains(&"CW281".to_string()), "got: {:?}", c);
}

#[test]
fn and_in_and_is_cw251() {
    let c = codes(
        NO_RULES,
        r#"
foo = {
    AND = {
        AND = { a = 1 }
    }
}
"#,
        &HOI4,
    );
    assert!(c.contains(&"CW251".to_string()), "got: {:?}", c);
}

#[test]
fn and_in_or_is_clean() {
    let c = codes(
        NO_RULES,
        r#"
foo = {
    OR = {
        AND = { a = 1 }
    }
}
"#,
        &HOI4,
    );
    assert!(!c.contains(&"CW251".to_string()), "got: {:?}", c);
}

#[test]
fn nor_itself_does_not_fire_cw251() {
    // NOR puts its children in an Or context but is never itself a redundant
    // boolean: `NOR { NOR {...} }` must stay clean (only OR-in-OR / AND-in-AND
    // fire CW251).
    let c = codes(
        NO_RULES,
        r#"
foo = {
    NOR = {
        NOR = { a = 1 }
    }
}
"#,
        &HOI4,
    );
    assert!(!c.contains(&"CW251".to_string()), "got: {:?}", c);
}

#[test]
fn or_inside_nor_is_cw251() {
    // NOR's children sit in an Or context, so an OR directly inside it is
    // redundant OR-in-OR and fires CW251 (matching the pre-refactor behavior).
    let c = codes(
        NO_RULES,
        r#"
foo = {
    NOR = {
        OR = { a = 1 }
    }
}
"#,
        &HOI4,
    );
    assert!(c.contains(&"CW251".to_string()), "got: {:?}", c);
}

#[test]
fn and_inside_not_is_clean() {
    // HOI4 `NOT = { a b }` means "none true" (= NOT(a OR b)), so an explicit AND
    // inside NOT is meaningful — `NOT = { AND = {...} }` (not-all) differs from
    // `NOT = {...}` (none). It must NOT fire CW251.
    let c = codes(
        NO_RULES,
        r#"
foo = {
    NOT = {
        AND = { a = 1 b = 2 }
    }
}
"#,
        &HOI4,
    );
    assert!(!c.contains(&"CW251".to_string()), "got: {:?}", c);
}

#[test]
fn or_inside_not_is_clean() {
    // `NOT = { OR = {…} }` is the standard HOI4 "none of these" idiom (HOI4 has
    // no NOR trigger). It's intentional, not redundant, so it must not flag
    // CW251.
    let c = codes(
        NO_RULES,
        r#"
foo = {
    NOT = {
        OR = { a = 1 b = 2 }
    }
}
"#,
        &HOI4,
    );
    assert!(!c.contains(&"CW251".to_string()), "got: {:?}", c);
}

#[test]
fn and_inside_count_triggers_is_clean() {
    // count_triggers counts how many of its direct children are true, so an AND
    // that groups several conditions into one counted unit is meaningful, not a
    // redundant AND-in-AND. It must not fire CW251 (cwtools-vscode#27).
    let c = codes(
        NO_RULES,
        r#"
foo = {
    count_triggers = {
        amount = 2
        has_war_with = INT
        AND = { has_global_flag = x has_war_with = RUS }
    }
}
"#,
        &HOI4,
    );
    assert!(!c.contains(&"CW251".to_string()), "got: {:?}", c);
}

#[test]
fn else_without_if_is_cw238() {
    let c = codes(
        NO_RULES,
        r#"
foo = {
    else = { a = 1 }
}
"#,
        &STELLARIS,
    );
    assert!(c.contains(&"CW238".to_string()), "got: {:?}", c);
}

#[test]
fn if_then_else_is_clean_order() {
    let c = codes(
        NO_RULES,
        r#"
foo = {
    if = { limit = { x = 1 } a = 1 }
    else = { b = 2 }
}
"#,
        &STELLARIS,
    );
    assert!(!c.contains(&"CW238".to_string()), "got: {:?}", c);
}

#[test]
fn deprecated_set_name_is_cw253() {
    let c = codes(
        NO_RULES,
        r#"
foo = {
    set_empire_name = { key = "X" }
}
"#,
        &STELLARIS,
    );
    assert!(c.contains(&"CW253".to_string()), "got: {:?}", c);
}

// CW236/238 advise about the if/else keyword, not the block it opens: the
// squiggle must cover only the key token.
#[test]
fn cw236_underlines_only_the_if_key() {
    let script =
        "foo = {\n    if = {\n        limit = { x = 1 }\n        else = { b = 2 }\n    }\n}\n";

    let err = validate(NO_RULES, script, &STELLARIS)
        .into_iter()
        .find(|e| e.code == Some("CW236"))
        .expect("CW236 emitted");

    assert_eq!((err.line, err.col), (2, 4));
    assert_eq!(
        err.end,
        Some((2, 4 + "if".len() as u16)),
        "CW236 must span only the key"
    );
}

#[test]
fn cw238_underlines_only_the_containing_key() {
    let script = "foo = {\n    else = { a = 1 }\n}\n";

    let err = validate(NO_RULES, script, &STELLARIS)
        .into_iter()
        .find(|e| e.code == Some("CW238"))
        .expect("CW238 emitted");

    assert_eq!((err.line, err.col), (1, 0));
    assert_eq!(
        err.end,
        Some((1, "foo".len() as u16)),
        "CW238 must span only the key"
    );
}

// CW261 fires at every definition of a duplicated `unique = yes` instance id.
// One of them has to go, so the squiggle spans the whole definition.
#[test]
fn cw261_spans_each_duplicate_definition() {
    let cwt = "types = {\n    type[thing] = {\n        path = \"game/common/things\"\n        unique = yes\n    }\n}\n";
    let (table, ruleset) = ruleset(cwt);
    let path = "game/common/things/test.txt";
    let script = "dup = {\n    a = 1\n}\ndup = {\n    b = 2\n}\n";
    let parsed = parse_string(script, &table);

    let mut index = TypeIndex::new();
    index.merge(
        path,
        collect_type_instances(&ruleset, &parsed, path, &table),
    );

    let opts = Opts {
        path,
        type_index: Some(&index),
        ..HOI4
    };
    let errs: Vec<_> = validate_with(&table, &ruleset, script, &opts)
        .into_iter()
        .filter(|e| e.code == Some("CW261"))
        .collect();

    assert_eq!(errs.len(), 2, "got: {errs:?}");
    // Each span runs from the definition's key to just past its closing brace.
    let spans: Vec<_> = errs.iter().map(|e| ((e.line, e.col), e.end)).collect();
    assert!(spans.contains(&((1, 0), Some((4, 0)))), "got: {spans:?}");
    assert!(spans.contains(&((4, 0), Some((7, 0)))), "got: {spans:?}");
}

// The complaint is the key name, not the block, and the fix already renames only
// the key token. The squiggle should agree with it.
#[test]
fn cw253_underlines_only_the_deprecated_key() {
    let script = "foo = {\n    set_empire_name = {\n        key = \"X\"\n    }\n}\n";

    let err = validate(NO_RULES, script, &STELLARIS)
        .into_iter()
        .find(|e| e.code == Some("CW253"))
        .expect("CW253 emitted");

    assert_eq!((err.line, err.col), (2, 4));
    assert_eq!(
        err.end,
        Some((2, 4 + "set_empire_name".len() as u16)),
        "CW253 must span only the key"
    );
}
