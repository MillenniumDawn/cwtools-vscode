use std::collections::HashMap;

use cwtools_index::{SourceLocation, TypeIndex, TypeInstance};
use cwtools_validation::{ErrorSeverity, Game, error_hash};

use super::common::{Opts, validate};

#[test]
fn test_validate_simple_type() {
    let cwt = r#"
ethos = {
    cost = int
    category = scalar
}

types = {
    type[ethos] = {
        path = "game/common/ethics"
        subtype[actual_ethics] = {
            cost = int
            category = scalar
        }
    }
}
"#;

    // Valid file matching the ethos type
    let script = r#"
ethos = {
    cost = 2
    category = "materialist"
}
"#;
    let errors = validate(cwt, script, &Opts::DEFAULT);
    assert!(
        errors.is_empty(),
        "Expected no errors but got: {:?}",
        errors
    );

    // Invalid file: wrong value type for int field
    let bad_script = r#"
ethos = {
    cost = "not_a_number"
    category = "materialist"
}
"#;
    let errors = validate(cwt, bad_script, &Opts::DEFAULT);
    assert!(
        !errors.is_empty(),
        "Expected validation error for wrong type"
    );
    assert_eq!(errors[0].severity, ErrorSeverity::Error);
}

#[test]
fn test_validate_cardinality() {
    let cwt = r#"
types = {
    type[ship_size] = {
        path = "game/common/ship_sizes"
        subtype[ship] = {
            ## cardinality = 0..1
            max_speed = float
            ## cardinality = 0..1
            is_civilian = bool
        }
    }
}
"#;

    // Valid: max_speed appears once
    let script = r#"
ship_size = {
    max_speed = 5.5
    is_civilian = no
}
"#;
    let errors = validate(cwt, script, &Opts::DEFAULT);
    assert!(
        errors.is_empty(),
        "Expected no errors but got: {:?}",
        errors
    );
}

#[test]
fn test_error_hash() {
    let error = cwtools_validation::ValidationError {
        message: "Field 'cost' has value 'not_a_number', expected Int".to_string(),
        severity: ErrorSeverity::Error,
        line: 3,
        col: 10,
        file: "test.txt".into(),
        code: None,
        fix: None,
        end: None,
        related: Vec::new(),
    };
    let hash = error_hash(&error);
    assert_eq!(
        hash,
        "error|test.txt|3|Field 'cost' has value 'not_a_number', expected Int"
    );
}

// Inertness guard (Task 8/18, step 2): a `SuggestedFix` payload AND an `end`
// position are pure metadata. `error_hash` must read only severity/file/line/
// message, so a diagnostic with and without them hashes identically. This is the
// corpus-safety net — it must pass both before the emit-site changes and after
// they populate real fixes and end positions.
#[test]
fn fix_payload_does_not_change_error_hash() {
    use cwtools_parser::ast::{SourcePos, SourceRange};
    use cwtools_parser::fix::SuggestedFix;

    let base = cwtools_validation::ValidationError {
        message: "redundant default".to_string(),
        severity: ErrorSeverity::Warning,
        line: 7,
        col: 4,
        file: "common/ideas/x.txt".into(),
        code: Some("CW282"),
        fix: None,
        end: None,
        related: Vec::new(),
    };
    let mut with_fix = base.clone();
    with_fix.fix = Some(SuggestedFix::delete(
        "Remove redundant default",
        SourceRange {
            start: SourcePos { line: 7, col: 4 },
            end: SourcePos { line: 8, col: 0 },
        },
    ));
    // Task 18: the end position is inert too.
    with_fix.end = Some((7, 24));

    assert_eq!(
        error_hash(&base),
        error_hash(&with_fix),
        "fix payload and end position must not affect the diagnostic hash"
    );
}

