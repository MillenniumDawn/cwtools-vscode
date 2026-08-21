use cwtools_index::{SourceLocation, TypeIndex, TypeInstance};
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::{ErrorSeverity, error_hash, validate_ast};
use std::collections::HashMap;

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

    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    // Valid file matching the ethos type
    let script = r#"
ethos = {
    cost = 2
    category = "materialist"
}
"#;
    let parsed = parse_string(script, &table);
    let errors = validate_ast(&parsed, &ruleset, &table, "test.txt", None, None, None);
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
    let parsed_bad = parse_string(bad_script, &table);
    let errors = validate_ast(&parsed_bad, &ruleset, &table, "test.txt", None, None, None);
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

    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    // Valid: max_speed appears once
    let script = r#"
ship_size = {
    max_speed = 5.5
    is_civilian = no
}
"#;
    let parsed = parse_string(script, &table);
    let errors = validate_ast(&parsed, &ruleset, &table, "test.txt", None, None, None);
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

    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    // country_event has is_triggered_only - should match country_event subtype
    let script = r#"
event = {
    is_triggered_only = yes
    id = my_event
}
"#;
    let parsed = parse_string(script, &table);
    let errors = validate_ast(&parsed, &ruleset, &table, "test.txt", None, None, None);
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
    let parsed2 = parse_string(script2, &table);
    let errors2 = validate_ast(&parsed2, &ruleset, &table, "test.txt", None, None, None);
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
    let parsed3 = parse_string(script3, &table);
    let errors3 = validate_ast(&parsed3, &ruleset, &table, "test.txt", None, None, None);
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
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

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
    let parsed = parse_string(script_empty, &table);
    let errs = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "game/events/test.txt",
        None,
        Some(&idx),
        None,
    );
    let type_errs: Vec<_> = errs.iter().filter(|e| e.code == Some("CW500")).collect();
    assert!(
        type_errs.is_empty(),
        "empty-string value must not be flagged as a missing type instance, got: {:?}",
        errs
    );
}

const TEXTURE_CWT: &str = r#"
spriteType = {
    texturefile = filepath
    secondfile = filepath
}
types = {
    type[spriteType] = {
        path = "game/interface"
    }
}
"#;

#[test]
fn texture_reference_resolves_via_sibling_extension() {
    // The engine resolves textures by stem: a `.tga` reference is satisfied by a
    // shipped `.dds` and vice versa (vanilla `core.gfx` points at `.tga` files
    // while only the `.dds` ships). CW113 must not fire when the sibling exists.
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(TEXTURE_CWT, &table), &table);

    let mut idx = TypeIndex::new();
    idx.file_index.add_paths([
        "gfx/test/button.dds".to_string(),
        "gfx/test/icon.tga".to_string(),
    ]);

    let script = r#"
spriteType = {
    texturefile = "gfx/test/button.tga"
    secondfile = "gfx/test/icon.dds"
}
"#;
    let parsed = parse_string(script, &table);
    let errs = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "game/interface/test.gfx",
        Some(cwtools_validation::Game::Hoi4),
        Some(&idx),
        None,
    );
    let cw113: Vec<_> = errs.iter().filter(|e| e.code == Some("CW113")).collect();
    assert!(
        cw113.is_empty(),
        "texture references should resolve via their sibling extension, got: {:?}",
        cw113
    );
}

#[test]
fn case_mismatched_file_reference_relates_the_on_disk_spelling() {
    // On a case-sensitive run the file is there, just under another spelling.
    // CW113 says which one, so the reader doesn't go hunting for the file.
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(TEXTURE_CWT, &table), &table);

    let mut idx = TypeIndex::new();
    idx.file_index.set_case_sensitive(true);
    idx.file_index
        .add_paths(["gfx/test/button.dds".to_string()]);

    let script = "spriteType = {\n    texturefile = \"gfx/test/Button.dds\"\n}\n";
    let parsed = parse_string(script, &table);
    let errs = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "game/interface/test.gfx",
        Some(cwtools_validation::Game::Hoi4),
        Some(&idx),
        None,
    );
    let cw113 = errs
        .iter()
        .find(|e| e.code == Some("CW113"))
        .expect("case mismatch is flagged in case-sensitive mode");
    assert_eq!(cw113.related.len(), 1, "got: {:?}", cw113.related);
    assert!(
        cw113.related[0].message.contains("gfx/test/button.dds"),
        "got: {}",
        cw113.related[0].message
    );
}

