//! Smoke tests for the shared driver pipeline (Session + primitives).
//!
//! The driver is the anti-drift hub between the CLI and the LSP: both call its
//! Session/pipeline primitives so the load sequence can't diverge. These tests
//! pin the pipeline against the checked-in `performancetest2` fixture (a
//! Stellaris mod slice with its own `.cwtools/config` ruleset) plus a couple of
//! synthesized temp dirs for the discovery-config helper. They assert the
//! pipeline loads, indexes, and validates without panicking, and that its
//! output is deterministic across runs.

use std::collections::HashSet;
use std::path::PathBuf;

use cwtools_driver::{
    RulesInput, Session, SessionConfig, VanillaCacheAuto, build_vanilla_cache_aux,
    discover_workspace_files, index_game_dir, search_config_for, workspace_discovery_config,
};
use cwtools_file_manager::file_manager::FileError;
use cwtools_game::constants::Game;
use cwtools_index::variable_defining_effects;
use cwtools_localization::Lang;
use cwtools_rules::ruleset_loader::load_ruleset_from_dir;
use cwtools_string_table::string_table::StringTable;

fn testfiles() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testfiles")
}

fn perf_mod() -> PathBuf {
    testfiles().join("performancetest2")
}

fn perf_rules() -> PathBuf {
    perf_mod().join(".cwtools").join("config")
}

fn total_instances(index: &cwtools_index::TypeIndex) -> usize {
    index.map.values().map(|v| v.len()).sum()
}

// ── search_config_for ────────────────────────────────────────────────────────

/// A directory whose own name is a known script folder is searched directly:
/// `include_dirs = ["."]`, root set to the directory itself.
#[test]
fn search_config_known_folder_searches_directly() {
    let dir = perf_mod().join("common");
    let config = search_config_for(&dir);
    assert_eq!(config.root, dir);
    assert_eq!(config.include_dirs, vec![".".to_string()]);
}

/// A mod root (no top-level script files, name not a known folder) is searched
/// as a workspace: the engine's default subfolder list, not `["."]`.
#[test]
fn search_config_mod_root_uses_default_subfolders() {
    let dir = perf_mod();
    let config = search_config_for(&dir);
    assert_eq!(config.root, dir);
    assert_ne!(config.include_dirs, vec![".".to_string()]);
    assert!(
        config.include_dirs.iter().any(|d| d == "common"),
        "mod-root branch should keep the default subfolder list, got {:?}",
        config.include_dirs
    );
}

/// A directory that itself holds loose script files is searched directly even
/// when its name is not a known folder.
#[test]
fn search_config_loose_script_files_search_directly() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("modroot");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("stuff.txt"), "foo = { x = 1 }\n").unwrap();

    let config = search_config_for(&root);
    assert_eq!(config.include_dirs, vec![".".to_string()]);
}

/// A directory with only subfolders (no top-level script files, non-known name)
/// falls to the workspace branch.
#[test]
fn search_config_subfolders_only_uses_default() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("modroot");
    std::fs::create_dir_all(root.join("sub")).unwrap();

    let config = search_config_for(&root);
    assert_ne!(config.include_dirs, vec![".".to_string()]);
}

// ── workspace_discovery_config / discover_workspace_files (#284) ───────────────

fn ruleset_with_folders(folders: &[&str]) -> cwtools_rules::rules_types::RuleSet {
    let mut rs = cwtools_rules::rules_types::RuleSet::new();
    rs.folders = folders.iter().map(|s| s.to_string()).collect();
    rs
}

#[test]
fn workspace_discovery_config_with_no_ruleset_matches_search_config() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("modroot");
    std::fs::create_dir_all(root.join("common")).unwrap();
    // No ruleset -> same as plain search_config_for.
    let via_none = workspace_discovery_config(&root, None);
    let via_search = search_config_for(&root);
    assert_eq!(via_none.include_dirs, via_search.include_dirs);
    assert_eq!(via_none.root, via_search.root);
}

#[test]
fn workspace_discovery_config_empty_folders_is_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("modroot");
    std::fs::create_dir_all(root.join("common")).unwrap();
    let empty = ruleset_with_folders(&[]);
    let cfg = workspace_discovery_config(&root, Some(&empty));
    let base = search_config_for(&root);
    assert_eq!(cfg.include_dirs, base.include_dirs);
}

#[test]
fn workspace_discovery_narrows_to_folders_when_root_contains_one() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("modroot");
    std::fs::create_dir_all(root.join("common")).unwrap();
    std::fs::create_dir_all(root.join("events")).unwrap();
    let rs = ruleset_with_folders(&["common"]);
    let cfg = workspace_discovery_config(&root, Some(&rs));
    assert_eq!(cfg.include_dirs, vec!["common".to_string()]);
}

#[test]
fn workspace_discovery_keeps_defaults_when_root_lacks_folders() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("modroot");
    // No subdir matching the ruleset folder exists.
    std::fs::create_dir_all(root.join("other")).unwrap();
    let rs = ruleset_with_folders(&["common", "events"]);
    let cfg = workspace_discovery_config(&root, Some(&rs));
    let base = search_config_for(&root);
    assert_eq!(
        cfg.include_dirs, base.include_dirs,
        "apply_config_folders must not fire when no listed folder exists on disk"
    );
}

#[test]
fn workspace_discovery_multi_mod_overrides_even_without_root_folder_check() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(ws.join("mod")).unwrap();
    // Descriptor that makes `ws` classify as MultipleMod.
    std::fs::write(ws.join("mod/test.mod"), "name = \"X\"\npath = \"alpha\"\n").unwrap();
    std::fs::create_dir_all(ws.join("alpha")).unwrap();
    // Note: `ws` itself has no `myfolder/` child — apply_config_folders alone
    // would leave include_dirs unchanged. The multi-mod branch must still
    // force the ruleset folders.
    let rs = ruleset_with_folders(&["myfolder"]);
    let cfg = workspace_discovery_config(&ws, Some(&rs));
    assert_eq!(cfg.include_dirs, vec!["myfolder".to_string()]);
}

#[test]
fn discover_workspace_files_single_mod_respects_include_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("modroot");
    std::fs::create_dir_all(root.join("common")).unwrap();
    std::fs::create_dir_all(root.join("events")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(root.join("common/a.txt"), "x = 1\n").unwrap();
    std::fs::write(root.join("events/b.txt"), "x = 1\n").unwrap();
    std::fs::write(root.join("docs/c.txt"), "x = 1\n").unwrap();
    let rs = ruleset_with_folders(&["common"]);
    let cfg = workspace_discovery_config(&root, Some(&rs));
    let files = discover_workspace_files(cfg).expect("discovery");
    let logical: Vec<String> = files.iter().map(|f| f.logical_path.clone()).collect();
    assert!(
        logical.iter().any(|p| p == "common/a.txt"),
        "common/a.txt must be discovered: {logical:?}"
    );
    assert!(
        !logical.iter().any(|p| p == "events/b.txt"),
        "events/b.txt must be excluded by narrowed include_dirs: {logical:?}"
    );
    assert!(
        !logical.iter().any(|p| p == "docs/c.txt"),
        "docs/c.txt must be excluded: {logical:?}"
    );
}

