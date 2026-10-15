use std::path::PathBuf;

use cwtools_driver::{Session, VanillaCacheAuto, build_vanilla_cache_aux, index_game_dir};
use cwtools_index::variable_defining_effects;
use cwtools_rules::ruleset_loader::load_ruleset_from_dir;
use cwtools_string_table::string_table::StringTable;

use crate::common::{
    FILEPATH_RULES, THING_RULES, parse_cache_entries, workspace_config, write_tree,
};

/// A mod whose `ref.gfx` references a vanilla file with the wrong case, loaded
/// from a pre-built vanilla cache (not a live install walk).
fn vanilla_cache_workspace() -> (
    tempfile::TempDir,
    cwtools_index::vanilla_cache::VanillaCacheData,
) {
    let tmp = write_tree(&[
        ("rules/gfx.cwt", FILEPATH_RULES),
        (
            "mod/gfx/ref.gfx",
            "spriteType = { texturefile = \"gfx/vanilla/icon.dds\" }\n",
        ),
    ]);
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
    let mut config = workspace_config(tmp.path());
    config.vanilla_cache = Some(cache);
    let session = Session::load(config);

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
    let mut config = workspace_config(workspace);
    config.vanilla_cache = Some(cache);
    config.case_sensitive_files = case_sensitive;
    let session = Session::load(config);
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
    let tmp = write_tree(&[("gfx/test/Icon.DDS", "")]);
    let aux = build_vanilla_cache_aux(tmp.path(), &cwtools_index::TypeIndex::default());
    assert!(
        aux.file_paths.contains(&"gfx/test/Icon.DDS".to_string()),
        "cache must store original on-disk case, got: {:?}",
        aux.file_paths
    );
}

// ── auto-managed vanilla cache ───────────────────────────────────────────────

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

const COMMON_ROOT_RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common"
    }
}
"#;

/// A temp workspace with `rules/`, a one-instance `mod/`, a one-instance
/// `vanilla/` install, and an empty `cache/` for the auto cache to write into.
fn cache_workspace() -> tempfile::TempDir {
    let tmp = write_tree(&[
        ("rules/things.cwt", THING_RULES),
        ("mod/common/things/x.txt", "my_thing = { }\n"),
        ("vanilla/common/things/x.txt", "vanilla_thing = { }\n"),
    ]);
    std::fs::create_dir_all(tmp.path().join("cache")).unwrap();
    tmp
}

fn load_cached_with_parse_cache(
    workspace: &std::path::Path,
    refresh: bool,
    parse_cache_dir: Option<PathBuf>,
) -> cwtools_driver::SessionWithFiles {
    load_cached_from_vanilla(
        workspace,
        &workspace.join("vanilla"),
        refresh,
        parse_cache_dir,
    )
}