#[test]
fn missing_texture_with_no_sibling_still_flagged() {
    // Regression guard against blanket-suppressing texture CW113: a reference with
    // neither extension present on disk (genuinely missing asset) must still flag.
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(TEXTURE_CWT, &table), &table);

    let mut idx = TypeIndex::new();
    idx.file_index
        .add_paths(["gfx/test/button.dds".to_string()]);

    let script = r#"
spriteType = {
    texturefile = "gfx/test/ghost.tga"
}
"#;
    let parsed = parse_string(script, &table);
    let errs = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "game/interface/test.gfx",
        Some(cwtools_validation::Game::Hoi4),
        Some(&idx),
        None,
    );
    let cw113: Vec<_> = errs.iter().filter(|e| e.code == Some("CW113")).collect();
    assert_eq!(
        cw113.len(),
        1,
        "a texture missing in both extensions must still be flagged, got: {:?}",
        errs
    );
}

#[test]
fn sound_asset_file_resolves_beside_the_asset() {
    // A sound `.asset` `file =` resolves relative to the .asset's own directory,
    // not the field's `sound/` root prefix (CW113 false positive).
    let cwt = r#"
sound = {
    name = scalar
    file = filepath[sound/]
}
types = {
    type[sound] = {
        path = "game/sound"
    }
}
"#;
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(cwt, &table), &table);

    let mut idx = TypeIndex::new();
    // The .asset itself is indexed (so its root-relative dir can be recovered)
    // and the referenced .wav lives beside it — but NOT under bare `sound/`.
    idx.file_index.add_paths([
        "sound/zom/zom_vo.asset".to_string(),
        "sound/zom/zom_idle_001.wav".to_string(),
    ]);

    let script = r#"
sound = {
    name = "zom_idle_001"
    file = "zom_idle_001.wav"
}
"#;
    let parsed = parse_string(script, &table);
    let errs = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "/game/root/sound/zom/zom_vo.asset",
        Some(cwtools_validation::Game::Hoi4),
        Some(&idx),
        None,
    );
    let cw113: Vec<_> = errs.iter().filter(|e| e.code == Some("CW113")).collect();
    assert!(
        cw113.is_empty(),
        "sound file beside the .asset should resolve, got: {:?}",
        cw113
    );
}

/// `filepath[gfx/,.dds]` declares both a root prefix and an extension. A
/// reference that already carries either one — in any case — must not have it
/// appended or prepended a second time.
const PREFIX_EXT_CWT: &str = r#"
spriteType = {
    texturefile = filepath[gfx/,.dds]
}
types = {
    type[spriteType] = {
        path = "game/interface"
    }
}
"#;

fn filepath_errors(script: &str, indexed: &[&str]) -> Vec<(Option<&'static str>, String)> {
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(PREFIX_EXT_CWT, &table), &table);
    let mut idx = TypeIndex::new();
    idx.file_index
        .add_paths(indexed.iter().map(|s| s.to_string()));
    let parsed = parse_string(script, &table);
    validate_ast(
        &parsed,
        &ruleset,
        &table,
        "game/interface/test.gfx",
        Some(cwtools_validation::Game::Hoi4),
        Some(&idx),
        None,
    )
    .into_iter()
    .filter(|e| e.code == Some("CW113"))
    .map(|e| (e.code, e.message))
    .collect()
}

#[test]
fn extension_already_present_in_mixed_case_is_not_appended_twice() {
    // `.DDS` is the configured `.dds` in another case. Appending would look for
    // `gfx/x.DDS.dds`, which is not what the engine resolves.
    let errs = filepath_errors(
        "spriteType = {\n    texturefile = \"gfx/x.DDS\"\n}\n",
        &["gfx/x.dds"],
    );
    assert!(
        errs.is_empty(),
        "a mixed-case extension must count as already present, got: {errs:?}"
    );
}

