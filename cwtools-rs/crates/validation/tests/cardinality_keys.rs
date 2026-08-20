//! Cardinality is aggregated per *distinct rule key*, matched case-insensitively.
//! These lock in the four properties that aggregation has to hold, independent of
//! how the counting is implemented: keys fold ASCII case, duplicate keys are one
//! disjunction under the most permissive bounds, a soft (`~`) minimum on any
//! overload softens the key, and each key reports at most once.

use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::{ValidationError, validate_ast};

fn run(cwt: &str, script: &str) -> Vec<ValidationError> {
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(cwt, &table), &table);
    let parsed = parse_string(script, &table);
    validate_ast(&parsed, &ruleset, &table, "test.txt", None, None, None)
}

fn cw242<'a>(errors: &'a [ValidationError], needle: &str) -> Vec<&'a ValidationError> {
    errors
        .iter()
        .filter(|e| e.code == Some("CW242") && e.message.contains(needle))
        .collect()
}

// A field written in a different case than the rule keys it satisfies the rule:
// Paradox keys are case-insensitive. `textureFile` in the config is satisfied by
// `texturefile` in script, so the required field is not reported missing.
#[test]
fn field_case_differing_from_the_rule_key_still_counts() {
    let cwt = r#"
spriteType = {
    textureFile = scalar
}
types = {
    type[spriteType] = {
        path = "game/interface"
    }
}
"#;
    let errors = run(cwt, "spriteType = {\n    texturefile = gfx/x.dds\n}\n");
    assert!(
        cw242(&errors, "textureFile").is_empty(),
        "a lowercased field must satisfy a camelCased rule key, got: {:?}",
        cw242(&errors, "textureFile")
    );
}

// The mirror of the above: an UPPERCASE field satisfies a lowercase rule key,
// and an over-count is still counted across the mixed spellings rather than
// being split into two separate tallies.
#[test]
fn mixed_case_spellings_of_one_key_share_a_single_tally() {
    let cwt = r#"
my_thing = {
    ## cardinality = 0..1
    icon = scalar
}
types = {
    type[my_thing] = {
        path = "game/common/things"
    }
}
"#;
    let errors = run(cwt, "my_thing = {\n    icon = a\n    ICON = b\n}\n");
    let over = cw242(&errors, "at most");
    assert_eq!(
        over.len(),
        1,
        "two spellings of one key are two occurrences of that key, got: {:?}",
        errors
            .iter()
            .map(|e| (e.code, e.message.clone()))
            .collect::<Vec<_>>()
    );
}

// Two rules keyed the same are alternatives, not two independent requirements.
// The key is checked once against the widest bounds either overload allows, so a
// field present twice satisfies the `0..2` overload even though the other says
// `0..1`.
#[test]
fn duplicate_key_rules_aggregate_to_the_widest_max() {
    let cwt = r#"
my_thing = {
    ## cardinality = 0..1
    clicksound = scalar
    ## cardinality = 0..2
    clicksound = scalar
}
types = {
    type[my_thing] = {
        path = "game/common/things"
    }
}
"#;
    let errors = run(
        cwt,
        "my_thing = {\n    clicksound = a\n    clicksound = b\n}\n",
    );
    assert!(
        cw242(&errors, "at most").is_empty(),
        "the widest overload allows two, so neither should report over-count: {:?}",
        cw242(&errors, "at most")
    );
}

// The same aggregation on the minimum: an absent key whose loosest overload
// allows zero is not missing.
#[test]
fn duplicate_key_rules_aggregate_to_the_narrowest_min() {
    let cwt = r#"
my_thing = {
    name = scalar
    ## cardinality = 1..1
    clicksound = scalar
    ## cardinality = 0..1
    clicksound = scalar
}
types = {
    type[my_thing] = {
        path = "game/common/things"
    }
}
"#;
    let errors = run(cwt, "my_thing = {\n    name = a\n}\n");
    assert!(
        cw242(&errors, "clicksound").is_empty(),
        "an overload allowing zero makes the key optional, got: {:?}",
        cw242(&errors, "clicksound")
    );
}