#[test]
fn discover_workspace_files_extra_ignore_globs_filter_after_narrowing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("modroot");
    std::fs::create_dir_all(root.join("common")).unwrap();
    std::fs::write(root.join("common/keep.txt"), "x = 1\n").unwrap();
    std::fs::write(root.join("common/skip.txt"), "x = 1\n").unwrap();
    let rs = ruleset_with_folders(&["common"]);
    let mut cfg = workspace_discovery_config(&root, Some(&rs));
    cfg.exclude_patterns.push("skip.txt".to_string());
    let files = discover_workspace_files(cfg).expect("discovery");
    let logical: Vec<String> = files.iter().map(|f| f.logical_path.clone()).collect();
    assert!(logical.contains(&"common/keep.txt".to_string()));
    assert!(!logical.contains(&"common/skip.txt".to_string()));
}

#[test]
fn discover_workspace_files_missing_root_is_error() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no_such_mod");
    let rs = ruleset_with_folders(&["common"]);
    let mut cfg = workspace_discovery_config(&missing, Some(&rs));
    cfg.root = missing.clone();
    let err = discover_workspace_files(cfg).expect_err("missing root must error");
    assert!(
        matches!(
            &err,
            FileError::MissingRoot(path) if *path == missing
        ),
        "missing root must be FileError::MissingRoot({}), got: {err:?}",
        missing.display()
    );
    assert!(
        err.to_string().contains(&missing.display().to_string()),
        "error must name the missing root, got: {err}"
    );
}

#[test]
fn discover_workspace_files_empty_include_dirs_returns_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("modroot");
    std::fs::create_dir_all(root.join("common")).unwrap();
    std::fs::write(root.join("common/ignored.txt"), "x = 1\n").unwrap();
    let rs = ruleset_with_folders(&["common"]);
    let mut cfg = workspace_discovery_config(&root, Some(&rs));
    cfg.include_dirs.clear();
    let files = discover_workspace_files(cfg).expect("empty include_dirs should succeed");
    assert!(
        files.is_empty(),
        "empty include_dirs must discover nothing: {files:?}"
    );
}

#[test]
fn discover_workspace_files_multi_mod_layers_and_suppresses_replace_path() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(ws.join("mod")).unwrap();
    // B is higher priority (B > A name-sorted) and declares replace_path.
    std::fs::write(ws.join("mod/a.mod"), "name = \"A Mod\"\npath = \"alpha\"\n").unwrap();
    std::fs::write(
        ws.join("mod/b.mod"),
        "name = \"B Mod\"\npath = \"bravo\"\nreplace_path = \"common\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(ws.join("alpha/common")).unwrap();
    std::fs::create_dir_all(ws.join("bravo/common")).unwrap();
    std::fs::write(ws.join("alpha/common/shared.txt"), "shared = alpha").unwrap();
    std::fs::write(ws.join("bravo/common/shared.txt"), "shared = bravo").unwrap();
    std::fs::write(ws.join("bravo/common/only_bravo.txt"), "x = 1").unwrap();
    let rs = ruleset_with_folders(&["common"]);
    let cfg = workspace_discovery_config(&ws, Some(&rs));
    assert_eq!(cfg.include_dirs, vec!["common".to_string()]);
    let files = discover_workspace_files(cfg).expect("multi-mod discovery");
    let by_path: std::collections::HashMap<String, String> = files
        .iter()
        .map(|f| {
            let content = std::fs::read_to_string(&f.path).unwrap();
            (f.logical_path.clone(), content)
        })
        .collect();
    // B's replace_path must suppress A's lower-priority shared file.
    assert_eq!(
        by_path.get("common/shared.txt").map(|s| s.as_str()),
        Some("shared = bravo"),
        "B Mod (higher priority) with replace_path must win: {by_path:?}"
    );
    assert!(
        by_path.contains_key("common/only_bravo.txt"),
        "non-shared file must survive: {by_path:?}"
    );
    assert_eq!(
        by_path.len(),
        2,
        "exactly two logical files must survive: {by_path:?}"
    );
}

/// `exclude_dir_patterns` (`ignoreDirectories`) must prune an ignored subtree
/// through the shared driver discovery path on a multi-mod workspace, matching
/// single-mod semantics (#412).
#[test]
fn discover_workspace_files_multi_mod_honours_ignore_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(ws.join("mod")).unwrap();
    std::fs::write(
        ws.join("mod/alpha.mod"),
        "name = \"Alpha Mod\"\npath = \"alpha\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(ws.join("alpha/common/temp")).unwrap();
    std::fs::create_dir_all(ws.join("alpha/common/template")).unwrap();
    std::fs::write(ws.join("alpha/common/keep.txt"), "x = 1").unwrap();
    std::fs::write(ws.join("alpha/common/temp/skip.txt"), "x = 1").unwrap();
    std::fs::write(ws.join("alpha/common/template/keep2.txt"), "x = 1").unwrap();
    let rs = ruleset_with_folders(&["common"]);
    let mut cfg = workspace_discovery_config(&ws, Some(&rs));
    cfg.exclude_dir_patterns.push("temp".to_string());
    let files = discover_workspace_files(cfg).expect("multi-mod discovery");
    let logical: Vec<String> = files.iter().map(|f| f.logical_path.clone()).collect();
    assert!(
        logical.contains(&"common/keep.txt".to_string()),
        "keep must survive: {logical:?}"
    );
    assert!(
        logical.contains(&"common/template/keep2.txt".to_string()),
        "template/ must NOT match the exact 'temp' pattern: {logical:?}"
    );
    assert!(
        !logical.iter().any(|p| p.ends_with("temp/skip.txt")),
        "temp/ must be skipped by ignore dirs in multi-mod: {logical:?}"
    );
}

