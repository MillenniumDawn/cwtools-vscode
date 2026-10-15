//! Type-attached modifier templates from the pinned game rules feed the
//! dynamic modifier set used by validation and completion.

use std::sync::OnceLock;

use cwtools_game::constants::Game;
use cwtools_index::{SourceLocation, TypeIndex, TypeInstance, collect_type_instances};
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_rules::rules_types::{RuleSet, TypeModifier};
use cwtools_rules::ruleset_loader::load_ruleset_from_dir;
use cwtools_string_table::string_table::StringTable;
use cwtools_test_support::stellaris_config;
use cwtools_validation::{
    Prepared, build_modifier_keys, build_scope_registry_arc, validate_prepared,
};

fn stellaris_ruleset() -> &'static RuleSet {
    static RULESET: OnceLock<RuleSet> = OnceLock::new();
    RULESET.get_or_init(|| {
        let table = StringTable::new();
        let (ruleset, _errors) = load_ruleset_from_dir(
            stellaris_config(),
            &table,
            cwtools_file_manager::file_manager::ScanBudget::default(),
        );
        ruleset
    })
}

fn job_index(table: &StringTable) -> TypeIndex {
    // This mirrors a real job definition file and lets the pinned `type[job]`
    // path declaration collect `farmer` as a job instance.
    let parsed = parse_string(r#"farmer = { category = worker }"#, table);
    let instances = collect_type_instances(
        stellaris_ruleset(),
        &parsed,
        "common/pop_jobs/00_jobs.txt",
        table,
    );
    let mut index = TypeIndex::new();
    index.merge("file:///game/common/pop_jobs/00_jobs.txt", instances);
    index
}

#[test]
fn pinned_stellaris_job_modifiers_are_indexed_and_validate() {
    let table = StringTable::new();
    let mut ruleset = stellaris_ruleset().clone();
    ruleset
        .types
        .iter_mut()
        .find(|td| td.name == "job")
        .expect("pinned Stellaris config should declare type[job]")
        .modifiers
        .extend([
            TypeModifier {
                prefix: String::new(),
                suffix: String::new(),
                is_template: true,
                category: "Planets".to_string(),
                documentation: None,
                explicit: false,
            },
            TypeModifier {
                prefix: String::new(),
                suffix: String::new(),
                is_template: false,
                category: "Planets".to_string(),
                documentation: None,
                explicit: false,
            },
        ]);
    let job = ruleset
        .types
        .iter()
        .find(|td| td.name == "job")
        .expect("pinned Stellaris config should declare type[job]");
    assert!(
        job.modifiers
            .iter()
            .any(|m| { m.prefix == "job_" && m.suffix == "_add" && m.category == "Planets" }),
        "the pinned type[job] declaration should parse `job_$_add = Planets`, got: {:?}",
        job.modifiers
    );

    let index = job_index(&table);
    assert!(
        index.contains("job", "farmer"),
        "the pinned type[job] path should collect the synthetic farmer instance"
    );
    let keys = build_modifier_keys(&ruleset, &index);
    for key in [
        "job_farmer_add",
        "job_farmer_per_pop",
        "job_farmer_per_crime",
        "farmer",
    ] {
        assert!(keys.contains(key), "generated modifier `{key}` missing");
    }
    assert!(
        !keys.contains("planets"),
        "a plain category is not a template"
    );

    // `pop_group_modifier` is a modifier alias in the pinned social-strata
    // rules. A generated job modifier must be accepted there and remain
    // eligible for the zero-modifier diagnostic.
    let parsed = parse_string(
        r#"worker = { pop_group_modifier = { job_farmer_add = 0 farmer = 0 } }"#,
        &table,
    );
    let registry = build_scope_registry_arc(&ruleset, Some(Game::Stellaris));
    let errors = validate_prepared(
        &parsed,
        "common/pop_categories/00_social_strata.txt",
        &Prepared {
            ruleset: &ruleset,
            table: &table,
            game: Some(Game::Stellaris),
            type_index: Some(&index),
            modifier_keys: Some(&keys),
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: true,
            var_checks: false,
        },
    );
    let codes: Vec<_> = errors
        .into_iter()
        .filter_map(|error| error.code.map(String::from))
        .collect();
    assert!(codes.contains(&"CW235".to_string()), "got: {codes:?}");
    assert!(
        !codes.contains(&"CW262".to_string()) && !codes.contains(&"CW263".to_string()),
        "a generated modifier should not be reported as an unexpected field: {codes:?}"
    );
}

#[test]
fn bare_type_modifier_templates_expand_but_plain_entries_do_not() {
    let table = StringTable::new();
    let cwt = parse_string(
        r#"types = {
            type[job] = { path = "game/common/jobs" modifiers = { "$" = Bare } }
            type[legacy] = { path = "game/common/legacy" modifiers = { Legacy = "$" } }
            type[plain] = { path = "game/common/plain" modifiers = { Static = Planets } }
        }"#,
        &table,
    );
    let ruleset = ast_to_ruleset(&cwt, &table);
    let instance = |name: &str| TypeInstance {
        name: name.to_string(),
        location: SourceLocation {
            line: 1,
            col: 0,
            end: (1, 0),
        },
        primary_loc_key: None,
        required_loc_keys: Vec::new(),
    };
    let mut index = TypeIndex::new();
    index.merge(
        "file:///game/common/types.txt",
        std::collections::HashMap::from([
            ("job".to_string(), vec![instance("farmer")]),
            ("legacy".to_string(), vec![instance("factory")]),
            ("plain".to_string(), vec![instance("ordinary")]),
        ]),
    );

    let keys = build_modifier_keys(&ruleset, &index);

    assert!(keys.contains("farmer"), "bare key-side `$` should expand");
    assert!(
        keys.contains("factory"),
        "the legacy value-side `$` form should still expand"
    );
    assert!(
        !keys.contains("ordinary"),
        "a non-template modifier declaration should not become a key"
    );
}

#[test]
fn type_and_global_modifier_templates_are_case_folded_together() {
    let table = StringTable::new();
    let mut ruleset = stellaris_ruleset().clone();
    ruleset
        .modifiers
        .push(("GLOBAL_<job>_MOD".to_string(), "Planets".to_string()));
    let parsed = parse_string(r#"Farmer = { category = worker }"#, &table);
    let instances =
        collect_type_instances(&ruleset, &parsed, "common/pop_jobs/00_jobs.txt", &table);
    let mut index = TypeIndex::new();
    index.merge("file:///game/common/pop_jobs/00_jobs.txt", instances);

    let keys = build_modifier_keys(&ruleset, &index);
    for key in ["job_farmer_add", "global_farmer_mod"] {
        assert!(keys.contains(key), "lowercase modifier `{key}` missing");
    }
    assert!(
        !keys.contains("JOB_FARMER_ADD") && !keys.contains("GLOBAL_FARMER_MOD"),
        "modifier membership stays case-insensitive with lowercase canonical keys"
    );
}

#[test]
fn pinned_hoi4_modifiers_keep_the_global_only_set() {
    let table = StringTable::new();
    let (ruleset, _errors) = load_ruleset_from_dir(
        cwtools_test_support::hoi4_config(),
        &table,
        cwtools_file_manager::file_manager::ScanBudget::default(),
    );
    assert!(
        ruleset.types.iter().all(|td| td.modifiers.is_empty()),
        "the pinned HOI4 config has no type-attached modifier templates"
    );

    let keys = build_modifier_keys(&ruleset, &TypeIndex::new());
    assert!(
        keys.contains("army_attack_factor"),
        "the existing HOI4 global modifier declaration should remain indexed"
    );
}