#[test]
fn prefix_already_present_in_mixed_case_is_not_prepended_twice() {
    // `GFX/` is the configured `gfx/` in another case. Prepending would look for
    // `gfx/GFX/x.dds`.
    let errs = filepath_errors(
        "spriteType = {\n    texturefile = \"GFX/x.dds\"\n}\n",
        &["gfx/x.dds"],
    );
    assert!(
        errs.is_empty(),
        "a mixed-case prefix must count as already present, got: {errs:?}"
    );
}

#[test]
fn missing_prefix_and_extension_are_both_supplied() {
    // The complement of the two above: a bare reference gets both, and the
    // resulting candidate is what resolves against the index.
    let errs = filepath_errors(
        "spriteType = {\n    texturefile = \"x\"\n}\n",
        &["gfx/x.dds"],
    );
    assert!(
        errs.is_empty(),
        "a bare reference must resolve as gfx/x.dds, got: {errs:?}"
    );
}

#[test]
fn a_genuinely_missing_prefixed_reference_still_reports_the_built_candidate() {
    // Guard against blanket suppression, and pin the message: the candidate the
    // diagnostic names is the prefixed-and-extended form actually looked up.
    let errs = filepath_errors(
        "spriteType = {\n    texturefile = \"ghost\"\n}\n",
        &["gfx/x.dds"],
    );
    assert_eq!(
        errs.len(),
        1,
        "a missing reference must report, got: {errs:?}"
    );
    assert!(
        errs[0].1.contains("gfx/ghost.dds"),
        "the diagnostic should name the built candidate, got: {:?}",
        errs[0].1
    );
}

#[test]
fn texture_sibling_lookup_handles_a_mixed_case_non_ascii_path() {
    // The sibling swap rewrites the last four bytes of the candidate. A
    // non-ASCII stem makes that a byte offset that must still land on a
    // character boundary, and the extension is matched case-insensitively.
    // No configured extension here: with one, the field appends it before the
    // sibling swap is ever reached.
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(TEXTURE_CWT, &table), &table);
    let mut idx = TypeIndex::new();
    idx.file_index.add_paths(["gfx/café.dds".to_string()]);

    let parsed = parse_string(
        "spriteType = {\n    texturefile = \"gfx/café.TGA\"\n}\n",
        &table,
    );
    let errs = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "game/interface/test.gfx",
        Some(cwtools_validation::Game::Hoi4),
        Some(&idx),
        None,
    );
    let cw113: Vec<_> = errs.iter().filter(|e| e.code == Some("CW113")).collect();
    assert!(
        cw113.is_empty(),
        "a .TGA reference should resolve via its .dds sibling, got: {cw113:?}"
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
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

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

    // Valid reference: no error
    let parsed_v = parse_string(script_valid, &table);
    let errs_v = validate_ast(
        &parsed_v,
        &ruleset,
        &table,
        "game/events/test.txt",
        None,
        Some(&idx),
        None,
    );
    assert!(
        errs_v.is_empty(),
        "Expected no errors for valid ref, got: {:?}",
        errs_v
    );

    // Bogus reference: should produce CW500
    let parsed_b = parse_string(script_bogus, &table);
    let errs_b = validate_ast(
        &parsed_b,
        &ruleset,
        &table,
        "game/events/test.txt",
        None,
        Some(&idx),
        None,
    );
    let type_errs: Vec<_> = errs_b.iter().filter(|e| e.code == Some("CW500")).collect();
    assert!(
        !type_errs.is_empty(),
        "Expected CW500 for bogus type ref, got: {:?}",
        errs_b
    );

    // Without type_index: no type-ref errors even for bogus reference
    let errs_no_idx = validate_ast(
        &parsed_b,
        &ruleset,
        &table,
        "game/events/test.txt",
        None,
        None,
        None,
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
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

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
    let parsed = parse_string(script, &table);
    let errors = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "game/common/ai_strategy/test.txt",
        Some(cwtools_game::constants::Game::Hoi4),
        None,
        None,
    );
    assert!(
        errors.is_empty(),
        "Expected no errors but got: {:?}",
        errors
    );
}

