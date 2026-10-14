use std::collections::HashSet;
use std::path::PathBuf;

use cwtools_driver::{RulesInput, Session, SessionConfig, index_game_dir};
use cwtools_file_manager::file_manager::FileError;
use cwtools_game::constants::Game;
use cwtools_index::variable_defining_effects;
use cwtools_rules::ruleset_loader::load_ruleset_from_dir;
use cwtools_string_table::string_table::StringTable;

use crate::common::{
    FILEPATH_RULES, THING_RULES, load_perf_session, loc_gate_workspace, perf_mod, perf_rules,
};

// ── index_game_dir ───────────────────────────────────────────────────────────

fn total_instances(index: &cwtools_index::TypeIndex) -> usize {
    index.map().values().map(|v| v.len()).sum()
}

/// Indexing a fixture dir parses + collects type instances, and the instance
/// count is stable across two runs (the merge order is deterministic).
#[test]
fn index_game_dir_is_populated_and_stable() {
    let table = StringTable::new();
    let (ruleset, _errors) = load_ruleset_from_dir(
        &perf_rules(),
        &table,
        cwtools_file_manager::file_manager::ScanBudget::default(),
    );
    let var_effects = variable_defining_effects(&ruleset);

    let first = index_game_dir(&perf_mod(), &ruleset, &table, &var_effects).unwrap();
    let second = index_game_dir(&perf_mod(), &ruleset, &table, &var_effects).unwrap();

    let n1 = total_instances(&first);
    assert!(n1 > 0, "expected the fixture to yield type instances");
    assert_eq!(
        n1,
        total_instances(&second),
        "instance count must be deterministic across runs"
    );
    // Events are the largest type in the fixture; the config defines `event`.
    assert!(
        first.map().contains_key("event"),
        "expected `event` instances, got types: {:?}",
        first.map().keys().collect::<Vec<_>>()
    );
}

/// Missing rules degrade gracefully: an empty ruleset yields an empty index,
/// not a panic.
#[test]
fn index_game_dir_empty_ruleset_yields_empty_index() {
    let table = StringTable::new();
    let ruleset = cwtools_rules::rules_types::RuleSet::new();
    let index = index_game_dir(&perf_mod(), &ruleset, &table, &HashSet::new()).unwrap();
    assert_eq!(total_instances(&index), 0);
}

#[test]
fn index_game_dir_missing_root_returns_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no_such_vanilla");
    let table = StringTable::new();
    let ruleset = cwtools_rules::rules_types::RuleSet::new();
    let result = index_game_dir(&missing, &ruleset, &table, &HashSet::new());

    assert!(matches!(
        result,
        Err(FileError::MissingRoot(path)) if path == missing
    ));
}

// ── Session::load / validate_all ─────────────────────────────────────────────

/// The full load pipeline runs end to end on the fixture: discovery succeeds,
/// a non-empty type index is built, the scope registry is prebuilt, and files
/// are resident for the batch path.
#[test]
fn session_load_builds_indexes() {
    let session = load_perf_session();
    assert!(!session.discovery_failed, "discovery should not fail");
    assert!(!session.parsed_files().is_empty(), "mod files should parse");
    assert!(
        !session.type_index().map().is_empty(),
        "type index should be populated"
    );
    assert!(
        !session.ruleset().types.is_empty(),
        "ruleset should carry type definitions"
    );
    assert!(
        session.registry().is_some(),
        "a game is set, so the scope registry should be prebuilt"
    );
}

#[test]
fn session_missing_vanilla_root_is_a_discovery_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no_such_vanilla");
    let session = Session::load(SessionConfig {
        game: Game::Stellaris,
        rules: RulesInput::Dir(perf_rules()),
        directory: perf_mod(),
        parent_roots: Vec::new(),
        vanilla: Some(missing),
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    });

    assert!(
        session.discovery_failed,
        "missing vanilla root must fail discovery"
    );
    assert!(
        !session.type_index().complete,
        "a failed vanilla walk must not mark its empty index complete"
    );
}

#[test]
fn session_batch_index_collects_scripted_gui_callbacks_not_outer_names() {
    let tmp = tempfile::tempdir().unwrap();
    let rules = tmp.path().join("rules");
    let mod_root = tmp.path().join("mod");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::create_dir_all(mod_root.join("common/scripted_guis")).unwrap();
    std::fs::write(
        rules.join("scripted_guis.cwt"),
        r#"
types = {
    type[scripted_gui] = {
        path = "game/common/scripted_guis"
        skip_root_key = any
    }
}
scripted_gui = { }
"#,
    )
    .unwrap();
    std::fs::write(
        mod_root.join("common/scripted_guis/example.txt"),
        r#"
scripted_gui = {
    outer_gui = {
        effects = { Topbar_Icon_Click = { nested_effect = yes } }
        visible = { unrelated_key = yes }
    }
}
"#,
    )
    .unwrap();

    let session = Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(rules),
        directory: mod_root,
        parent_roots: Vec::new(),
        vanilla: None,
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    });

    let callbacks = &session.type_index().scripted_gui_index;
    assert!(callbacks.contains("topbar_icon_click"));
    assert!(!callbacks.contains("outer_gui"));
    assert!(!callbacks.contains("nested_effect"));
    assert!(!callbacks.contains("unrelated_key"));
}