#[test]
fn discover_workspace_files_multi_mod_layers_lower_priority_replace_path_does_not_invalidate_higher_priority_files()
 {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(ws.join("mod")).unwrap();
    // A is lower priority (A < B) but declares replace_path.
    std::fs::write(
        ws.join("mod/a.mod"),
        "name = \"A Mod\"\npath = \"alpha\"\nreplace_path = \"common\"\n",
    )
    .unwrap();
    std::fs::write(ws.join("mod/b.mod"), "name = \"B Mod\"\npath = \"bravo\"\n").unwrap();
    std::fs::create_dir_all(ws.join("alpha/common")).unwrap();
    std::fs::create_dir_all(ws.join("bravo/common")).unwrap();
    std::fs::write(ws.join("alpha/common/shared.txt"), "shared = alpha").unwrap();
    std::fs::write(ws.join("alpha/common/only_alpha.txt"), "x = 1").unwrap();
    std::fs::write(ws.join("bravo/common/shared.txt"), "shared = bravo").unwrap();
    let rs = ruleset_with_folders(&["common"]);
    let cfg = workspace_discovery_config(&ws, Some(&rs));
    let files = discover_workspace_files(cfg).expect("multi-mod discovery");
    let by_path: std::collections::HashMap<String, String> = files
        .iter()
        .map(|f| {
            let content = std::fs::read_to_string(&f.path).unwrap();
            (f.logical_path.clone(), content)
        })
        .collect();
    // A's replace_path is lower priority, so it cannot suppress B's files.
    assert_eq!(
        by_path.get("common/shared.txt").map(|s| s.as_str()),
        Some("shared = bravo"),
        "A is lower priority, so B must survive: {by_path:?}"
    );
    assert!(
        by_path.contains_key("common/only_alpha.txt"),
        "lower-priority lower-priority-only file should survive: {by_path:?}"
    );
    assert_eq!(
        by_path.len(),
        2,
        "only shared and alpha-only should survive: {by_path:?}"
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
fn discover_workspace_files_parity_with_session_discovery() {
    // Session::load must discover the same file set as the standalone
    // discover_workspace_files primitive when given identical inputs (#284).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("modroot");
    std::fs::create_dir_all(root.join("common")).unwrap();
    std::fs::create_dir_all(root.join("events")).unwrap();
    std::fs::write(root.join("common/a.txt"), "my_a = { }\n").unwrap();
    std::fs::write(root.join("events/b.txt"), "my_b = { }\n").unwrap();
    std::fs::create_dir_all(tmp.path().join("rules")).unwrap();
    std::fs::write(
        tmp.path().join("rules/f.cwt"),
        "types = { type[my_a] = { path = \"common\" } type[my_b] = { path = \"events\" } }",
    )
    .unwrap();
    // Session path.
    let session = Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(tmp.path().join("rules")),
        directory: root.clone(),
        vanilla: None,
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    });
    let mut session_paths: Vec<String> = session
        .parsed_files()
        .iter()
        .map(|f| f.logical_path.clone())
        .collect();
    session_paths.sort();
    // Direct primitive path using the session's own ruleset.
    let cfg = workspace_discovery_config(&root, Some(session.ruleset()));
    let direct = discover_workspace_files(cfg).expect("direct discovery");
    let mut direct_paths: Vec<String> = direct.iter().map(|f| f.logical_path.clone()).collect();
    direct_paths.sort();
    assert_eq!(session_paths, direct_paths);
}

#[test]
fn discover_workspace_files_inherits_engine_baseline_excludes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("modroot");
    std::fs::create_dir_all(root.join("common")).unwrap();
    std::fs::write(root.join("common/keep.txt"), "x = 1\n").unwrap();
    std::fs::write(root.join("common/Changelog.txt"), "x = 1\n").unwrap();
    std::fs::write(root.join("common/README.md"), "x\n").unwrap();
    std::fs::write(root.join("common/notes.md"), "x\n").unwrap();
    let rs = ruleset_with_folders(&["common"]);
    let mut cfg = workspace_discovery_config(&root, Some(&rs));
    cfg.exclude_dir_patterns.push("build".to_string());
    std::fs::create_dir_all(root.join("common/build")).unwrap();
    std::fs::write(root.join("common/build/skip.txt"), "x = 1\n").unwrap();
    let files = discover_workspace_files(cfg).expect("discovery");
    let logical: Vec<String> = files.iter().map(|f| f.logical_path.clone()).collect();
    assert!(
        logical.contains(&"common/keep.txt".to_string()),
        "keep must survive: {logical:?}"
    );
    assert!(
        !logical.iter().any(|p| p.ends_with("Changelog.txt")),
        "baseline exclude Changelog.txt: {logical:?}"
    );
    assert!(
        !logical.iter().any(|p| p.ends_with("README.md")),
        "baseline *.md: {logical:?}"
    );
    assert!(
        !logical.iter().any(|p| p.ends_with("notes.md")),
        "baseline *.md: {logical:?}"
    );
    assert!(
        !logical.iter().any(|p| p.contains("build/skip.txt")),
        "dir glob build: {logical:?}"
    );
}

#[test]
fn discover_workspace_files_inherits_engine_default_excluded_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("modroot");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::create_dir_all(root.join("resources")).unwrap();
    std::fs::create_dir_all(root.join("allowed_dir")).unwrap();
    std::fs::create_dir_all(root.join("allowed_dir/sub")).unwrap();
    std::fs::write(root.join("keep.txt"), "x = 1\n").unwrap();
    std::fs::write(root.join("allowed_dir/sub/keep.txt"), "x = 1\n").unwrap();
    std::fs::write(root.join(".git/ignore.txt"), "x = 1\n").unwrap();
    std::fs::write(root.join("target/skip.txt"), "x = 1\n").unwrap();
    std::fs::write(root.join("resources/skip.txt"), "x = 1\n").unwrap();
    let mut cfg = search_config_for(&root);
    cfg.include_dirs = vec![".".to_string()];
    let files = discover_workspace_files(cfg).expect("discovery");
    let logical: Vec<String> = files.iter().map(|f| f.logical_path.clone()).collect();
    assert!(
        logical.contains(&"keep.txt".to_string()),
        "root file should survive: {logical:?}"
    );
    assert!(
        logical
            .iter()
            .any(|p| p.ends_with("allowed_dir/sub/keep.txt")),
        "allowed dir files should survive: {logical:?}"
    );
    assert!(
        !logical.iter().any(|p| p.contains(".git/ignore.txt")),
        ".git folder is excluded by default: {logical:?}"
    );
    assert!(
        !logical.iter().any(|p| p.contains("target/skip.txt")),
        "target folder is excluded by default: {logical:?}"
    );
    assert!(
        !logical.iter().any(|p| p.contains("resources/skip.txt")),
        "resources is excluded at root by default: {logical:?}"
    );
}

#[test]
fn discover_workspace_files_parity_with_session_discovery_multi_mod() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(ws.join("mod")).unwrap();
    std::fs::write(ws.join("mod/a.mod"), "name = \"A Mod\"\npath = \"alpha\"\n").unwrap();
    std::fs::write(ws.join("mod/b.mod"), "name = \"B Mod\"\npath = \"bravo\"\n").unwrap();
    for (mod_name, files) in [
        ("alpha", vec!["common/a.txt", "common/skip.txt"]),
        ("bravo", vec!["common/b.txt", "common/keep.txt"]),
    ] {
        for rel in files {
            let p = ws.join(mod_name).join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "x = 1\n").unwrap();
        }
    }
    std::fs::create_dir_all(tmp.path().join("rules")).unwrap();
    std::fs::write(
        tmp.path().join("rules/f.cwt"),
        "types = { type[my_a] = { path = \"common\" } }",
    )
    .unwrap();
    let ignore_files = vec!["skip.txt".to_string()];
    let session = Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(tmp.path().join("rules")),
        directory: ws.clone(),
        vanilla: None,
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &ignore_files,
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    });
    let session_paths: Vec<String> = session
        .parsed_files()
        .iter()
        .map(|f| f.logical_path.clone())
        .collect();
    let rs = session.ruleset();
    let mut cfg = workspace_discovery_config(&ws, Some(rs));
    cfg.exclude_patterns.extend(ignore_files.clone());
    let direct = discover_workspace_files(cfg).expect("direct multi-mod discovery");
    let direct_paths: Vec<String> = direct.iter().map(|f| f.logical_path.clone()).collect();
    assert_eq!(
        session_paths, direct_paths,
        "Session and direct must agree with ignore_files"
    );
    assert_eq!(
        session_paths,
        vec![
            "common/a.txt".to_string(),
            "common/b.txt".to_string(),
            "common/keep.txt".to_string(),
        ],
        "multi-mod discovery order must be deterministic: {session_paths:?}"
    );
    assert!(!direct_paths.iter().any(|p| p.ends_with("skip.txt")));
}

