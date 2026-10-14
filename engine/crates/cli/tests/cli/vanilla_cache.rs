use crate::common::{cwtools, fix_mod, fixtures_dir};
use predicates::prelude::*;

// ── Vanilla cache ─────────────────────────────────────────────────────────────

#[test]
fn test_cache_vanilla_unknown_game_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let vanilla = fixtures_dir().join("discover").join("mod_a");
    let rules = fixtures_dir().join("rules");
    let output = tmp.path().join("vanilla.cwv");

    cwtools()
        .args([
            "cache-vanilla",
            "--game",
            "not_a_real_game",
            "--vanilla",
            vanilla.to_str().unwrap(),
            "--rules",
            rules.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("Unknown game: not_a_real_game"));
    assert!(!output.exists(), "an unknown game must not write a cache");
}

#[test]
fn test_cache_vanilla_missing_directory_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no_such_vanilla");
    let rules = fixtures_dir().join("rules");
    let output = tmp.path().join("vanilla.cwv");

    cwtools()
        .args([
            "cache-vanilla",
            "--game",
            "stellaris",
            "--vanilla",
            missing.to_str().unwrap(),
            "--rules",
            rules.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains(missing.to_str().unwrap()));
    assert!(
        !output.exists(),
        "a failed cache must not write an output file"
    );
}