#[test]
fn test_type_key_filter_matching() {
    let cwt = r#"
types = {
    type[event] = {
        path = "game/events"
        subtype[country_event] = {
            type_key_field = is_triggered_only
            is_triggered_only = bool
            id = scalar
        }
        subtype[news_event] = {
            type_key_field = major
            major = bool
            id = scalar
        }
    }
}
"#;

    // country_event has is_triggered_only - should match country_event subtype
    let script = r#"
event = {
    is_triggered_only = yes
    id = my_event
}
"#;
    let errors = validate(cwt, script, &Opts::DEFAULT);
    assert!(
        errors.is_empty(),
        "Expected no errors but got: {:?}",
        errors
    );

    // news_event has major - should match news_event subtype
    let script2 = r#"
event = {
    major = yes
    id = news_event
}
"#;
    let errors2 = validate(cwt, script2, &Opts::DEFAULT);
    assert!(
        errors2.is_empty(),
        "Expected no errors but got: {:?}",
        errors2
    );

    // Generic event without subtype key - should not get subtype-specific errors
    let script3 = r#"
event = {
    id = generic_event
}
"#;
    let errors3 = validate(cwt, script3, &Opts::DEFAULT);
    // No subtype matches, so no subtype rules apply, no errors expected
    assert!(
        errors3.is_empty(),
        "Expected no errors for generic event: {:?}",
        errors3
    );
}

#[test]
fn empty_string_typefield_value_is_not_flagged() {
    // `soundeffect = ""` / `textureFile = ""` in vanilla + mods — the game treats
    // an empty value as "none", so an empty TypeField value must not be checked
    // against the instance set (CW500 false positive).
    let cwt = r#"
event = {
    ## cardinality = 0..inf
    requires_technology = <technology>
}
types = {
    type[event] = {
        path = "game/events"
    }
    type[technology] = {
        path = "game/common/technology"
    }
}
"#;

    let mut idx = TypeIndex::new();
    let mut map = HashMap::new();
    map.insert(
        "technology".to_string(),
        vec![TypeInstance {
            name: "my_tech_alpha".to_string(),
            location: SourceLocation {
                line: 1,
                col: 0,
                end: (1, 0),
            },
            primary_loc_key: None,
            required_loc_keys: Vec::new(),
        }],
    );
    idx.merge("file://tech.txt", map);
    idx.complete = true;

    let script_empty = r#"
event = {
    requires_technology = ""
}
"#;
    let errs = validate(
        cwt,
        script_empty,
        &Opts {
            path: "game/events/test.txt",
            type_index: Some(&idx),
            ..Opts::DEFAULT
        },
    );
    let type_errs: Vec<_> = errs.iter().filter(|e| e.code == Some("CW500")).collect();
    assert!(
        type_errs.is_empty(),
        "empty-string value must not be flagged as a missing type instance, got: {:?}",
        errs
    );
}

#[test]
fn test_type_index_checking() {
    // Rules: simple type reference field
    let cwt = r#"
event = {
    ## cardinality = 0..inf
    requires_technology = <technology>
}
types = {
    type[event] = {
        path = "game/events"
    }
    type[technology] = {
        path = "game/common/technology"
    }
}
"#;

    let script_valid = r#"
event = {
    requires_technology = my_tech_alpha
}
"#;
    let script_bogus = r#"
event = {
    requires_technology = bogus_tech_xyz
}
"#;

    // Build a TypeIndex with one known technology instance
    let mut idx = TypeIndex::new();
    let mut map = HashMap::new();
    map.insert(
        "technology".to_string(),
        vec![TypeInstance {
            name: "my_tech_alpha".to_string(),
            location: SourceLocation {
                line: 1,
                col: 0,
                end: (1, 0),
            },
            primary_loc_key: None,
            required_loc_keys: Vec::new(),
        }],
    );
    idx.merge("file://tech.txt", map);
    idx.complete = true;

    let opts = Opts {
        path: "game/events/test.txt",
        type_index: Some(&idx),
        ..Opts::DEFAULT
    };

    // Valid reference: no error
    let errs_v = validate(cwt, script_valid, &opts);
    assert!(
        errs_v.is_empty(),
        "Expected no errors for valid ref, got: {:?}",
        errs_v
    );

    // Bogus reference: should produce CW500
    let errs_b = validate(cwt, script_bogus, &opts);
    let type_errs: Vec<_> = errs_b.iter().filter(|e| e.code == Some("CW500")).collect();
    assert!(
        !type_errs.is_empty(),
        "Expected CW500 for bogus type ref, got: {:?}",
        errs_b
    );

    // Without type_index: no type-ref errors even for bogus reference
    let errs_no_idx = validate(
        cwt,
        script_bogus,
        &Opts {
            type_index: None,
            ..opts
        },
    );
    assert!(
        errs_no_idx.iter().all(|e| e.code != Some("CW500")),
        "Expected no CW500 without index, got: {:?}",
        errs_no_idx
    );
}