// ── index_game_dir ───────────────────────────────────────────────────────────

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
        first.map.contains_key("event"),
        "expected `event` instances, got types: {:?}",
        first.map.keys().collect::<Vec<_>>()
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

fn load_perf_session() -> cwtools_driver::SessionWithFiles {
    Session::load(SessionConfig {
        game: Game::Stellaris,
        rules: RulesInput::Dir(perf_rules()),
        directory: perf_mod(),
        vanilla: None,
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    })
}

/// The full load pipeline runs end to end on the fixture: discovery succeeds,
/// a non-empty type index is built, the scope registry is prebuilt, and files
/// are resident for the batch path.
#[test]
fn session_load_builds_indexes() {
    let session = load_perf_session();
    assert!(!session.discovery_failed, "discovery should not fail");
    assert!(!session.parsed_files().is_empty(), "mod files should parse");
    assert!(
        !session.type_index().map.is_empty(),
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

// ── CW100 loc gate ───────────────────────────────────────────────────────────

const LOC_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        localisation = {
            ## required
            name = "$"
        }
    }
}
"#;

/// A temp workspace: `rules/` with a `## required` loc rule, and `mod/` with one
/// `thing` instance plus an optional `localisation/` file.
fn loc_gate_workspace(loc: Option<&str>) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("rules")).unwrap();
    std::fs::write(tmp.path().join("rules").join("things.cwt"), LOC_RULES).unwrap();
    let things = tmp.path().join("mod").join("common").join("things");
    std::fs::create_dir_all(&things).unwrap();
    std::fs::write(things.join("x.txt"), "my_thing = { }\n").unwrap();
    if let Some(text) = loc {
        let loc_dir = tmp.path().join("mod").join("localisation");
        std::fs::create_dir_all(&loc_dir).unwrap();
        std::fs::write(loc_dir.join("l_english.yml"), text).unwrap();
    }
    tmp
}

fn cw100_count(workspace: &std::path::Path) -> usize {
    let session = Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(workspace.join("rules")),
        directory: workspace.join("mod"),
        vanilla: None,
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    });
    session
        .validate_all()
        .iter()
        .flat_map(|(_, errs)| errs.iter())
        .filter(|e| e.code == Some("CW100"))
        .count()
}

/// A mod with no `localisation/` at all must not report CW100 for every object:
/// an empty loc index means "loc not loaded", not "nothing is localised". Same
/// gate the LSP applies in `append_missing_loc_errors`.
#[test]
fn cw100_is_suppressed_when_the_loc_index_is_empty() {
    let tmp = loc_gate_workspace(None);
    assert_eq!(
        cw100_count(tmp.path()),
        0,
        "CW100 must not fire when no loc files were loaded"
    );
}

/// With loc data present the gate is open: an object whose required key is
/// missing from a non-empty loc index still reports CW100.
#[test]
fn cw100_still_fires_when_loc_data_exists() {
    let tmp = loc_gate_workspace(Some("l_english:\n unrelated_key:0 \"text\"\n"));
    assert_eq!(
        cw100_count(tmp.path()),
        1,
        "CW100 must still fire for a missing key once loc data is loaded"
    );
}

// ── CW113 case-sensitive filepath check ─────────────────────────────────────

const FILEPATH_RULES: &str = r#"
spriteType = {
    texturefile = filepath
}
types = {
    type[spriteType] = {
        path = "gfx"
    }
}
"#;

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

/// A mod whose `ref.gfx` references a vanilla file with the wrong case, loaded
/// from a pre-built vanilla cache (not a live install walk).
fn vanilla_cache_workspace() -> (
    tempfile::TempDir,
    cwtools_index::vanilla_cache::VanillaCacheData,
) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("rules")).unwrap();
    std::fs::write(tmp.path().join("rules").join("gfx.cwt"), FILEPATH_RULES).unwrap();
    let gfx_dir = tmp.path().join("mod").join("gfx");
    std::fs::create_dir_all(&gfx_dir).unwrap();
    std::fs::write(
        gfx_dir.join("ref.gfx"),
        "spriteType = { texturefile = \"gfx/vanilla/icon.dds\" }\n",
    )
    .unwrap();
    // The vanilla cache carries the on-disk case `Icon.DDS`; the mod's reference
    // is the lowercased `icon.dds`.
    let cache = cwtools_index::vanilla_cache::VanillaCacheData {
        per_type: std::collections::HashMap::new(),
        aux: cwtools_index::vanilla_cache::VanillaCacheAux {
            file_paths: vec!["gfx/vanilla/Icon.DDS".to_string()],
            ..Default::default()
        },
    };
    (tmp, cache)
}

#[test]
fn session_installs_vanilla_scripted_gui_callbacks_from_cache() {
    let (tmp, mut cache) = vanilla_cache_workspace();
    cache.aux.scripted_gui_names = vec!["Topbar_Icon_Click".into()];
    let session = Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(tmp.path().join("rules")),
        directory: tmp.path().join("mod"),
        vanilla: None,
        vanilla_cache: Some(cache),
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
            .scripted_gui_index
            .contains("topbar_icon_click")
    );
}

fn cw113_count_from_cache(
    workspace: &std::path::Path,
    cache: cwtools_index::vanilla_cache::VanillaCacheData,
    case_sensitive: bool,
) -> usize {
    let session = Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(workspace.join("rules")),
        directory: workspace.join("mod"),
        vanilla: None,
        vanilla_cache: Some(cache),
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
fn cw113_case_mismatch_on_cache_restored_vanilla_flagged_in_case_sensitive_mode() {
    let (tmp, cache) = vanilla_cache_workspace();
    assert_eq!(
        cw113_count_from_cache(tmp.path(), cache, false),
        0,
        "case-insensitive (default) must resolve a cache-restored vanilla file"
    );
    let (tmp2, cache2) = vanilla_cache_workspace();
    assert_eq!(
        cw113_count_from_cache(tmp2.path(), cache2, true),
        1,
        "case-sensitive mode must flag a case-mismatched cache-restored vanilla file"
    );
}

#[test]
fn vanilla_cache_aux_preserves_original_case_file_paths() {
    // The cache must store on-disk case so a later case-sensitive run can enforce
    // it against base-game files too.
    let tmp = tempfile::tempdir().unwrap();
    let asset = tmp.path().join("gfx").join("test");
    std::fs::create_dir_all(&asset).unwrap();
    std::fs::write(asset.join("Icon.DDS"), b"").unwrap();
    let aux = build_vanilla_cache_aux(tmp.path(), &cwtools_index::TypeIndex::default());
    assert!(
        aux.file_paths.contains(&"gfx/test/Icon.DDS".to_string()),
        "cache must store original on-disk case, got: {:?}",
        aux.file_paths
    );
}

// ── CW239 unused-instance pass ───────────────────────────────────────────────

/// Two types: `thing`, whose instances are expected to be referenced, and
/// `user`, which references one. `{SHOULD_BE_USED}` is filled per test so the
/// same mod can be validated with the check armed and disarmed.
const UNUSED_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        {SHOULD_BE_USED}
    }
    type[user] = {
        path = "game/common/users"
    }
}
thing = { }
user = { uses = <thing> }
"#;