fn load_cached_from_vanilla(
    workspace: &std::path::Path,
    vanilla_root: &std::path::Path,
    refresh: bool,
    parse_cache_dir: Option<PathBuf>,
) -> cwtools_driver::SessionWithFiles {
    let mut config = workspace_config(workspace);
    config.vanilla = Some(vanilla_root.to_path_buf());
    config.vanilla_cache_auto = Some(VanillaCacheAuto {
        dir: workspace.join("cache"),
        refresh,
    });
    Session::load_with_parse_cache(config, parse_cache_dir)
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
fn vanilla_cache_auto_rebuilds_replacement_view_without_leaking_auxiliary_paths() {
    let ws = cache_workspace();
    std::fs::write(ws.path().join("rules/things.cwt"), COMMON_ROOT_RULES).unwrap();
    let vanilla = ws.path().join("vanilla");
    for (relative, name) in [
        ("common/things_extra/y.txt", "sibling_thing"),
        ("common/outside/z.txt", "outside_thing"),
    ] {
        let path = vanilla.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("{name} = {{ }}\n")).unwrap();
    }
    for relative in [
        "common/things/replaced.dds",
        "common/things/assets/nested_kept.dds",
        "common/things_extra/assets/kept.dds",
    ] {
        let path = vanilla.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }

    let first = load_cached_from_vanilla(ws.path(), &vanilla, false, None);
    assert!(first.type_index().contains("thing", "vanilla_thing"));
    assert!(first.type_index().contains("thing", "sibling_thing"));
    assert!(first.type_index().contains("thing", "outside_thing"));
    assert!(
        first
            .type_index()
            .file_index
            .contains("common/things/replaced.dds")
    );

    std::fs::write(
        ws.path().join("mod/descriptor.mod"),
        r#"name = "Test mod"
replace_path = "COMMON\THINGS"
"#,
    )
    .unwrap();
    let descriptor =
        cwtools_file_manager::file_manager::read_root_descriptor(&ws.path().join("mod"))
            .expect("the primary mod descriptor should parse");
    assert_eq!(descriptor.replace_paths, vec![r"COMMON\THINGS"]);
    let shadow = cwtools_driver::vanilla_replacement_shadow(&ws.path().join("mod"), &[]);
    assert!(shadow.hides_file_under(&vanilla, &vanilla.join("common/things/x.txt")));
    let cached = load_cached_from_vanilla(ws.path(), &vanilla, false, None);
    assert!(
        !cached.type_index().contains("thing", "vanilla_thing"),
        "the cached definition under replace_path must be suppressed"
    );
    assert!(
        cached.type_index().contains("thing", "sibling_thing"),
        "replace_path must not capture a sibling directory with the same prefix"
    );
    assert!(cached.type_index().contains("thing", "outside_thing"));
    assert!(cached.type_index().contains("thing", "my_thing"));
    assert!(
        !cached
            .type_index()
            .file_index
            .contains("common/things/replaced.dds"),
        "the live file index must also hide direct files below replace_path"
    );
    assert!(
        cached
            .type_index()
            .file_index
            .contains("common/things/assets/nested_kept.dds"),
        "replace_path must leave files in nested directories visible"
    );
    assert!(
        cached
            .type_index()
            .file_index
            .contains("common/things_extra/assets/kept.dds")
    );
    assert_eq!(cache_files(&ws.path().join("cache")).len(), 2);

    // Keep the install-root directory metadata stable but remove the source
    // contents: the replacement-view cache must still supply unrelated data.
    std::fs::write(vanilla.join("common/things_extra/y.txt"), "").unwrap();
    std::fs::write(vanilla.join("common/outside/z.txt"), "").unwrap();
    let replacement_cache_hit = load_cached_from_vanilla(ws.path(), &vanilla, false, None);
    assert!(
        replacement_cache_hit
            .type_index()
            .contains("thing", "sibling_thing")
    );
    assert!(
        replacement_cache_hit
            .type_index()
            .contains("thing", "outside_thing")
    );
    assert_eq!(cache_files(&ws.path().join("cache")).len(), 2);

    std::fs::remove_file(ws.path().join("mod/descriptor.mod")).unwrap();
    let unfiltered_cache_hit = load_cached_from_vanilla(ws.path(), &vanilla, false, None);
    assert!(
        unfiltered_cache_hit
            .type_index()
            .contains("thing", "vanilla_thing")
    );
    assert!(
        unfiltered_cache_hit
            .type_index()
            .contains("thing", "sibling_thing")
    );
    assert_eq!(cache_files(&ws.path().join("cache")).len(), 2);
}

