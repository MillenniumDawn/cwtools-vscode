use cwtools_driver::{
    Session, discover_workspace_files, search_config_for, workspace_discovery_config,
};
use cwtools_file_manager::file_manager::FileError;

use crate::common::{perf_mod, workspace_config, write_tree};

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
    let tmp = write_tree(&[("modroot/stuff.txt", "foo = { x = 1 }\n")]);
    let root = tmp.path().join("modroot");

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
    // Descriptor that makes `ws` classify as MultipleMod.
    let tmp = write_tree(&[("workspace/mod/test.mod", "name = \"X\"\npath = \"alpha\"\n")]);
    let ws = tmp.path().join("workspace");
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
    let tmp = write_tree(&[
        ("modroot/common/a.txt", "x = 1\n"),
        ("modroot/events/b.txt", "x = 1\n"),
        ("modroot/docs/c.txt", "x = 1\n"),
    ]);
    let root = tmp.path().join("modroot");
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
    let tmp = write_tree(&[
        ("modroot/common/keep.txt", "x = 1\n"),
        ("modroot/common/skip.txt", "x = 1\n"),
    ]);
    let root = tmp.path().join("modroot");
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
    let tmp = write_tree(&[("modroot/common/ignored.txt", "x = 1\n")]);
    let root = tmp.path().join("modroot");
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
    let tmp = write_tree(&[
        (
            "workspace/mod/a.mod",
            "name = \"A Mod\"\npath = \"alpha\"\n",
        ),
        // B is higher priority (B > A name-sorted) and declares replace_path.
        (
            "workspace/mod/b.mod",
            "name = \"B Mod\"\npath = \"bravo\"\nreplace_path = \"common\"\n",
        ),
        ("workspace/alpha/common/shared.txt", "shared = alpha"),
        ("workspace/bravo/common/shared.txt", "shared = bravo"),
        ("workspace/bravo/common/only_bravo.txt", "x = 1"),
    ]);
    let ws = tmp.path().join("workspace");
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
    let tmp = write_tree(&[
        (
            "workspace/mod/alpha.mod",
            "name = \"Alpha Mod\"\npath = \"alpha\"\n",
        ),
        ("workspace/alpha/common/keep.txt", "x = 1"),
        ("workspace/alpha/common/temp/skip.txt", "x = 1"),
        ("workspace/alpha/common/template/keep2.txt", "x = 1"),
    ]);
    let ws = tmp.path().join("workspace");
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
    let tmp = write_tree(&[
        // A is lower priority (A < B) but declares replace_path.
        (
            "workspace/mod/a.mod",
            "name = \"A Mod\"\npath = \"alpha\"\nreplace_path = \"common\"\n",
        ),
        (
            "workspace/mod/b.mod",
            "name = \"B Mod\"\npath = \"bravo\"\n",
        ),
        ("workspace/alpha/common/shared.txt", "shared = alpha"),
        ("workspace/alpha/common/only_alpha.txt", "x = 1"),
        ("workspace/bravo/common/shared.txt", "shared = bravo"),
    ]);
    let ws = tmp.path().join("workspace");
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
fn discover_workspace_files_parity_with_session_discovery() {
    // Session::load must discover the same file set as the standalone
    // discover_workspace_files primitive when given identical inputs (#284).
    let tmp = write_tree(&[
        ("modroot/common/a.txt", "my_a = { }\n"),
        ("modroot/events/b.txt", "my_b = { }\n"),
        (
            "rules/f.cwt",
            "types = { type[my_a] = { path = \"common\" } type[my_b] = { path = \"events\" } }",
        ),
    ]);
    let root = tmp.path().join("modroot");
    // Session path.
    let mut config = workspace_config(tmp.path());
    config.directory = root.clone();
    let session = Session::load(config);
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
    let tmp = write_tree(&[
        ("modroot/common/keep.txt", "x = 1\n"),
        ("modroot/common/Changelog.txt", "x = 1\n"),
        ("modroot/common/README.md", "x\n"),
        ("modroot/common/notes.md", "x\n"),
        ("modroot/common/build/skip.txt", "x = 1\n"),
    ]);
    let root = tmp.path().join("modroot");
    let rs = ruleset_with_folders(&["common"]);
    let mut cfg = workspace_discovery_config(&root, Some(&rs));
    cfg.exclude_dir_patterns.push("build".to_string());
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
    let tmp = write_tree(&[
        ("modroot/keep.txt", "x = 1\n"),
        ("modroot/allowed_dir/sub/keep.txt", "x = 1\n"),
        ("modroot/.git/ignore.txt", "x = 1\n"),
        ("modroot/target/skip.txt", "x = 1\n"),
        ("modroot/resources/skip.txt", "x = 1\n"),
    ]);
    let root = tmp.path().join("modroot");
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
    let tmp = write_tree(&[
        (
            "workspace/mod/a.mod",
            "name = \"A Mod\"\npath = \"alpha\"\n",
        ),
        (
            "workspace/mod/b.mod",
            "name = \"B Mod\"\npath = \"bravo\"\n",
        ),
        ("workspace/alpha/common/a.txt", "x = 1\n"),
        ("workspace/alpha/common/skip.txt", "x = 1\n"),
        ("workspace/bravo/common/b.txt", "x = 1\n"),
        ("workspace/bravo/common/keep.txt", "x = 1\n"),
        (
            "rules/f.cwt",
            "types = { type[my_a] = { path = \"common\" } }",
        ),
    ]);
    let ws = tmp.path().join("workspace");
    let ignore_files = vec!["skip.txt".to_string()];
    let mut config = workspace_config(tmp.path());
    config.directory = ws.clone();
    config.ignore_files = &ignore_files;
    let session = Session::load(config);
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

#[cfg(unix)]
#[test]
fn discover_workspace_files_rejects_symlinks() {
    use std::os::unix::fs::symlink;

    let tmp = write_tree(&[("modroot/real.txt", "x = 1\n")]);
    let root = tmp.path().join("modroot");
    symlink(root.join("real.txt"), root.join("link.txt")).unwrap();

    let mut cfg = workspace_discovery_config(&root, None);
    cfg.include_dirs = vec![".".to_string()];
    let files = discover_workspace_files(cfg).expect("discovery");
    let logical: Vec<String> = files.iter().map(|f| f.logical_path.clone()).collect();
    assert!(
        logical.contains(&"real.txt".to_string()),
        "got: {logical:?}"
    );
    assert!(
        !logical.contains(&"link.txt".to_string()),
        "file symlink must be rejected: {logical:?}"
    );
}

#[test]
fn discover_workspace_files_enforces_file_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("modroot");
    std::fs::create_dir_all(&root).unwrap();
    for i in 0..10 {
        std::fs::write(root.join(format!("f{i}.txt")), "x = 1\n").unwrap();
    }

    let mut cfg = workspace_discovery_config(&root, None);
    cfg.include_dirs = vec![".".to_string()];
    cfg.scan_budget.max_files = 3;
    let files = discover_workspace_files(cfg).expect("discovery");
    assert_eq!(files.len(), 3, "discovery must stop at the file budget");
}