const CAPPED_ALIAS_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        should_be_used = yes
    }
    type[user] = {
        path = "game/common/users"
    }
}
thing = { x = scalar }
user = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
## severity = warning
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
alias[effect:needs_int] = int
"#;

/// A temp workspace holding two `thing` instances, only one of which `a_user`
/// references. `armed` controls whether the config asks for the check at all.
fn unused_workspace(armed: bool) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let rules = UNUSED_RULES.replace(
        "{SHOULD_BE_USED}",
        if armed { "should_be_used = yes" } else { "" },
    );
    std::fs::create_dir_all(tmp.path().join("rules")).unwrap();
    std::fs::write(tmp.path().join("rules").join("things.cwt"), rules).unwrap();

    let things = tmp.path().join("mod").join("common").join("things");
    std::fs::create_dir_all(&things).unwrap();
    std::fs::write(things.join("x.txt"), "used_thing = { }\nlone_thing = { }\n").unwrap();

    let users = tmp.path().join("mod").join("common").join("users");
    std::fs::create_dir_all(&users).unwrap();
    std::fs::write(users.join("u.txt"), "a_user = { uses = used_thing }\n").unwrap();
    tmp
}

fn capped_alias_workspace() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let rules = tmp.path().join("rules");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(rules.join("aliases.cwt"), CAPPED_ALIAS_RULES).unwrap();

    let things = tmp.path().join("mod").join("common").join("things");
    std::fs::create_dir_all(&things).unwrap();
    std::fs::write(things.join("x.txt"), "my_thing = { x = a }\n").unwrap();

    let users = tmp.path().join("mod").join("common").join("users");
    std::fs::create_dir_all(&users).unwrap();
    // One two-overload usage past the 65,536-branch budget, every usage its own,
    // so nothing is memoizable and the budget is what stops the file.
    let mut user = String::from("a_user = {\n");
    for _ in 0..32_769 {
        user.push_str("recurse = { }\n");
    }
    user.push_str("}\n");
    std::fs::write(users.join("u.txt"), user).unwrap();
    // A neighbour with one ordinary error. The budget is per file, so the capped
    // file must not take this one's diagnostic down with it.
    std::fs::write(users.join("v.txt"), "b_user = { needs_int = nope }\n").unwrap();
    tmp
}

fn unused_session(workspace: &std::path::Path) -> cwtools_driver::SessionWithFiles {
    Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(workspace.join("rules")),
        directory: workspace.join("mod"),
        vanilla: None,
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    })
}

fn cw239_rows(workspace: &std::path::Path) -> Vec<(PathBuf, String)> {
    unused_session(workspace)
        .validate_all()
        .into_iter()
        .flat_map(|(path, errs)| {
            errs.into_iter()
                .filter(|e| e.code == Some("CW239"))
                .map(move |e| (path.clone(), e.message))
        })
        .collect()
}

/// The batch path's two-phase pass runs end to end: uses recorded across every
/// file, merged, and the definition nothing referenced reported against the
/// file that defines it. A per-file check could not tell these two apart, since
/// the reference lives in a different file from both definitions.
#[test]
fn validate_all_reports_the_unreferenced_instance() {
    let tmp = unused_workspace(true);
    let rows = cw239_rows(tmp.path());
    assert_eq!(
        rows.len(),
        1,
        "exactly one instance is unreferenced: {rows:?}"
    );
    let (path, message) = &rows[0];
    assert!(
        message.contains("lone_thing"),
        "the unreferenced instance should be named: {rows:?}"
    );
    assert!(
        path.ends_with("common/things/x.txt"),
        "CW239 belongs to the file that defines the instance, got {}",
        path.display()
    );
}

#[test]
fn validate_all_reports_capped_alias_without_unused_errors() {
    let tmp = capped_alias_workspace();
    let results = unused_session(tmp.path()).validate_all();
    let capped: Vec<_> = results
        .iter()
        .flat_map(|(path, errors)| {
            errors
                .iter()
                .filter(|error| error.code == Some("CW277"))
                .map(move |error| (path, error))
        })
        .collect();
    assert_eq!(capped.len(), 1, "expected one CW277: {results:?}");
    assert!(
        capped[0].0.ends_with("common/users/u.txt"),
        "the capped file should carry CW277, got {}",
        capped[0].0.display()
    );
    assert!(
        results
            .iter()
            .flat_map(|(_, errors)| errors)
            .all(|error| error.code != Some("CW239")),
        "a capped file must not create false unused-instance errors: {results:?}"
    );
    let neighbour = results
        .iter()
        .find(|(path, _)| path.ends_with("common/users/v.txt"))
        .expect("v.txt should be validated");
    assert_eq!(
        neighbour.1.len(),
        1,
        "the budget is per file; the neighbour keeps its own diagnostic: {:?}",
        neighbour.1
    );
    // Files are validated in parallel. A budget shared across them would move
    // the cap (and everything downstream of it) from run to run.
    assert_eq!(
        results,
        unused_session(tmp.path()).validate_all(),
        "a capped run must be repeatable"
    );
}

/// The same mod under a config that marks no type `should_be_used` reports
/// nothing. This pins the output, not the `needs_use_tracking` short-circuit:
/// that gate only saves work, and `is_tracked` keeps the result empty either
/// way, so forcing the gate on is not observable here.
#[test]
fn validate_all_reports_nothing_without_should_be_used() {
    let tmp = unused_workspace(false);
    assert!(
        cw239_rows(tmp.path()).is_empty(),
        "no type asks to be referenced, so nothing should report"
    );
}

/// `validate_selected` skips the files outside the selection, so a caller
/// reporting on a subset doesn't pay to validate the rest.
#[test]
fn validate_selected_skips_the_unselected_files() {
    let tmp = unused_workspace(false);
    let session = unused_session(tmp.path());
    let only = HashSet::from([tmp
        .path()
        .join("mod")
        .join("common")
        .join("things")
        .join("x.txt")]);

    let paths: Vec<PathBuf> = session
        .validate_selected(Some(&only))
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    assert_eq!(paths.len(), 1, "only the selected file runs: {paths:?}");
    assert!(paths[0].ends_with("common/things/x.txt"));
    assert_eq!(
        session.validate_all().len(),
        2,
        "and the unrestricted pass still covers both"
    );
}