const RECURSIVE_ALIAS_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
"#;

const RECURSIVE_DISTINCT_ALIAS_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
## severity = warning
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
"#;

fn recursive_alias_script(depth: usize) -> String {
    let mut script = String::from("foo = {\n");
    for _ in 0..depth {
        script.push_str("recurse = {\n");
    }
    script.push_str("bad = nope\n");
    for _ in 0..=depth {
        script.push_str("}\n");
    }
    script
}

fn validate_aliases_at(
    rules: &str,
    script: &str,
    file_path: &str,
) -> Vec<cwtools_validation::ValidationError> {
    let table = StringTable::new();
    let parsed_rules = parse_string(rules, &table);
    let ruleset = ast_to_ruleset(&parsed_rules, &table);
    let parsed_script = parse_string(script, &table);
    validate_ast(
        &parsed_script,
        &ruleset,
        &table,
        file_path,
        None,
        None,
        None,
    )
}

fn validate_aliases(rules: &str, script: &str) -> Vec<cwtools_validation::ValidationError> {
    validate_aliases_at(rules, script, "game/common/foo/test.txt")
}

fn validate_recursive_aliases(
    rules: &str,
    depth: usize,
) -> Vec<cwtools_validation::ValidationError> {
    validate_aliases(rules, &recursive_alias_script(depth))
}

fn sibling_alias_script(count: usize) -> String {
    let mut script = String::from("foo = {\n");
    for _ in 0..count {
        script.push_str("recurse = { }\n");
    }
    script.push_str("}\n");
    script
}

#[test]
fn duplicate_recursive_aliases_reuse_one_candidate() {
    let errors = validate_recursive_aliases(RECURSIVE_ALIAS_RULES, 20);
    assert!(
        errors.iter().any(|error| error.code == Some("CW263")),
        "expected the deepest invalid field, got: {errors:?}"
    );
    assert!(
        errors.iter().all(|error| error.code != Some("CW277")),
        "equivalent candidates must not exhaust the branch budget: {errors:?}"
    );
}

#[test]
fn alias_branch_budget_accepts_exact_capacity() {
    // 32,768 two-overload usages reserve exactly the 65,536-branch budget.
    let errors = validate_aliases(
        RECURSIVE_DISTINCT_ALIAS_RULES,
        &sibling_alias_script(32_768),
    );
    assert!(
        errors.is_empty(),
        "the last fully funded alias usage must remain valid: {errors:?}"
    );
}

/// The budget is the fallback guarantee: distinct usages have nothing to reuse,
/// so 32,769 of them still stop the file one usage past capacity.
#[test]
fn alias_branch_budget_rejects_the_next_usage() {
    let errors = validate_aliases(
        RECURSIVE_DISTINCT_ALIAS_RULES,
        &sibling_alias_script(32_769),
    );
    assert_eq!(errors.len(), 1, "expected one limit diagnostic: {errors:?}");
    assert_eq!(errors[0].code, Some("CW277"));
    assert_eq!(errors[0].severity, ErrorSeverity::Warning);
    // The usage that could not be funded, squiggling its key.
    assert_eq!((errors[0].line, errors[0].col), (32_770, 0));
    assert_eq!(errors[0].end, Some((32_770, 7)));
}

/// Distinct overloads (only the severity differs, so the equivalent-candidate
/// coalescing cannot collapse them) recurse into the same subtree in the same
/// state at every level. The memo answers the repeat, so a depth that used to
/// spend the whole budget validates to the bottom instead.
#[test]
fn distinct_recursive_aliases_are_memoized_not_capped() {
    let errors = validate_recursive_aliases(RECURSIVE_DISTINCT_ALIAS_RULES, 20);
    assert!(
        errors.iter().all(|error| error.code != Some("CW277")),
        "an equivalent repeated subtree must not exhaust the budget: {errors:?}"
    );
    assert!(
        errors.iter().any(|error| error.code == Some("CW263")),
        "the deepest invalid field should be reported: {errors:?}"
    );
}