#[test]
fn changed_source_is_not_validated_against_a_stale_index() {
    let tmp = loc_gate_workspace(None);
    let session = Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(tmp.path().join("rules")),
        directory: tmp.path().join("mod"),
        parent_roots: Vec::new(),
        vanilla: None,
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    });
    std::fs::write(
        tmp.path().join("mod/common/things/x.txt"),
        "changed_thing = { different_value = yes }\n",
    )
    .unwrap();

    assert!(
        session
            .validate_all()
            .into_iter()
            .flat_map(|(_, errors)| errors)
            .any(|error| error.message.contains("changed after indexing"))
    );
}

#[test]
fn parent_mods_are_ordered_reference_layers_but_not_validation_targets() {
    let tmp = tempfile::tempdir().unwrap();
    let rules = tmp.path().join("rules");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(rules.join("things.cwt"), THING_RULES).unwrap();

    let mod_root = tmp.path().join("mod");
    let first_parent = tmp.path().join("first-parent");
    let second_parent = tmp.path().join("second-parent");
    let vanilla = tmp.path().join("vanilla");
    for root in [&mod_root, &first_parent, &second_parent, &vanilla] {
        std::fs::create_dir_all(root.join("common/things")).unwrap();
    }
    std::fs::write(mod_root.join("common/things/main.txt"), "shared = { }\n").unwrap();
    // Distinct file paths preserve all same-named definitions, so the
    // TypeIndex's source order exposes the actual layer priority.
    std::fs::write(
        first_parent.join("common/things/first.txt"),
        "shared = { }\nfirst_only = { }\n",
    )
    .unwrap();
    std::fs::write(
        second_parent.join("common/things/second.txt"),
        "shared = { }\nsecond_only = { }\n",
    )
    .unwrap();
    std::fs::write(vanilla.join("common/things/vanilla.txt"), "shared = { }\n").unwrap();
    std::fs::create_dir_all(first_parent.join("gfx")).unwrap();
    std::fs::write(first_parent.join("gfx/parent_asset.dds"), b"").unwrap();

    // This parent localisation file is malformed. It must not be returned as
    // a project diagnostic even though a valid parent file contributes keys.
    std::fs::create_dir_all(mod_root.join("localisation")).unwrap();
    std::fs::create_dir_all(first_parent.join("localisation")).unwrap();
    std::fs::create_dir_all(second_parent.join("localisation")).unwrap();
    std::fs::create_dir_all(vanilla.join("localisation")).unwrap();
    std::fs::write(
        mod_root.join("localisation/main_l_english.yml"),
        "l_english:\n shared_key:0 \"Workspace [GetName]\"\n",
    )
    .unwrap();
    std::fs::write(
        first_parent.join("localisation/parent_l_english.yml"),
        "l_english:\n shared_key:0 \"Parent [GetName]\"\n parent_key:0 \"Parent text\"\n parent_priority:0 \"First [GetName]\"\n",
    )
    .unwrap();
    std::fs::write(
        first_parent.join("localisation/broken_l_english.yml"),
        "l_english:\n broken\n",
    )
    .unwrap();
    std::fs::write(
        second_parent.join("localisation/second_l_english.yml"),
        "l_english:\n second_parent_key:0 \"Second parent\"\n parent_priority:0 \"Second [GetName]\"\n",
    )
    .unwrap();
    std::fs::write(
        vanilla.join("localisation/vanilla_l_english.yml"),
        "l_english:\n shared_key:0 \"Vanilla [GetName]\"\n vanilla_key:0 \"Vanilla text\"\n parent_priority:0 \"Vanilla [GetName]\"\n",
    )
    .unwrap();

    let session = Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(rules),
        directory: mod_root.clone(),
        parent_roots: vec![first_parent.clone(), second_parent.clone()],
        vanilla: Some(vanilla.clone()),
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    });

    let shared_sources: Vec<_> = session
        .type_index()
        .instances("thing")
        .iter()
        .filter(|(_, instance)| instance.name == "shared")
        .map(|(uri, _)| PathBuf::from(uri.as_ref()))
        .collect();
    assert_eq!(
        shared_sources,
        [
            mod_root.join("common").join("things").join("main.txt"),
            first_parent.join("common").join("things").join("first.txt"),
            second_parent
                .join("common")
                .join("things")
                .join("second.txt"),
            vanilla.join("common").join("things").join("vanilla.txt"),
        ],
        "priority is workspace, parents in declaration order, then vanilla"
    );
    assert_eq!(
        session
            .type_index()
            .workspace_definition_count("thing", "shared"),
        1,
        "parents and vanilla have base-game provenance, not workspace provenance"
    );
    assert!(session.type_index().contains("thing", "first_only"));
    assert!(session.type_index().contains("thing", "second_only"));
    assert!(
        session
            .type_index()
            .file_index
            .contains("gfx/parent_asset.dds")
    );
    assert_eq!(session.parsed_files().len(), 1);
    assert_eq!(
        session.parsed_files()[0].path,
        mod_root.join("common/things/main.txt")
    );
    let validation_paths: Vec<_> = session
        .validate_all()
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    assert_eq!(validation_paths, [mod_root.join("common/things/main.txt")]);

    let loc_index = session.loc_index();
    assert_eq!(
        loc_index.entry("shared_key").unwrap().desc,
        "\"Workspace [GetName]\""
    );
    assert!(
        loc_index.exists_any("parent_key"),
        "loc keys: {:?}",
        loc_index.union()
    );
    assert!(loc_index.exists_any("second_parent_key"));
    assert!(loc_index.exists_any("vanilla_key"));
    assert_eq!(
        loc_index.entry("parent_priority").unwrap().desc,
        "\"Second [GetName]\"",
        "later-loaded parent wins over earlier parents and vanilla"
    );
    assert!(
        session
            .loc_project_diagnostics()
            .iter()
            .all(|diagnostic| !diagnostic.file.contains("first-parent"))
    );
}