/// The selection is an optimization, never a change of answer. With the
/// cross-file use pass armed, CW239 judges every definition against the
/// references collected from every file — so honouring a selection that leaves
/// out the only file holding a reference would report `used_thing` as unused.
/// `validate_selected` validates the whole set instead, leaving the caller to
/// filter its own report.
#[test]
fn validate_selected_is_ignored_while_use_tracking_is_on() {
    let tmp = unused_workspace(true);
    let session = unused_session(tmp.path());
    // The definitions, without the file that references one of them.
    let only = HashSet::from([tmp
        .path()
        .join("mod")
        .join("common")
        .join("things")
        .join("x.txt")]);

    let results = session.validate_selected(Some(&only));
    let unused: Vec<&str> = results
        .iter()
        .flat_map(|(_, errors)| errors)
        .filter(|e| e.code == Some("CW239"))
        .map(|e| e.message.as_str())
        .collect();
    assert_eq!(
        unused.len(),
        1,
        "only lone_thing is unreferenced; a partial pass would add used_thing: {unused:?}"
    );
    assert!(unused[0].contains("lone_thing"), "got {unused:?}");
    assert_eq!(
        results.len(),
        2,
        "the whole set is validated when the selection can't be honoured"
    );
}

/// The pass is part of `validate_all`'s result, so it must be as repeatable as
/// the rest of it. The per-file uses are merged out of a rayon collect, and a
/// set iteration leaking into the output would show up here.
#[test]
fn validate_all_unused_rows_are_deterministic() {
    let tmp = unused_workspace(true);
    assert_eq!(cw239_rows(tmp.path()), cw239_rows(tmp.path()));
}

fn load_loc_session(
    workspace: &std::path::Path,
    parse_cache_dir: Option<PathBuf>,
) -> cwtools_driver::SessionWithFiles {
    Session::load_with_parse_cache(
        SessionConfig {
            game: Game::Hoi4,
            rules: RulesInput::Dir(workspace.join("rules")),
            directory: workspace.join("mod"),
            vanilla: None,
            vanilla_cache: None,
            vanilla_cache_auto: None,
            ignore_files: &[],
            ignore_dirs: &[],
            loc_languages: None,
            case_sensitive_files: false,
            on_rules_diagnostic: None,
        },
        parse_cache_dir,
    )
}

fn parse_cache_entries(cache_dir: &std::path::Path) -> Vec<(String, std::time::SystemTime)> {
    let mut entries = Vec::new();
    let root = cache_dir.join("parse-cache");
    for workspace in std::fs::read_dir(&root).unwrap().flatten() {
        let dir_name = workspace.file_name().to_string_lossy().into_owned();
        for entry in std::fs::read_dir(workspace.path()).unwrap().flatten() {
            let path = entry.path();
            let Some(ext) = path.extension() else {
                continue;
            };
            if ext != "cwb" && ext != "cwe" {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let mtime = entry.metadata().unwrap().modified().unwrap();
            entries.push((format!("{dir_name}/{name}"), mtime));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

#[test]
fn parse_cache_preserves_cold_and_warm_validation_output() {
    let tmp = loc_gate_workspace(None);
    std::fs::write(
        tmp.path().join("mod/common/things/x.txt"),
        "my_thing = { broken =\n",
    )
    .unwrap();
    let uncached = load_loc_session(tmp.path(), None).validate_all();
    let cache_dir = tmp.path().join("cache");
    let cold = load_loc_session(tmp.path(), Some(cache_dir.clone())).validate_all();
    let cache_entries: usize = std::fs::read_dir(cache_dir.join("parse-cache"))
        .unwrap()
        .flatten()
        .map(|workspace| {
            std::fs::read_dir(workspace.path())
                .unwrap()
                .flatten()
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "cwb"))
                .count()
        })
        .sum();
    let warm = load_loc_session(tmp.path(), Some(cache_dir)).validate_all();

    assert!(cache_entries > 0);
    assert!(uncached.iter().any(|(_, errors)| !errors.is_empty()));
    assert_eq!(uncached, cold);
    assert_eq!(cold, warm);
}

#[test]
fn parse_cache_survives_a_ruleset_only_edit() {
    let tmp = loc_gate_workspace(None);
    let cache_dir = tmp.path().join("cache");
    let cold = load_loc_session(tmp.path(), Some(cache_dir.clone())).validate_all();
    let before = parse_cache_entries(&cache_dir);
    assert!(!before.is_empty());

    std::fs::write(
        tmp.path().join("rules/alias.cwt"),
        "alias[effect:foo] = scalar\n",
    )
    .unwrap();

    let warm = load_loc_session(tmp.path(), Some(cache_dir.clone())).validate_all();
    let after = parse_cache_entries(&cache_dir);
    assert_eq!(
        before, after,
        "a rules-only edit must reuse the existing parse-cache entries"
    );
    assert_eq!(cold, warm);
}

#[test]
fn unusable_parse_cache_falls_back_to_uncached_validation() {
    let tmp = loc_gate_workspace(None);
    let uncached = load_loc_session(tmp.path(), None).validate_all();
    let blocker = tmp.path().join("not-a-cache-directory");
    std::fs::write(&blocker, b"x").unwrap();

    let fallback = load_loc_session(tmp.path(), Some(blocker)).validate_all();
    assert_eq!(uncached, fallback);
}

#[test]
fn changed_source_is_not_validated_against_a_stale_index() {
    let tmp = loc_gate_workspace(None);
    let session = Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(tmp.path().join("rules")),
        directory: tmp.path().join("mod"),
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

// ── auto-managed vanilla cache ───────────────────────────────────────────────

const THING_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
    }
}
"#;

/// Two-type variant of [`THING_RULES`]: same `thing` definition, different
/// ruleset shape, so it must not reuse the one-type cache.
const THING_RULES_V2: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
    }
    type[other] = {
        path = "game/common/others"
    }
}
"#;

/// A temp workspace with `rules/`, a one-instance `mod/`, a one-instance
/// `vanilla/` install, and an empty `cache/` for the auto cache to write into.
fn cache_workspace() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("rules")).unwrap();
    std::fs::write(tmp.path().join("rules").join("things.cwt"), THING_RULES).unwrap();
    for (root, instance) in [("mod", "my_thing"), ("vanilla", "vanilla_thing")] {
        let things = tmp.path().join(root).join("common").join("things");
        std::fs::create_dir_all(&things).unwrap();
        std::fs::write(things.join("x.txt"), format!("{instance} = {{ }}\n")).unwrap();
    }
    std::fs::create_dir_all(tmp.path().join("cache")).unwrap();
    tmp
}

fn load_cached_with_parse_cache(
    workspace: &std::path::Path,
    refresh: bool,
    parse_cache_dir: Option<PathBuf>,
) -> cwtools_driver::SessionWithFiles {
    Session::load_with_parse_cache(
        SessionConfig {
            game: Game::Hoi4,
            rules: RulesInput::Dir(workspace.join("rules")),
            directory: workspace.join("mod"),
            vanilla: Some(workspace.join("vanilla")),
            vanilla_cache: None,
            vanilla_cache_auto: Some(VanillaCacheAuto {
                dir: workspace.join("cache"),
                refresh,
            }),
            ignore_files: &[],
            ignore_dirs: &[],
            loc_languages: None,
            case_sensitive_files: false,
            on_rules_diagnostic: None,
        },
        parse_cache_dir,
    )
}

fn load_cached(workspace: &std::path::Path, refresh: bool) -> cwtools_driver::SessionWithFiles {
    load_cached_with_parse_cache(workspace, refresh, None)
}

fn cache_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "cwv"))
        .collect();
    files.sort();
    files
}