/// The disjunction accepts on the first clean candidate, so a file that is
/// actually valid never opens the second overload and never spends the budget.
/// That is what keeps the cap off legitimate script: depth alone is not what
/// exhausts it, unresolvable depth is.
///
/// Kept to 40 deliberately. The deepest nesting in real content is 24, the
/// parser refuses to descend past `MAX_CLAUSE_DEPTH`, and a debug-build walk
/// this deep is already spending real stack on a 1-2 MiB test thread.
#[test]
fn valid_deep_recursion_is_never_capped() {
    let mut script = String::from("foo = {\n");
    for _ in 0..40 {
        script.push_str("recurse = {\n");
    }
    for _ in 0..=40 {
        script.push_str("}\n");
    }
    let errors = validate_aliases(RECURSIVE_DISTINCT_ALIAS_RULES, &script);
    assert!(
        errors.is_empty(),
        "40 levels of valid recursion must validate clean: {errors:?}"
    );
}

const RECURSIVE_PER_FILE_ALIAS_RULES: &str = r#"
types = {
    type[oob] = {
        path = "game/history/units"
        type_per_file = yes
    }
}
oob = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
## severity = warning
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
"#;

/// A `type_per_file` file is validated and returned from its own branch, before
/// the root-child dispatch every other cap test goes through. That branch has to
/// report the limit too, or an OOB-shaped file stops validating in silence.
#[test]
fn type_per_file_recursion_stops_at_the_branch_budget() {
    // One usage past capacity, all of them distinct, so there is nothing for the
    // memo to reuse and the budget is what stops the file.
    let mut script = String::new();
    for _ in 0..32_769 {
        script.push_str("recurse = { }\n");
    }
    let errors = validate_aliases_at(
        RECURSIVE_PER_FILE_ALIAS_RULES,
        &script,
        "history/units/test.txt",
    );
    let capped: Vec<_> = errors
        .iter()
        .filter(|error| error.code == Some("CW277"))
        .collect();
    assert_eq!(capped.len(), 1, "expected one limit diagnostic: {errors:?}");
    assert_eq!(capped[0].severity, ErrorSeverity::Warning);
}

/// The same file under a single overload never branches, so it validates to the
/// bottom and reports the invalid field instead of the limit. Without this the
/// test above could pass on a rule set that caps everything.
#[test]
fn type_per_file_recursion_without_overloads_is_not_capped() {
    const SINGLE: &str = r#"
types = {
    type[oob] = {
        path = "game/history/units"
        type_per_file = yes
    }
}
oob = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
"#;
    let mut script = String::new();
    for _ in 0..20 {
        script.push_str("recurse = {\n");
    }
    script.push_str("bad = nope\n");
    for _ in 0..20 {
        script.push_str("}\n");
    }
    let errors = validate_aliases_at(SINGLE, &script, "history/units/test.txt");
    assert!(
        errors.iter().all(|error| error.code != Some("CW277")),
        "a single overload never reserves a branch: {errors:?}"
    );
    assert!(
        errors.iter().any(|error| error.code == Some("CW263")),
        "the deepest invalid field should still be reported: {errors:?}"
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
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    // `ai_strategy` is a block-only overload, but it's used as a bare scalar.
    let script = r#"my_strat = {
    ai_strategy = oops
}
"#;
    let parsed = parse_string(script, &table);
    let errors = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "game/common/ai_strategy/test.txt",
        Some(cwtools_game::constants::Game::Hoi4),
        None,
        None,
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
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    let script = r#"
my_plan = {
    research = {
        CAT_inf = 10.0
    }
}
"#;
    let parsed = parse_string(script, &table);
    let errors = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "game/common/ai_strategy_plans/test.txt",
        Some(cwtools_game::constants::Game::Hoi4),
        None,
        None,
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
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    // Both quoted and unquoted tag keys must validate cleanly.
    let script = r#"
diplo = {
    "AST" = { value = 1 }
    LOG = { value = 2 }
}
"#;
    let parsed = parse_string(script, &table);
    let errors = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "game/common/diplo/test.txt",
        Some(cwtools_game::constants::Game::Hoi4),
        None,
        None,
    );
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
    let parsed_bad = parse_string(bad, &table);
    let errors = validate_ast(
        &parsed_bad,
        &ruleset,
        &table,
        "game/common/diplo/test.txt",
        Some(cwtools_game::constants::Game::Hoi4),
        None,
        None,
    );
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
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);
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
    let parsed = parse_string(script, &table);
    let errors = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "game/events/test.txt",
        Some(cwtools_game::constants::Game::Hoi4),
        None,
        None,
    );
    assert!(
        !errors.iter().any(|e| e.message.contains("character")),
        "Expected no 'Unexpected field character' error, got: {:?}",
        errors
    );
}