// A required key with several overloads must not report once per overload.
#[test]
fn a_missing_key_with_several_overloads_reports_once() {
    let cwt = r#"
my_thing = {
    name = scalar
    required_field = scalar
    required_field = int
    required_field = bool
}
types = {
    type[my_thing] = {
        path = "game/common/things"
    }
}
"#;
    let errors = run(cwt, "my_thing = {\n    name = a\n}\n");
    assert_eq!(
        cw242(&errors, "required_field").len(),
        1,
        "three overloads of one missing key is still one diagnostic, got: {:?}",
        cw242(&errors, "required_field")
    );
}

// `~` marks a soft minimum. A soft minimum on ANY overload softens the whole
// key, so an absent field is not flagged even though a sibling overload declares
// a hard `1..1`.
#[test]
fn a_soft_minimum_on_one_overload_softens_the_key() {
    let cwt = r#"
my_thing = {
    name = scalar
    ## cardinality = 1..1
    ship_types = scalar
    ## cardinality = ~1..1
    ship_types = int
}
types = {
    type[my_thing] = {
        path = "game/common/things"
    }
}
"#;
    let errors = run(cwt, "my_thing = {\n    name = a\n}\n");
    assert!(
        cw242(&errors, "ship_types").is_empty(),
        "a ~ minimum on any overload suppresses the under-count, got: {:?}",
        cw242(&errors, "ship_types")
    );
}

// Guard the other direction: with every overload hard, the under-count still
// fires. Without this, the soft-minimum test above would pass on a build that
// had stopped reporting under-counts entirely.
#[test]
fn an_all_hard_minimum_key_still_reports_under_count() {
    let cwt = r#"
my_thing = {
    name = scalar
    ## cardinality = 1..1
    ship_types = scalar
    ## cardinality = 1..1
    ship_types = int
}
types = {
    type[my_thing] = {
        path = "game/common/things"
    }
}
"#;
    let errors = run(cwt, "my_thing = {\n    name = a\n}\n");
    assert_eq!(
        cw242(&errors, "ship_types").len(),
        1,
        "an absent hard-minimum key must still be reported, got: {:?}",
        errors
            .iter()
            .map(|e| (e.code, e.message.clone()))
            .collect::<Vec<_>>()
    );
}

// A child key no rule mentions contributes to no key's tally. It is reported as
// an unexpected field (CW263), never as another key's cardinality.
#[test]
fn an_unrelated_child_key_does_not_feed_another_keys_count() {
    let cwt = r#"
my_thing = {
    ## cardinality = 0..1
    icon = scalar
}
types = {
    type[my_thing] = {
        path = "game/common/things"
    }
}
"#;
    let errors = run(
        cwt,
        "my_thing = {\n    icon = a\n    stray = b\n    other = c\n}\n",
    );
    assert!(
        cw242(&errors, "at most").is_empty(),
        "unrelated keys must not inflate icon's count, got: {:?}",
        cw242(&errors, "at most")
    );
    assert_eq!(
        errors.iter().filter(|e| e.code == Some("CW263")).count(),
        2,
        "both stray keys are unexpected fields, got: {:?}",
        errors
            .iter()
            .map(|e| (e.code, e.message.clone()))
            .collect::<Vec<_>>()
    );
}

// A rule list with no keyed, bare-value or value-clause rule has no cardinality
// to enforce. The block still validates its children (the alias body is checked
// through its own rules); it must simply never emit CW242.
#[test]
fn a_block_whose_rules_are_all_aliases_emits_no_cardinality() {
    let cwt = r#"
my_thing = {
    effect = {
        alias_name[effect] = alias_match_left[effect]
    }
}
alias[effect:set_flag] = scalar
types = {
    type[my_thing] = {
        path = "game/common/things"
    }
}
"#;
    let errors = run(
        cwt,
        "my_thing = {\n    effect = {\n        set_flag = my_flag\n    }\n}\n",
    );
    assert!(
        errors.iter().all(|e| e.code != Some("CW242")),
        "an alias-only body has no cardinality to enforce, got: {:?}",
        errors
            .iter()
            .map(|e| (e.code, e.message.clone()))
            .collect::<Vec<_>>()
    );
}