/// Emptying the install's only script file after the first load: a second load
/// that still resolves the base-game instance can only have read it from the
/// cache the first load wrote.
fn blank_the_install(workspace: &std::path::Path) {
    std::fs::write(
        workspace
            .join("vanilla")
            .join("common")
            .join("things")
            .join("x.txt"),
        "",
    )
    .unwrap();
}

#[test]
fn vanilla_cache_auto_writes_then_reuses() {
    let ws = cache_workspace();
    let first = load_cached(ws.path(), false);
    assert!(
        first.type_index().contains("thing", "vanilla_thing"),
        "the install walk should index the base-game instance"
    );
    assert_eq!(
        cache_files(&ws.path().join("cache")).len(),
        1,
        "the first run should write exactly one cache file"
    );

    blank_the_install(ws.path());
    let second = load_cached(ws.path(), false);
    assert!(
        second.type_index().contains("thing", "vanilla_thing"),
        "the second run should read the base-game instance from the cache"
    );
}

#[test]
fn vanilla_cache_auto_refresh_rebuilds_and_overwrites() {
    let ws = cache_workspace();
    let parse_cache_dir = ws.path().join("ast-cache");
    load_cached_with_parse_cache(ws.path(), false, Some(parse_cache_dir.clone()));
    assert!(
        !parse_cache_entries(&parse_cache_dir).is_empty(),
        "the first run should seed the parse cache"
    );
    blank_the_install(ws.path());

    let refreshed = load_cached_with_parse_cache(ws.path(), true, Some(parse_cache_dir.clone()));
    assert!(
        !refreshed.type_index().contains("thing", "vanilla_thing"),
        "--refresh must re-index the install, not read the cache"
    );
    let after = load_cached_with_parse_cache(ws.path(), false, Some(parse_cache_dir));
    assert!(
        !after.type_index().contains("thing", "vanilla_thing"),
        "--refresh must also overwrite the stale cache it skipped"
    );
}

#[test]
fn vanilla_cache_auto_is_keyed_by_ruleset_shape() {
    let ws = cache_workspace();
    load_cached(ws.path(), false);

    // Same install, different rules: the cached instances are extracted by the
    // rules, so the old cache must not be reused for the new ones.
    std::fs::write(ws.path().join("rules").join("things.cwt"), THING_RULES_V2).unwrap();
    let reloaded = load_cached(ws.path(), false);
    assert!(
        reloaded.type_index().contains("thing", "vanilla_thing"),
        "a rules change should re-index, not lose base-game data"
    );
    assert_eq!(
        cache_files(&ws.path().join("cache")).len(),
        2,
        "each ruleset shape gets its own cache file"
    );
}

#[test]
fn vanilla_cache_miss_reuses_parse_cache_after_rules_change() {
    let ws = cache_workspace();
    let parse_cache_dir = ws.path().join("ast-cache");
    let first = load_cached_with_parse_cache(ws.path(), false, Some(parse_cache_dir.clone()));
    assert!(first.type_index().contains("thing", "vanilla_thing"));

    let before = parse_cache_entries(&parse_cache_dir);
    assert_eq!(
        before
            .iter()
            .filter(|(path, _)| path.ends_with(".cwb"))
            .count(),
        2,
        "the mod and vanilla roots should each write one AST entry"
    );

    std::fs::write(ws.path().join("rules").join("things.cwt"), THING_RULES_V2).unwrap();
    let rebuilt = load_cached_with_parse_cache(ws.path(), false, Some(parse_cache_dir.clone()));
    assert!(
        rebuilt.type_index().contains("thing", "vanilla_thing"),
        "the rules-shape cache miss must rebuild the vanilla index"
    );
    assert_eq!(
        before,
        parse_cache_entries(&parse_cache_dir),
        "the rules-only rebuild must reuse vanilla AST entries"
    );
    assert_eq!(
        cache_files(&ws.path().join("cache")).len(),
        2,
        "the rules shape should still produce a new vanilla index cache"
    );
}

#[test]
fn vanilla_parse_cache_reparses_changed_source() {
    let ws = cache_workspace();
    let parse_cache_dir = ws.path().join("ast-cache");
    std::fs::write(
        ws.path()
            .join("vanilla")
            .join("common")
            .join("things")
            .join("y.txt"),
        "other_thing = { }\n",
    )
    .unwrap();
    let first = load_cached_with_parse_cache(ws.path(), false, Some(parse_cache_dir.clone()));
    assert!(first.type_index().contains("thing", "vanilla_thing"));
    assert!(first.type_index().contains("thing", "other_thing"));

    std::fs::write(
        ws.path()
            .join("vanilla")
            .join("common")
            .join("things")
            .join("x.txt"),
        "updated_vanilla_thing = { }\n",
    )
    .unwrap();
    std::fs::write(ws.path().join("rules").join("things.cwt"), THING_RULES_V2).unwrap();

    let rebuilt = load_cached_with_parse_cache(ws.path(), false, Some(parse_cache_dir));
    assert!(
        !rebuilt.type_index().contains("thing", "vanilla_thing"),
        "a changed vanilla file must not use its old AST"
    );
    assert!(
        rebuilt
            .type_index()
            .contains("thing", "updated_vanilla_thing")
    );
    assert!(
        rebuilt.type_index().contains("thing", "other_thing"),
        "an unchanged vanilla file must remain indexed"
    );
    assert_eq!(
        cache_files(&ws.path().join("cache")).len(),
        2,
        "the rules shape should force a vanilla index-cache rebuild"
    );
}

#[test]
fn vanilla_cache_auto_recovers_from_an_unreadable_file() {
    let ws = cache_workspace();
    load_cached(ws.path(), false);
    let cache = cache_files(&ws.path().join("cache")).remove(0);
    std::fs::write(&cache, b"not a cache file").unwrap();

    let session = load_cached(ws.path(), false);
    assert!(
        session.type_index().contains("thing", "vanilla_thing"),
        "an unreadable cache must fall back to indexing the install"
    );
    assert!(
        std::fs::metadata(&cache).unwrap().len() > "not a cache file".len() as u64,
        "the unreadable cache should have been replaced"
    );
}

// ── loc language scoping ─────────────────────────────────────────────────────

/// A temp workspace whose `mod/localisation/` holds one english file, one french
/// file, and one file with an unrecognised language header.
fn loc_language_workspace() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("rules")).unwrap();
    std::fs::write(tmp.path().join("rules").join("things.cwt"), THING_RULES).unwrap();
    let things = tmp.path().join("mod").join("common").join("things");
    std::fs::create_dir_all(&things).unwrap();
    std::fs::write(things.join("x.txt"), "my_thing = { }\n").unwrap();
    let loc = tmp.path().join("mod").join("localisation");
    std::fs::create_dir_all(&loc).unwrap();
    std::fs::write(
        loc.join("a_l_english.yml"),
        "l_english:\n english_key:0 \"e\"\n",
    )
    .unwrap();
    std::fs::write(
        loc.join("b_l_french.yml"),
        "l_french:\n french_key:0 \"f\"\n",
    )
    .unwrap();
    std::fs::write(
        loc.join("c_l_klingon.yml"),
        "l_klingon:\n other_key:0 \"k\"\n",
    )
    .unwrap();
    tmp
}