#[test]
fn inline_scripts_from_parent_mods_are_available_without_validating_parent_files() {
    let tmp = tempfile::tempdir().unwrap();
    let rules = tmp.path().join("rules");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(
        rules.join("things.cwt"),
        r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    ## cardinality = 1..1
    id = scalar
    ## cardinality = 0..inf
    add = int
}
"#,
    )
    .unwrap();
    let mod_root = tmp.path().join("mod");
    let parent = tmp.path().join("parent");
    std::fs::create_dir_all(mod_root.join("common/foo")).unwrap();
    std::fs::create_dir_all(parent.join("common/inline_scripts")).unwrap();
    let mod_file = mod_root.join("common/foo/main.txt");
    let parent_script = parent.join("common/inline_scripts/bonus.txt");
    std::fs::write(
        &mod_file,
        "sample = { id = x inline_script = { script = bonus } }\n",
    )
    .unwrap();
    std::fs::write(&parent_script, "not_a_field = 5\n").unwrap();

    let session = Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(rules),
        directory: mod_root,
        parent_roots: vec![parent],
        vanilla: None,
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    });
    let results = session.validate_all();
    assert_eq!(
        results.len(),
        1,
        "only the primary file is validated: {results:?}"
    );
    assert_eq!(results[0].0, mod_file);
    assert!(
        results[0].1.iter().any(|error| {
            error.code == Some("CW263") && error.message.contains("common/inline_scripts/bonus.txt")
        }),
        "the parent inline script body must be expanded at its call site: {:?}",
        results[0].1
    );
    assert!(
        !session
            .parsed_files()
            .iter()
            .any(|source| source.path == parent_script)
    );
}

/// validate_all runs the whole batch without panicking and returns one entry
/// per parsed file. The total error count is deterministic across two loads.
#[test]
fn session_validate_all_is_deterministic() {
    let s1 = load_perf_session();
    let r1 = s1.validate_all();
    assert_eq!(
        r1.len(),
        s1.parsed_files().len(),
        "validate_all returns one result per parsed file"
    );
    let errors1: usize = r1.iter().map(|(_, e)| e.len()).sum();

    let s2 = load_perf_session();
    let errors2: usize = s2.validate_all().iter().map(|(_, e)| e.len()).sum();

    assert_eq!(
        errors1, errors2,
        "validation output must be deterministic across runs"
    );
}

// ── mod-defined complex-enum members (#454) ─────────────────────────────────

const COMPLEX_ENUM_RULES: &str = r#"
enums = {
    complex_enum[my_enum] = {
        path = "game/common/my_enum"
        start_from_root = yes
        name = {
            enum_name = scalar
        }
    }
}
"#;