#[test]
fn test_gfx_sprite_type_parses_and_validates() {
    // Minimal rules matching HOI4 interface/gfx.cwt: spriteType lives in
    // .gfx files under interface/, skipping the spriteTypes root key.
    let cwt = r#"
types = {
    type[spriteType] = {
        path = "game/interface"
        skip_root_key = spriteTypes
        path_extension = .gfx
        name_field = "name"
        ## type_key_filter = spriteType
        subtype[spriteType] = { }
    }
}

spriteType = {
    name = scalar
    ## cardinality = 0..1
    textureFile = scalar
    ## cardinality = 0..1
    noOfFrames = int
}
"#;

    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    // Valid .gfx file: a spriteType wrapped in spriteTypes { }
    let script = r#"
spriteTypes = {
    spriteType = {
        name = "GFX_my_icon"
        textureFile = "gfx/interface/my_icon.dds"
    }
}
"#;
    let parsed = parse_string(script, &table);
    // Path must match the type's path (interface/) and extension (.gfx)
    let errors = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "game/interface/my_mod.gfx",
        Some(cwtools_game::constants::Game::Hoi4),
        None,
        None,
    );
    assert!(
        errors.is_empty(),
        "Expected no errors for valid .gfx, got: {:?}",
        errors
    );

    // The same content in a .txt file should NOT match the spriteType type
    // (path_extension = .gfx filters it out), so no type errors but also no
    // validation against spriteType rules (the file is simply untyped).
    let errors_txt = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "game/interface/my_mod.txt",
        Some(cwtools_game::constants::Game::Hoi4),
        None,
        None,
    );
    // A .txt file won't match the type (wrong extension), so validation is
    // lenient (no rules apply). Either 0 errors or only non-type errors are OK.
    let _ = errors_txt; // just ensure it doesn't panic
}

#[test]
fn test_gfx_sprite_type_indexed_for_reference_resolution() {
    // spriteType instances defined in a .gfx file must appear in the TypeIndex
    // so that <spriteType> references in .gui files resolve correctly.
    let cwt = r#"
types = {
    type[spriteType] = {
        path = "game/interface"
        skip_root_key = spriteTypes
        path_extension = .gfx
        name_field = "name"
    }
    type[widget] = {
        path = "game/interface"
        path_extension = .gui
        skip_root_key = guiTypes
    }
}

widget = {
    name = scalar
    ## cardinality = 0..1
    spriteType = <spriteType>
}
"#;

    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    // Build a TypeIndex that includes a known sprite (as if collected from a .gfx file)
    let mut idx = TypeIndex::new();
    let mut map = HashMap::new();
    map.insert(
        "spriteType".to_string(),
        vec![TypeInstance {
            name: "GFX_my_icon".to_string(),
            location: SourceLocation {
                line: 1,
                col: 0,
                end: (1, 0),
            },
            primary_loc_key: None,
            required_loc_keys: Vec::new(),
        }],
    );
    idx.merge("game/interface/sprites.gfx", map);
    idx.complete = true;

    // .gui file referencing the known sprite — should be clean
    let gui_valid = r#"
guiTypes = {
    widget = {
        name = "my_widget"
        spriteType = GFX_my_icon
    }
}
"#;
    let parsed_valid = parse_string(gui_valid, &table);
    let errs_valid = validate_ast(
        &parsed_valid,
        &ruleset,
        &table,
        "game/interface/my_widget.gui",
        Some(cwtools_game::constants::Game::Hoi4),
        Some(&idx),
        None,
    );
    let type_errs: Vec<_> = errs_valid
        .iter()
        .filter(|e| e.code == Some("CW500"))
        .collect();
    assert!(
        type_errs.is_empty(),
        "Valid sprite ref should not produce CW500, got: {:?}",
        errs_valid
    );

    // .gui file referencing an unknown sprite — should produce CW500
    let gui_bad = r#"
guiTypes = {
    widget = {
        name = "my_widget"
        spriteType = GFX_nonexistent
    }
}
"#;
    let parsed_bad = parse_string(gui_bad, &table);
    let errs_bad = validate_ast(
        &parsed_bad,
        &ruleset,
        &table,
        "game/interface/my_widget.gui",
        Some(cwtools_game::constants::Game::Hoi4),
        Some(&idx),
        None,
    );
    let type_errs_bad: Vec<_> = errs_bad
        .iter()
        .filter(|e| e.code == Some("CW500"))
        .collect();
    assert!(
        !type_errs_bad.is_empty(),
        "Bogus sprite ref should produce CW500, got: {:?}",
        errs_bad
    );
}