fn load_scoped(
    workspace: &std::path::Path,
    langs: Option<Vec<Lang>>,
) -> cwtools_driver::SessionWithFiles {
    Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(workspace.join("rules")),
        directory: workspace.join("mod"),
        vanilla: None,
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: langs,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    })
}

#[test]
fn unscoped_loc_load_keeps_every_language() {
    let ws = loc_language_workspace();
    let session = load_scoped(ws.path(), None);
    assert!(session.loc_index().exists_any("english_key"));
    assert!(
        session.loc_index().exists_any("french_key"),
        "the default must keep loading every language"
    );
}

#[test]
fn scoped_loc_load_skips_other_languages() {
    let ws = loc_language_workspace();
    let session = load_scoped(ws.path(), Some(vec![Lang::English]));
    assert!(session.loc_index().exists_any("english_key"));
    assert!(
        !session.loc_index().exists_any("french_key"),
        "a language outside --loc-language should never be parsed"
    );
    assert_eq!(session.loc_index().languages_with_data(), &[Lang::English]);
}

#[test]
fn session_loc_load_filters_the_mod_but_not_vanilla() {
    let ws = loc_language_workspace();
    let mod_loc = ws.path().join("mod/localisation");
    std::fs::write(
        mod_loc.join("skip_mod_l_english.yml"),
        "l_english:\n ignored_mod_key:0 \"m\"\n",
    )
    .unwrap();
    let vanilla = ws.path().join("vanilla");
    let vanilla_loc = vanilla.join("localisation");
    std::fs::create_dir_all(&vanilla_loc).unwrap();
    std::fs::write(
        vanilla_loc.join("skip_vanilla_l_english.yml"),
        "l_english:\n vanilla_key:0 \"v\"\n",
    )
    .unwrap();
    std::fs::write(
        vanilla_loc.join("names.csv"),
        "key;english;french\nvanilla_csv_key;;Nom\n",
    )
    .unwrap();
    let ignore_files = ["skip_*".to_string()];

    let session = Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(ws.path().join("rules")),
        directory: ws.path().join("mod"),
        vanilla: Some(vanilla),
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &ignore_files,
        ignore_dirs: &[],
        loc_languages: Some(vec![Lang::English]),
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    });

    assert!(!session.loc_index().exists_any("ignored_mod_key"));
    assert!(session.loc_index().exists_any("vanilla_key"));
    assert!(session.loc_index().exists_any("vanilla_csv_key"));
}

#[test]
fn scoped_loc_load_still_validates_unrecognised_headers() {
    // A file whose header language can't be read isn't scoped out by the loc
    // validator, so the parse-time filter must not drop it either (CW256).
    let ws = loc_language_workspace();
    let session = load_scoped(ws.path(), Some(vec![Lang::English]));
    let diagnostics = session.loc_project_diagnostics();
    assert!(
        diagnostics
            .iter()
            .any(|d| d.file.ends_with("c_l_klingon.yml")),
        "unrecognised-header files must still be parsed and linted, got {:?}",
        diagnostics.iter().map(|d| &d.file).collect::<Vec<_>>()
    );
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.file.ends_with("b_l_french.yml")),
        "a scoped-out language reports nothing, same as before"
    );
}

/// Discovery order is a contract: the TypeIndex merge order is observable
/// (goto-def first match, duplicate counts), so a silent reorder is a
/// behavioral change. Single-mod walks are sorted within each directory and
/// multi-mod deduplication sorts globally by logical_path (#339).
#[test]
fn discover_workspace_files_returns_sorted_order_single_mod() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("modroot");
    std::fs::create_dir_all(root.join("common")).unwrap();
    for name in ["zebra.txt", "middle.txt", "alpha.txt"] {
        std::fs::write(root.join("common").join(name), "x = 1\n").unwrap();
    }
    let rs = ruleset_with_folders(&["common"]);
    let cfg = workspace_discovery_config(&root, Some(&rs));
    let files = discover_workspace_files(cfg).expect("discovery");
    let logical: Vec<String> = files.iter().map(|f| f.logical_path.clone()).collect();
    assert_eq!(
        logical,
        vec![
            "common/alpha.txt".to_string(),
            "common/middle.txt".to_string(),
            "common/zebra.txt".to_string()
        ],
        "single-mod discovery must be sorted: {logical:?}"
    );
}

#[test]
fn discover_workspace_files_returns_sorted_order_multi_mod() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(ws.join("mod")).unwrap();
    std::fs::write(ws.join("mod/a.mod"), "name = \"A Mod\"\npath = \"alpha\"\n").unwrap();
    std::fs::write(ws.join("mod/b.mod"), "name = \"B Mod\"\npath = \"bravo\"\n").unwrap();
    for p in [
        "events/z.txt",
        "common/z.txt",
        "common/a.txt",
        "events/a.txt",
    ] {
        for m in ["alpha", "bravo"] {
            let path = ws.join(m).join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "x = 1\n").unwrap();
        }
    }
    let rs = ruleset_with_folders(&["events", "common"]);
    let cfg = workspace_discovery_config(&ws, Some(&rs));
    let files = discover_workspace_files(cfg).expect("multi-mod discovery");
    let logical: Vec<String> = files.iter().map(|f| f.logical_path.clone()).collect();
    assert_eq!(
        logical,
        vec![
            "common/a.txt".to_string(),
            "common/z.txt".to_string(),
            "events/a.txt".to_string(),
            "events/z.txt".to_string(),
        ],
        "multi-mod discovery must be globally sorted: {logical:?}"
    );
}

#[test]
fn discover_workspace_files_parity_with_session_preserves_order() {
    // Same inputs must yield the same file *order*, not just the same set
    // (#339). Session::load shares discover_workspace_files, so this pins
    // that no layer re-sorts or shuffles between them.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("modroot");
    std::fs::create_dir_all(root.join("common")).unwrap();
    std::fs::write(root.join("common/b.txt"), "my_b = { }\n").unwrap();
    std::fs::write(root.join("common/a.txt"), "my_a = { }\n").unwrap();
    std::fs::create_dir_all(tmp.path().join("rules")).unwrap();
    std::fs::write(
        tmp.path().join("rules/f.cwt"),
        "types = { type[my_a] = { path = \"common\" } type[my_b] = { path = \"common\" } }",
    )
    .unwrap();
    let session = Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(tmp.path().join("rules")),
        directory: root.clone(),
        vanilla: None,
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    });
    let session_paths: Vec<String> = session
        .parsed_files()
        .iter()
        .map(|f| f.logical_path.clone())
        .collect();
    let cfg = workspace_discovery_config(&root, Some(session.ruleset()));
    let direct = discover_workspace_files(cfg).expect("direct discovery");
    let direct_paths: Vec<String> = direct.iter().map(|f| f.logical_path.clone()).collect();
    assert_eq!(
        session_paths, direct_paths,
        "Session and direct discovery must agree on order without sorting"
    );
    let expected = vec!["common/a.txt".to_string(), "common/b.txt".to_string()];
    assert_eq!(
        session_paths, expected,
        "discovery must be sorted: {session_paths:?}"
    );
    assert_eq!(
        direct_paths, expected,
        "discovery must be sorted: {direct_paths:?}"
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