/// A mod-defined complex-enum member must reach the driver's `TypeIndex`, not
/// only the vanilla-cache path (#454): the per-mod-file loop has to collect and
/// merge `complex_enum_values` the same way `collect.rs`'s sequential builder
/// does.
#[test]
fn session_load_collects_mod_defined_complex_enum_members() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("rules")).unwrap();
    std::fs::write(
        tmp.path().join("rules").join("enums.cwt"),
        COMPLEX_ENUM_RULES,
    )
    .unwrap();
    let enum_dir = tmp.path().join("mod").join("common").join("my_enum");
    std::fs::create_dir_all(&enum_dir).unwrap();
    std::fs::write(enum_dir.join("x.txt"), "MY_VALUE = \"something\"\n").unwrap();

    let session = Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(tmp.path().join("rules")),
        directory: tmp.path().join("mod"),
        parent_roots: Vec::new(),
        vanilla: None,
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    });

    assert!(
        session
            .type_index()
            .complex_enum_values
            .contains("my_enum", "MY_VALUE"),
        "mod-defined complex-enum member should reach the index"
    );
}

// ── CW113 case-sensitive filepath check ─────────────────────────────────────

/// A mod whose `ref.gfx` references `gfx/test/button.dds` while the on-disk file
/// is `Button.DDS` (case differs), plus a minimal vanilla install so the file
/// index (mod walk) is populated.
fn filepath_workspace() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("rules")).unwrap();
    std::fs::write(tmp.path().join("rules").join("gfx.cwt"), FILEPATH_RULES).unwrap();
    let gfx_dir = tmp.path().join("mod").join("gfx");
    std::fs::create_dir_all(&gfx_dir).unwrap();
    std::fs::write(
        gfx_dir.join("ref.gfx"),
        "spriteType = { texturefile = \"gfx/test/button.dds\" }\n",
    )
    .unwrap();
    let asset = gfx_dir.join("test");
    std::fs::create_dir_all(&asset).unwrap();
    std::fs::write(asset.join("Button.DDS"), b"").unwrap();
    std::fs::create_dir_all(tmp.path().join("vanilla").join("common")).unwrap();
    std::fs::write(
        tmp.path().join("vanilla").join("common").join("dummy.txt"),
        "x = {}\n",
    )
    .unwrap();
    tmp
}

fn cw113_count(workspace: &std::path::Path, case_sensitive: bool) -> usize {
    let session = Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(workspace.join("rules")),
        directory: workspace.join("mod"),
        parent_roots: Vec::new(),
        vanilla: Some(workspace.join("vanilla")),
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: case_sensitive,
        on_rules_diagnostic: None,
    });
    session
        .validate_all()
        .iter()
        .flat_map(|(_, errs)| errs.iter())
        .filter(|e| e.code == Some("CW113"))
        .count()
}

#[test]
fn cw113_case_mismatch_only_flagged_in_case_sensitive_mode() {
    let tmp = filepath_workspace();
    assert_eq!(
        cw113_count(tmp.path(), false),
        0,
        "case-insensitive (default) must resolve a case-differing reference"
    );
    assert_eq!(
        cw113_count(tmp.path(), true),
        1,
        "case-sensitive mode must flag a reference that only differs by case"
    );
}

#[test]
fn primary_replace_path_hides_parent_resources_from_file_references() {
    let tmp = tempfile::tempdir().unwrap();
    let rules = tmp.path().join("rules");
    let primary = tmp.path().join("mod");
    let parent = tmp.path().join("parent");
    let vanilla = tmp.path().join("vanilla");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::create_dir_all(primary.join("gfx")).unwrap();
    std::fs::create_dir_all(parent.join("gfx")).unwrap();
    std::fs::create_dir_all(&vanilla).unwrap();
    std::fs::write(rules.join("gfx.cwt"), FILEPATH_RULES).unwrap();
    std::fs::write(
        primary.join("descriptor.mod"),
        "name = \"Primary\"\nreplace_path = \"gfx\"\n",
    )
    .unwrap();
    std::fs::write(
        primary.join("gfx/ref.gfx"),
        "spriteType = { texturefile = \"gfx/hidden.dds\" }\n",
    )
    .unwrap();
    std::fs::write(parent.join("gfx/hidden.dds"), b"").unwrap();

    assert!(
        cwtools_driver::vanilla_replacement_shadow(&primary, &[]).hides("gfx/hidden.dds"),
        "the primary descriptor should establish the gfx replacement shadow"
    );

    let session = Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(rules),
        directory: primary,
        parent_roots: vec![parent],
        vanilla: Some(vanilla),
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    });

    assert!(
        !session.type_index().file_index.contains("gfx/hidden.dds"),
        "a parent resource under the primary mod's replace_path is shadowed"
    );
    let diagnostics = session.validate_all();
    assert_eq!(
        diagnostics
            .iter()
            .flat_map(|(_, errors)| errors.iter())
            .filter(|error| error.code == Some("CW113"))
            .count(),
        1,
        "the reference to a hidden parent resource must retain CW113: {diagnostics:?}"
    );
}