#[test]
fn test_gui_container_window_type_parses_and_validates() {
    // Minimal rules matching HOI4 interface/gui.cwt: containerWindowType lives
    // in .gui files under interface/, skipping the guiTypes root key.
    let cwt = r#"
types = {
    type[containerWindowType] = {
        path = "game/interface"
        name_field = "name"
        path_extension = .gui
        skip_root_key = guiTypes
    }
}

containerWindowType = {
    name = scalar
    ## cardinality = 0..1
    moveable = bool
}
"#;

    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

    // Valid .gui file
    let script = r#"
guiTypes = {
    containerWindowType = {
        name = "my_window"
        moveable = yes
    }
}
"#;
    let parsed = parse_string(script, &table);
    let errors = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "game/interface/my_window.gui",
        Some(cwtools_game::constants::Game::Hoi4),
        None,
        None,
    );
    assert!(
        errors.is_empty(),
        "Expected no errors for valid .gui, got: {:?}",
        errors
    );

    // Malformed .gui: wrong type for moveable (expects bool, got int)
    let bad_script = r#"
guiTypes = {
    containerWindowType = {
        name = "my_window"
        moveable = 42
    }
}
"#;
    let parsed_bad = parse_string(bad_script, &table);
    let errors_bad = validate_ast(
        &parsed_bad,
        &ruleset,
        &table,
        "game/interface/my_window.gui",
        Some(cwtools_game::constants::Game::Hoi4),
        None,
        None,
    );
    assert!(
        !errors_bad.is_empty(),
        "Expected a validation error for moveable = 42, got none"
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
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let ruleset = ast_to_ruleset(&parsed_cwt, &table);

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
    let parsed = parse_string(script, &table);
    let errs = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "game/events/test.txt",
        None,
        Some(&idx),
        None,
    );
    assert!(
        errs.iter().all(|e| e.code != Some("CW500")),
        "scripted-loc [..] value must not flag CW500, got: {:?}",
        errs
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
    let table = StringTable::new();
    let parsed_cwt = parse_string(cwt, &table);
    let mut ruleset = ast_to_ruleset(&parsed_cwt, &table);
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
    let parsed = parse_string(script, &table);
    let errors = validate_ast(
        &parsed,
        &ruleset,
        &table,
        "game/events/test.txt",
        Some(cwtools_game::constants::Game::Hoi4),
        Some(&idx),
        None,
    );
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
    let parsed_bad = parse_string(bad, &table);
    let errors_bad = validate_ast(
        &parsed_bad,
        &ruleset,
        &table,
        "game/events/test.txt",
        Some(cwtools_game::constants::Game::Hoi4),
        Some(&idx),
        None,
    );
    assert!(
        errors_bad.iter().any(|e| e.message.contains("notathing")),
        "a truly unknown scope key must still flag, got: {errors_bad:?}",
    );
}