#[test]
fn test_cache_vanilla_writes_instances_and_validate_loads_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let vanilla = fixtures_dir().join("discover").join("mod_a");
    let rules = fixtures_dir().join("rules");
    let output = tmp.path().join("vanilla.cwv");

    cwtools()
        .args([
            "cache-vanilla",
            "--game",
            "stellaris",
            "--vanilla",
            vanilla.to_str().unwrap(),
            "--rules",
            rules.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Wrote 1 base-game instances"));
    assert!(
        std::fs::metadata(&output).unwrap().len() > 0,
        "cache-vanilla must write a non-empty cache"
    );

    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            vanilla.to_str().unwrap(),
            "--rules",
            rules.to_str().unwrap(),
            "--vanilla-cache",
            output.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Validation complete"))
        .stderr(predicate::str::contains("Loaded 1 base-game instances"));
}

/// A mod with `replace_path` and an explicit cache that does not carry its
/// replacement view, without `--vanilla`: no exit — the old warn-and-rebuild
/// path degrades to a loud warn + a run without base-game data, so the run
/// still completes and never validates on the unfiltered install.
#[test]
fn test_validate_warns_and_skips_unfilterable_explicit_vanilla_cache_without_vanilla() {
    let tmp = tempfile::tempdir().unwrap();
    let mod_dir = tmp.path().join("mod");
    let mod_common = mod_dir.join("common");
    std::fs::create_dir_all(&mod_common).unwrap();
    std::fs::write(
        mod_dir.join("descriptor.mod"),
        r#"replace_path = "common/ideas""#,
    )
    .unwrap();
    std::fs::write(mod_common.join("test.txt"), "test_thing = { }\n").unwrap();
    let rules_dir = tmp.path().join("rules");
    std::fs::create_dir_all(&rules_dir).unwrap();
    std::fs::write(
        rules_dir.join("things.cwt"),
        r#"
types = {
    type[thing] = {
        path = "game/common/things"
    }
}
"#,
    )
    .unwrap();
    let cache = tmp.path().join("vanilla.cwv");
    let vanilla = tmp.path().join("vanilla");
    std::fs::create_dir_all(&vanilla).unwrap();
    cwtools_info::vanilla_cache::save_per_type(
        &std::collections::HashMap::new(),
        "hoi4",
        "test-fingerprint",
        &cache,
        Default::default(),
    )
    .unwrap();

    cwtools()
        .args([
            "validate",
            "--game",
            "hoi4",
            "--directory",
            mod_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--vanilla-cache",
            cache.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Validation complete"))
        .stderr(predicate::str::contains(
            "does not carry this mod's replace_path view",
        ))
        .stderr(predicate::str::contains(
            "no base-game data loaded, so CW113, CW222, CW500",
        ));
}

/// An unfiltered `cache-vanilla` cache paired with `--vanilla` on a mod with
/// `replace_path`: the warn-and-rebuild path rebuilds the filtered view under
/// the replacement-aware fingerprint instead of exiting.
#[test]
fn test_validate_warns_and_rebuilds_replace_path_cache_with_vanilla() {
    let tmp = tempfile::tempdir().unwrap();
    let mod_dir = tmp.path().join("mod");
    std::fs::create_dir_all(&mod_dir).unwrap();
    std::fs::write(
        mod_dir.join("descriptor.mod"),
        r#"replace_path = "common/things""#,
    )
    .unwrap();
    std::fs::write(mod_dir.join("script.txt"), "anyway_valid = { }\n").unwrap();
    let rules_dir = tmp.path().join("rules");
    std::fs::create_dir_all(&rules_dir).unwrap();
    std::fs::write(
        rules_dir.join("things.cwt"),
        r#"
types = {
    type[thing] = {
        path = "game/common/things"
    }
}
"#,
    )
    .unwrap();
    let vanilla = tmp.path().join("vanilla");
    let replaced = vanilla.join("common/things");
    std::fs::create_dir_all(&replaced).unwrap();
    std::fs::write(replaced.join("x.txt"), "vanilla_thing = { }\n").unwrap();
    let nested = replaced.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("y.txt"), "nested_kept = { }\n").unwrap();
    let cache = tmp.path().join("vanilla.cwv");

    // Build the cache the documented way: the unfiltered install view.
    cwtools()
        .args([
            "cache-vanilla",
            "--game",
            "hoi4",
            "--vanilla",
            vanilla.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--output",
            cache.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Wrote 2 base-game instances"));
    let (_, cache_fp, cache_data) = cwtools_info::vanilla_cache::load(&cache).unwrap();
    assert!(
        cache_data.per_type["thing"]
            .iter()
            .any(|(_, i)| i.name == "vanilla_thing"),
        "cache-vanilla writes the unfiltered install"
    );
    assert!(
        !cache_fp.contains("|rp:"),
        "the documented fingerprint is the unfiltered one"
    );

    cwtools()
        .args([
            "validate",
            "--game",
            "hoi4",
            "--directory",
            mod_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--vanilla",
            vanilla.to_str().unwrap(),
            "--vanilla-cache",
            cache.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Validation complete"))
        .stderr(predicate::str::contains(
            "does not carry this mod's replace_path view",
        ))
        .stderr(predicate::str::contains(
            "using a live base-game index for this run",
        ))
        .stderr(predicate::str::contains("vanilla cache is stale (cached:"))
        .stderr(predicate::str::contains(
            "Rebuilt vanilla cache with 1 instances",
        ));

    // The rebuilt file carries the filtered view under the replacement-aware
    // fingerprint: not an unfiltered index labelled as one.
    let (_, rebuilt_fp, rebuilt) = cwtools_info::vanilla_cache::load(&cache).unwrap();
    assert!(
        rebuilt_fp.contains("|rp:"),
        "the rebuilt cache is replacement-scoped"
    );
    assert!(
        !rebuilt.per_type["thing"]
            .iter()
            .any(|(_, i)| i.name == "vanilla_thing"),
        "the replaced definition must not survive the rebuild"
    );
    assert!(
        rebuilt
            .per_type
            .get("thing")
            .is_some_and(|instances| instances.iter().any(|(_, i)| i.name == "nested_kept")),
        "files in nested directories stay visible to the rebuilt view"
    );
    assert!(
        !rebuilt
            .aux
            .file_paths
            .iter()
            .any(|path| path == "common/things/x.txt"),
        "the rebuilt auxiliary index hides the replaced file"
    );
    assert!(
        rebuilt
            .aux
            .file_paths
            .iter()
            .any(|path| path == "common/things/nested/y.txt"),
        "the rebuilt auxiliary index keeps nested files"
    );
}

/// The FIRST `--vanilla --vanilla-cache` run on a replace_path mod must
/// already refuse a loc key hidden by the replacement view, not only heal the
/// cache file for the next run.
#[test]
fn test_validate_first_run_cannot_resolve_loc_key_hidden_by_replace_path() {
    let tmp = tempfile::tempdir().unwrap();
    let mod_dir = tmp.path().join("mod");
    std::fs::create_dir_all(&mod_dir).unwrap();
    std::fs::write(
        mod_dir.join("descriptor.mod"),
        r#"replace_path = "localisation/replaced""#,
    )
    .unwrap();
    std::fs::write(mod_dir.join("script.txt"), "anyway_valid = { }\n").unwrap();
    let mod_loc = mod_dir.join("localisation");
    std::fs::create_dir_all(&mod_loc).unwrap();
    std::fs::write(
        mod_loc.join("mod_l_english.yml"),
        "l_english:\n mod_key:0 \"$hidden_key$\"\n ref_kept:0 \"$kept_key$\"\n",
    )
    .unwrap();
    let rules_dir = tmp.path().join("rules");
    std::fs::create_dir_all(&rules_dir).unwrap();
    std::fs::write(
        rules_dir.join("things.cwt"),
        r#"
types = {
    type[thing] = {
        path = "game/common/things"
    }
}
"#,
    )
    .unwrap();
    let vanilla = tmp.path().join("vanilla");
    let replaced_loc = vanilla.join("localisation/replaced");
    std::fs::create_dir_all(&replaced_loc).unwrap();
    std::fs::write(
        replaced_loc.join("hidden_l_english.yml"),
        "l_english:\n hidden_key:0 \"Hidden\"\n",
    )
    .unwrap();
    let vanilla_loc = vanilla.join("localisation");
    std::fs::write(
        vanilla_loc.join("kept_l_english.yml"),
        "l_english:\n kept_key:0 \"Kept\"\n",
    )
    .unwrap();
    let cache = tmp.path().join("vanilla.cwv");

    // The cache built the documented way carries the unfiltered loc keys.
    cwtools()
        .args([
            "cache-vanilla",
            "--game",
            "hoi4",
            "--vanilla",
            vanilla.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--output",
            cache.to_str().unwrap(),
        ])
        .assert()
        .success();
    let (_, cache_fp, cache_data) = cwtools_info::vanilla_cache::load(&cache).unwrap();
    assert!(
        cache_data
            .aux
            .loc_keys
            .iter()
            .any(|(_, keys)| keys.iter().any(|key| key == "hidden_key")),
        "the documented cache holds the key that the game no longer loads"
    );
    assert!(!cache_fp.contains("|rp:"));

    cwtools()
        .args([
            "validate",
            "--game",
            "hoi4",
            "--directory",
            mod_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--vanilla",
            vanilla.to_str().unwrap(),
            "--vanilla-cache",
            cache.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("Validation complete"))
        .stdout(predicate::str::contains(
            "Localisation key \"mod_key\" references \"hidden_key\"",
        ))
        .stdout(predicate::str::contains("CW225"))
        .stdout(predicate::str::contains("references \"kept_key\"").not());

    // The same first run rewrites the explicit file as the filtered view:
    // the hidden key leaves the auxiliary loc index too.
    let (_, rebuilt_fp, rebuilt) = cwtools_info::vanilla_cache::load(&cache).unwrap();
    assert!(rebuilt_fp.contains("|rp:"));
    assert!(
        !rebuilt
            .aux
            .loc_keys
            .iter()
            .any(|(_, keys)| keys.iter().any(|key| key == "hidden_key")),
        "the rebuilt cache must not carry the replaced loc key"
    );
    assert!(
        rebuilt
            .aux
            .loc_keys
            .iter()
            .any(|(_, keys)| keys.iter().any(|key| key == "kept_key")),
        "the rebuilt cache keeps the visible loc key"
    );
}

#[test]
fn test_validate_warns_when_vanilla_cache_is_for_another_game() {
    let tmp = tempfile::tempdir().unwrap();
    let vanilla = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    let cache = tmp.path().join("vanilla.cwv");

    cwtools()
        .args([
            "cache-vanilla",
            "--game",
            "stellaris",
            "--vanilla",
            vanilla.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--output",
            cache.to_str().unwrap(),
        ])
        .assert()
        .success();

    cwtools()
        .args([
            "validate",
            "--game",
            "hoi4",
            "--directory",
            vanilla.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--vanilla-cache",
            cache.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "vanilla cache was built for game 'stellaris', validating 'hoi4'",
        ));
}

#[test]
fn test_validate_does_not_warn_for_equivalent_vanilla_cache_game_aliases() {
    let tmp = tempfile::tempdir().unwrap();
    let vanilla = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");

    for (cache_game, validate_game) in [("stl", "stellaris"), ("imperator", "ir")] {
        let cache = tmp.path().join(format!("{cache_game}.cwv"));
        cwtools()
            .args([
                "cache-vanilla",
                "--game",
                cache_game,
                "--vanilla",
                vanilla.to_str().unwrap(),
                "--rules",
                rules_dir.to_str().unwrap(),
                "--output",
                cache.to_str().unwrap(),
            ])
            .assert()
            .success();

        cwtools()
            .args([
                "validate",
                "--game",
                validate_game,
                "--directory",
                vanilla.to_str().unwrap(),
                "--rules",
                rules_dir.to_str().unwrap(),
                "--vanilla-cache",
                cache.to_str().unwrap(),
            ])
            .assert()
            .success()
            .stderr(predicate::str::contains("vanilla cache was built").not());
    }
}

#[test]
fn test_validate_warns_when_vanilla_cache_has_unknown_game_identifier() {
    let tmp = tempfile::tempdir().unwrap();
    let vanilla = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    let cache = tmp.path().join("unknown.cwv");
    let empty_index = std::collections::HashMap::new();
    cwtools_info::vanilla_cache::save_per_type(
        &empty_index,
        "not_a_real_game",
        "test-fingerprint",
        &cache,
        Default::default(),
    )
    .unwrap();

    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            vanilla.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--vanilla-cache",
            cache.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "vanilla cache was built for game 'not_a_real_game', validating 'stellaris'",
        ));
}

#[test]
fn test_fix_warns_when_vanilla_cache_is_for_another_game() {
    let tmp = tempfile::tempdir().unwrap();
    let vanilla = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    let cache = tmp.path().join("vanilla.cwv");

    cwtools()
        .args([
            "cache-vanilla",
            "--game",
            "stellaris",
            "--vanilla",
            vanilla.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--output",
            cache.to_str().unwrap(),
        ])
        .assert()
        .success();

    let mod_dir = fix_mod();
    cwtools()
        .args([
            "fix",
            "--game",
            "hoi4",
            "--directory",
            mod_dir.path().to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--vanilla-cache",
            cache.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "vanilla cache was built for game 'stellaris', fixing 'hoi4'",
        ));
}

#[test]
fn test_fix_does_not_warn_for_equivalent_vanilla_cache_game_aliases() {
    let tmp = tempfile::tempdir().unwrap();
    let vanilla = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    let mod_dir = fix_mod();

    for (cache_game, fix_game) in [("stl", "stellaris"), ("imperator", "ir")] {
        let cache = tmp.path().join(format!("{cache_game}.cwv"));
        cwtools()
            .args([
                "cache-vanilla",
                "--game",
                cache_game,
                "--vanilla",
                vanilla.to_str().unwrap(),
                "--rules",
                rules_dir.to_str().unwrap(),
                "--output",
                cache.to_str().unwrap(),
            ])
            .assert()
            .success();

        cwtools()
            .args([
                "fix",
                "--game",
                fix_game,
                "--directory",
                mod_dir.path().to_str().unwrap(),
                "--rules",
                rules_dir.to_str().unwrap(),
                "--vanilla-cache",
                cache.to_str().unwrap(),
            ])
            .assert()
            .success()
            .stderr(predicate::str::contains("vanilla cache was built").not());
    }
}