#[test]
fn explicit_vanilla_cache_keeps_the_exact_replacement_view_and_unrelated_entries() {
    let ws = cache_workspace();
    std::fs::write(ws.path().join("rules/things.cwt"), COMMON_ROOT_RULES).unwrap();
    let vanilla = ws.path().join("vanilla");
    for (relative, body) in [
        ("common/things_extra/sibling.txt", "sibling_thing = { }\n"),
        ("common/outside/unrelated.txt", "unrelated_thing = { }\n"),
    ] {
        let path = vanilla.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    for relative in ["common/things/replaced.dds", "common/things_extra/kept.dds"] {
        let path = vanilla.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }
    std::fs::write(
        ws.path().join("mod/descriptor.mod"),
        r#"replace_path = "common/things""#,
    )
    .unwrap();

    let generated = load_cached_from_vanilla(ws.path(), &vanilla, false, None);
    assert!(!generated.type_index().contains("thing", "vanilla_thing"));
    let cache_path = cache_files(&ws.path().join("cache"))
        .into_iter()
        .next()
        .expect("replacement-aware cache is written");
    let (_, _, cache) = cwtools_index::vanilla_cache::load(&cache_path).unwrap();

    let mut config = workspace_config(ws.path());
    config.vanilla = Some(vanilla);
    config.vanilla_cache = Some(cache);
    let explicit = Session::load(config);

    assert!(!explicit.type_index().contains("thing", "vanilla_thing"));
    assert!(explicit.type_index().contains("thing", "sibling_thing"));
    assert!(explicit.type_index().contains("thing", "unrelated_thing"));
    assert!(
        !explicit
            .type_index()
            .file_index
            .contains("common/things/replaced.dds")
    );
    assert!(
        explicit
            .type_index()
            .file_index
            .contains("common/things_extra/kept.dds")
    );
}

/// A shadow with no `replace_path`s hides nothing: the filter must not spend
/// any file resolution proving it.
#[test]
fn filter_vanilla_index_passes_everything_through_without_replace_paths() {
    let ws = cache_workspace();
    std::fs::write(ws.path().join("rules/things.cwt"), COMMON_ROOT_RULES).unwrap();
    let vanilla = ws.path().join("vanilla");
    let table = StringTable::new();
    let (ruleset, _) = load_ruleset_from_dir(
        &ws.path().join("rules"),
        &table,
        cwtools_file_manager::file_manager::ScanBudget::default(),
    );
    let var_effects = variable_defining_effects(&ruleset);

    let index = index_game_dir(&vanilla, &ruleset, &table, &var_effects).unwrap();
    let shadow = cwtools_driver::vanilla_replacement_shadow(&ws.path().join("mod"), &[]);
    assert!(!shadow.has_replace_paths());

    let filtered = cwtools_driver::filter_vanilla_index(index.map().clone(), &vanilla, &shadow);
    assert!(
        filtered.get("thing").is_some_and(|instances| instances
            .iter()
            .any(|(_, instance)| instance.name == "vanilla_thing")),
        "every instance must survive a shadow without replace paths"
    );
}

/// Cached sources carry the real install root while the run sees an alias of
/// it: the filter resolves the aliased root and still hides exactly the
/// replaced files.
#[cfg(unix)]
#[test]
fn filter_vanilla_index_matches_cached_sources_through_a_root_alias() {
    use std::os::unix::fs::symlink;

    let ws = cache_workspace();
    std::fs::write(ws.path().join("rules/things.cwt"), COMMON_ROOT_RULES).unwrap();
    let vanilla = ws.path().join("vanilla");
    let nested = vanilla.join("common/things/nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("y.txt"), "nested_kept = { }\n").unwrap();
    let extra = vanilla.join("common/things_extra");
    std::fs::create_dir_all(&extra).unwrap();
    std::fs::write(extra.join("z.txt"), "sibling_kept = { }\n").unwrap();
    std::fs::write(
        ws.path().join("mod/descriptor.mod"),
        r#"replace_path = "common/things""#,
    )
    .unwrap();

    let table = StringTable::new();
    let (ruleset, _) = load_ruleset_from_dir(
        &ws.path().join("rules"),
        &table,
        cwtools_file_manager::file_manager::ScanBudget::default(),
    );
    let var_effects = variable_defining_effects(&ruleset);
    // Index through the real root, the way a cache build would.
    let index = index_game_dir(&vanilla, &ruleset, &table, &var_effects).unwrap();

    let alias = ws.path().join("vanilla-alias");
    symlink(&vanilla, &alias).unwrap();

    let shadow = cwtools_driver::vanilla_replacement_shadow(&ws.path().join("mod"), &[]);
    assert!(shadow.has_replace_paths());

    let filtered = cwtools_driver::filter_vanilla_index(index.into_map(), &alias, &shadow);
    let things: Vec<String> = filtered
        .get("thing")
        .map(|instances| {
            instances
                .iter()
                .map(|(_, instance)| instance.name.clone())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !things.iter().any(|name| name == "vanilla_thing"),
        "the replaced definition must be hidden through the alias"
    );
    assert!(
        things.iter().any(|name| name == "nested_kept"),
        "nested descendants stay visible"
    );
    assert!(
        things.iter().any(|name| name == "sibling_kept"),
        "the replace path must not capture a sibling directory"
    );
}

/// A source spelled under the root as given is matched by prefix, with no
/// file resolution: the file can be gone and the filter still hides it.
#[cfg(unix)]
#[test]
fn filter_vanilla_index_matches_sources_under_the_given_root_without_resolving_them() {
    use std::os::unix::fs::symlink;

    let ws = cache_workspace();
    std::fs::write(ws.path().join("rules/things.cwt"), COMMON_ROOT_RULES).unwrap();
    let vanilla = ws.path().join("vanilla");
    std::fs::write(
        ws.path().join("mod/descriptor.mod"),
        r#"replace_path = "common/things""#,
    )
    .unwrap();
    let alias = ws.path().join("vanilla-alias");
    symlink(&vanilla, &alias).unwrap();

    let table = StringTable::new();
    let (ruleset, _) = load_ruleset_from_dir(
        &ws.path().join("rules"),
        &table,
        cwtools_file_manager::file_manager::ScanBudget::default(),
    );
    let var_effects = variable_defining_effects(&ruleset);
    // Index through the alias, so every source is spelled under it.
    let index = index_game_dir(&alias, &ruleset, &table, &var_effects).unwrap();
    std::fs::remove_file(vanilla.join("common/things/x.txt")).unwrap();

    let shadow = cwtools_driver::vanilla_replacement_shadow(&ws.path().join("mod"), &[]);
    let filtered = cwtools_driver::filter_vanilla_index(index.into_map(), &alias, &shadow);
    assert!(
        !filtered.get("thing").is_some_and(|instances| instances
            .iter()
            .any(|(_, instance)| instance.name == "vanilla_thing")),
        "the replaced definition must be hidden without its file on disk"
    );
}

#[test]
fn vanilla_cache_auto_distinguishes_installs_with_the_same_game_version() {
    let ws = cache_workspace();
    let first_root = ws.path().join("vanilla");
    let second_root = ws.path().join("other-vanilla");
    let second_things = second_root.join("common/things");
    std::fs::create_dir_all(&second_things).unwrap();
    std::fs::write(second_things.join("x.txt"), "second_vanilla_thing = { }\n").unwrap();
    for root in [&first_root, &second_root] {
        std::fs::write(
            root.join("launcher-settings.json"),
            "{\"rawVersion\":\"1.16.4\"}\n",
        )
        .unwrap();
    }

    let first = load_cached_from_vanilla(ws.path(), &first_root, false, None);
    assert!(first.type_index().contains("thing", "vanilla_thing"));
    let second = load_cached_from_vanilla(ws.path(), &second_root, false, None);
    assert!(
        second
            .type_index()
            .contains("thing", "second_vanilla_thing"),
        "a cache entry from the first install must not be reused for a different root"
    );
    assert!(!second.type_index().contains("thing", "vanilla_thing"));
    assert_eq!(
        cache_files(&ws.path().join("cache")).len(),
        2,
        "each vanilla root should have its own cache entry"
    );
}

#[test]
fn vanilla_cache_auto_preserves_real_source_uris() {
    let ws = cache_workspace();
    let vanilla_things = ws.path().join("vanilla").join("common").join("things");
    std::fs::write(vanilla_things.join("y.txt"), "other_vanilla_thing = { }\n").unwrap();

    let source_uris = |session: &cwtools_driver::SessionWithFiles| {
        let mut uris: Vec<(String, String)> = session
            .type_index()
            .instances("thing")
            .iter()
            .filter(|(_, instance)| {
                instance.name == "vanilla_thing" || instance.name == "other_vanilla_thing"
            })
            .map(|(uri, instance)| (instance.name.clone(), uri.to_string()))
            .collect();
        uris.sort();
        uris
    };
    let expected = vec![
        (
            "other_vanilla_thing".to_string(),
            vanilla_things.join("y.txt").to_string_lossy().into_owned(),
        ),
        (
            "vanilla_thing".to_string(),
            vanilla_things.join("x.txt").to_string_lossy().into_owned(),
        ),
    ];

    let live = load_cached(ws.path(), false);
    assert_eq!(source_uris(&live), expected);

    std::fs::write(vanilla_things.join("x.txt"), "").unwrap();
    std::fs::write(vanilla_things.join("y.txt"), "").unwrap();
    let cached = load_cached(ws.path(), false);
    assert_eq!(source_uris(&cached), expected);
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