#[test]
fn test_alias_overload_disjunction() {
    // Regression: an aliased usage must be accepted if it matches ANY
    // `alias[cat:key]` overload, not just the first one declared.
    // Mirrors HOI4 `alias[trigger:original_tag]` (scope OR tag) and the ~40
    // `alias[ai_strategy_rule:ai_strategy]` blocks keyed by `type`.
    let cwt = r#"
types = {
    type[ai_strategy] = {
        path = "game/common/ai_strategy"
    }
}

ai_strategy = {
    ## cardinality = 0..1
    enable = {
        alias_name[trigger] = alias_match_left[trigger]
    }
    alias_name[ai_strategy_rule] = alias_match_left[ai_strategy_rule]
}

alias[trigger:original_tag] = scope[country]
alias[trigger:original_tag] = enum[country_tags]

alias[ai_strategy_rule:ai_strategy] = {
    type = enum[ai_role_strats]
    id = scalar
    value = int
}
alias[ai_strategy_rule:ai_strategy] = {
    type = enum[building_strats]
    id = scalar
    target = int
    value = int
}

enums = {
    enum[country_tags] = { AFG TAL }
    enum[ai_role_strats] = { role_ratio }
    enum[building_strats] = { building_target }
}
"#;

    // original_tag = AFG matches the 2nd trigger overload (enum[country_tags]).
    // The ai_strategy block matches the 2nd rule overload (building_strats, which
    // is the only one that allows `target`). Both fail against the FIRST overload.
    let script = r#"
my_strat = {
    enable = {
        original_tag = AFG
    }
    ai_strategy = {
        type = building_target
        id = foo
        target = 5
        value = 10
    }
}
"#;
    let errors = validate(
        cwt,
        script,
        &Opts {
            path: "game/common/ai_strategy/test.txt",
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
fn test_alias_value_mismatch_is_cw267() {
    // An alias with only a block overload, used as a scalar value, doesn't match
    // any overload. The surfaced diagnostic must be CW267 (not CW240) and carry
    // the real source position of the offending leaf, not 0,0.
    let cwt = r#"
types = {
    type[ai_strategy] = {
        path = "game/common/ai_strategy"
    }
}

ai_strategy = {
    alias_name[ai_strategy_rule] = alias_match_left[ai_strategy_rule]
}

alias[ai_strategy_rule:ai_strategy] = {
    id = scalar
    value = int
}
"#;

    // `ai_strategy` is a block-only overload, but it's used as a bare scalar.
    let script = r#"my_strat = {
    ai_strategy = oops
}
"#;
    let errors = validate(
        cwt,
        script,
        &Opts {
            path: "game/common/ai_strategy/test.txt",
            game: Some(Game::Hoi4),
            ..Opts::DEFAULT
        },
    );
    let cw267: Vec<_> = errors.iter().filter(|e| e.code == Some("CW267")).collect();
    assert!(
        !cw267.is_empty(),
        "Expected a CW267 alias mismatch, got: {:?}",
        errors
    );
    assert!(
        errors.iter().all(|e| e.code != Some("CW240")),
        "CW240 must not be emitted for an alias shape mismatch, got: {:?}",
        errors
    );
    let err = cw267[0];
    assert!(
        err.line > 0,
        "CW267 must carry a real source line, got line {} col {}",
        err.line,
        err.col
    );
    assert!(
        err.message.contains("ai_strategy_rule") && err.message.contains("oops"),
        "CW267 message should name the alias category and offending value, got: {:?}",
        err.message
    );
}

#[test]
fn scripted_loc_value_is_not_cw500() {
    // A `[...]` value is inline scripted localisation / a defined_text reference
    // resolved at runtime (e.g. `picture = "[GetCivilWarVictorPicture]"`), not a
    // literal type instance, so it must not flag CW500.
    let cwt = r#"
event = {
    requires_technology = <technology>
}
types = {
    type[event] = { path = "game/events" }
    type[technology] = { path = "game/common/technology" }
}
"#;

    let mut idx = TypeIndex::new();
    let mut map = HashMap::new();
    map.insert(
        "technology".to_string(),
        vec![TypeInstance {
            name: "my_tech_alpha".to_string(),
            location: SourceLocation {
                line: 1,
                col: 0,
                end: (1, 0),
            },
            primary_loc_key: None,
            required_loc_keys: Vec::new(),
        }],
    );
    idx.merge("file://tech.txt", map);
    idx.complete = true;

    let script = "event = { requires_technology = \"[GetSomeTech]\" }\n";
    let errs = validate(
        cwt,
        script,
        &Opts {
            path: "game/events/test.txt",
            type_index: Some(&idx),
            ..Opts::DEFAULT
        },
    );
    assert!(
        errs.iter().all(|e| e.code != Some("CW500")),
        "scripted-loc [..] value must not flag CW500, got: {:?}",
        errs
    );
}

#[test]
fn unknown_type_key_is_unexpected_when_index_is_complete() {
    let cwt = r#"
types = {
    type[state] = {
        path = "game/history/states"
    }
    type[resource] = {
        path = "game/common/resources"
    }
}
state = {
    resources = {
        ## cardinality = 0..inf
        <resource> = float
    }
}
"#;

    let mut idx = TypeIndex::new();
    let mut map = HashMap::new();
    map.insert(
        "resource".to_string(),
        vec![TypeInstance {
            name: "steel".to_string(),
            location: SourceLocation {
                line: 1,
                col: 0,
                end: (1, 0),
            },
            primary_loc_key: None,
            required_loc_keys: Vec::new(),
        }],
    );
    idx.merge("file://resources.txt", map);
    idx.complete = true;

    fn state_file(idx: &TypeIndex) -> Opts<'_> {
        Opts {
            path: "game/history/states/1-test.txt",
            type_index: Some(idx),
            ..Opts::DEFAULT
        }
    }
    let valid = "state = { resources = { steel = 5 } }\n";
    let errs_valid = validate(cwt, valid, &state_file(&idx));
    assert!(
        errs_valid.is_empty(),
        "known resource key must validate, got: {errs_valid:?}"
    );

    let bogus = "state = { resources = { unobtainium = 3 } }\n";
    let errs_bogus = validate(cwt, bogus, &state_file(&idx));
    assert!(
        errs_bogus.iter().any(|e| e.code == Some("CW263")),
        "unknown resource key must be CW263, got: {errs_bogus:?}"
    );

    let bad_rhs = "state = { resources = { steel = not_a_number } }\n";
    let errs_rhs = validate(cwt, bad_rhs, &state_file(&idx));
    assert!(
        errs_rhs.iter().any(|e| e.code == Some("CW240")),
        "invalid resource amount must still be CW240, got: {errs_rhs:?}"
    );

    idx.complete = false;
    let errs_incomplete = validate(cwt, bogus, &state_file(&idx));
    assert!(
        errs_incomplete.iter().all(|e| e.code != Some("CW263")),
        "unknown resource key must stay quiet without a complete index, got: {errs_incomplete:?}"
    );
}