/// Discovery order is a contract: the TypeIndex merge order is observable
/// (goto-def first match, duplicate counts), so a silent reorder is a
/// behavioral change. Single-mod walks are sorted within each directory and
/// multi-mod deduplication sorts globally by logical_path (#339).
#[test]
fn discover_workspace_files_returns_sorted_order_single_mod() {
    let tmp = write_tree(&[
        ("modroot/common/zebra.txt", "x = 1\n"),
        ("modroot/common/middle.txt", "x = 1\n"),
        ("modroot/common/alpha.txt", "x = 1\n"),
    ]);
    let root = tmp.path().join("modroot");
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
    let tmp = write_tree(&[
        (
            "workspace/mod/a.mod",
            "name = \"A Mod\"\npath = \"alpha\"\n",
        ),
        (
            "workspace/mod/b.mod",
            "name = \"B Mod\"\npath = \"bravo\"\n",
        ),
        ("workspace/alpha/events/z.txt", "x = 1\n"),
        ("workspace/bravo/events/z.txt", "x = 1\n"),
        ("workspace/alpha/common/z.txt", "x = 1\n"),
        ("workspace/bravo/common/z.txt", "x = 1\n"),
        ("workspace/alpha/common/a.txt", "x = 1\n"),
        ("workspace/bravo/common/a.txt", "x = 1\n"),
        ("workspace/alpha/events/a.txt", "x = 1\n"),
        ("workspace/bravo/events/a.txt", "x = 1\n"),
    ]);
    let ws = tmp.path().join("workspace");
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
    let tmp = write_tree(&[
        ("modroot/common/b.txt", "my_b = { }\n"),
        ("modroot/common/a.txt", "my_a = { }\n"),
        (
            "rules/f.cwt",
            "types = { type[my_a] = { path = \"common\" } type[my_b] = { path = \"common\" } }",
        ),
    ]);
    let root = tmp.path().join("modroot");
    let mut config = workspace_config(tmp.path());
    config.directory = root.clone();
    let session = Session::load(config);
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
