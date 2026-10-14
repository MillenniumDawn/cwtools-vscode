use crate::common::cwtools;
use predicates::prelude::*;

fn parent_file_reference_workspace() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let rules = tmp.path().join("rules");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(
        rules.join("gfx.cwt"),
        r#"
spriteType = { texturefile = filepath }
types = { type[spriteType] = { path = "gfx" } }
"#,
    )
    .unwrap();
    let mod_gfx = tmp.path().join("mod/gfx");
    std::fs::create_dir_all(&mod_gfx).unwrap();
    std::fs::write(
        mod_gfx.join("ref.gfx"),
        "spriteType = { texturefile = \"gfx/parent_icon.dds\" }\n",
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join("parent/gfx")).unwrap();
    std::fs::write(tmp.path().join("parent/gfx/parent_icon.dds"), b"").unwrap();
    std::fs::create_dir_all(tmp.path().join("other-parent")).unwrap();
    std::fs::create_dir_all(tmp.path().join("vanilla")).unwrap();
    tmp
}

#[test]
fn validate_parent_flag_loads_a_relative_parent_from_the_working_directory() {
    let tmp = parent_file_reference_workspace();
    cwtools()
        .current_dir(tmp.path())
        .args([
            "validate",
            "--game",
            "hoi4",
            "--directory",
            "mod",
            "--rules",
            "rules",
            "--vanilla",
            "vanilla",
            "--parent",
            "other-parent",
            "--parent",
            "parent",
            "--report-type",
            "cli",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW113").not());
}

#[test]
fn validate_config_loads_relative_parent_paths_from_the_config_directory() {
    let tmp = parent_file_reference_workspace();
    let config = tmp.path().join("cwtools.toml");
    std::fs::write(
        &config,
        "game = \"hoi4\"\ndirectory = \"mod\"\nrules = \"rules\"\nvanilla = \"vanilla\"\nparents = [\"parent\"]\n",
    )
    .unwrap();
    cwtools()
        .args([
            "validate",
            "--config",
            config.to_str().unwrap(),
            "--report-type",
            "cli",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW113").not());
}

#[test]
fn validate_missing_parent_keeps_the_normal_missing_file_diagnostic() {
    let tmp = parent_file_reference_workspace();
    cwtools()
        .current_dir(tmp.path())
        .args([
            "validate",
            "--game",
            "hoi4",
            "--directory",
            "mod",
            "--rules",
            "rules",
            "--vanilla",
            "vanilla",
            "--parent",
            "missing-parent",
            "--report-type",
            "cli",
        ])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("CW113"));
}

#[test]
fn validate_rejects_an_empty_parent_path_in_config() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("cwtools.toml");
    std::fs::write(&config, "parents = [\"\"]\n").unwrap();
    cwtools()
        .args(["validate", "--config", config.to_str().unwrap()])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("parents").and(predicate::str::contains("empty path")));
}

#[test]
fn an_explicit_empty_config_parent_list_clears_parent_resolution() {
    let tmp = parent_file_reference_workspace();
    let config = tmp.path().join("cwtools.toml");
    std::fs::write(
        &config,
        "game = \"hoi4\"\ndirectory = \"mod\"\nrules = \"rules\"\nvanilla = \"vanilla\"\nparents = []\n",
    )
    .unwrap();
    cwtools()
        .args([
            "validate",
            "--config",
            config.to_str().unwrap(),
            "--report-type",
            "cli",
        ])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("CW113"))
        .stderr(predicate::str::contains("does not read config key `parents`").not());
}

#[test]
fn parent_replace_path_is_applied_to_explicit_vanilla_cache_with_or_without_install() {
    let tmp = tempfile::tempdir().unwrap();
    let mod_dir = tmp.path().join("mod");
    let parent_dir = tmp.path().join("parent");
    let rules_dir = tmp.path().join("rules");
    let vanilla_dir = tmp.path().join("vanilla");
    std::fs::create_dir_all(mod_dir.join("common/things")).unwrap();
    std::fs::create_dir_all(mod_dir.join("localisation")).unwrap();
    std::fs::create_dir_all(&parent_dir).unwrap();
    std::fs::create_dir_all(&rules_dir).unwrap();
    std::fs::create_dir_all(vanilla_dir.join("localisation")).unwrap();
    std::fs::write(
        parent_dir.join("descriptor.mod"),
        "name = \"Parent\"\nreplace_path = \"localisation\"\n",
    )
    .unwrap();
    std::fs::write(
        mod_dir.join("common/things/test.txt"),
        "thing = { name = hidden_key }\n",
    )
    .unwrap();
    std::fs::write(
        mod_dir.join("localisation/mod_l_english.yml"),
        "l_english:\n visible_key:0 \"Visible\"\n",
    )
    .unwrap();
    std::fs::write(
        vanilla_dir.join("localisation/hidden_l_english.yml"),
        "l_english:\n hidden_key:0 \"Hidden\"\n",
    )
    .unwrap();
    std::fs::write(mod_dir.join("script.txt"), "anyway_valid = { }\n").unwrap();
    std::fs::write(
        rules_dir.join("things.cwt"),
        "types = { type[thing] = { path = \"game/common/things\" } }\nthing = { name = localisation }\n",
    )
    .unwrap();
    let cache = tmp.path().join("vanilla.cwv");

    cwtools()
        .args([
            "cache-vanilla",
            "--game",
            "hoi4",
            "--vanilla",
            vanilla_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--output",
            cache.to_str().unwrap(),
        ])
        .assert()
        .success();
    let (_, initial_fingerprint, initial) = cwtools_info::vanilla_cache::load(&cache).unwrap();
    assert!(!initial_fingerprint.contains("|rp:"));
    assert!(
        initial
            .aux
            .loc_keys
            .iter()
            .any(|(_, keys)| keys.iter().any(|key| key == "hidden_key"))
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
            "--parent",
            parent_dir.to_str().unwrap(),
            "--vanilla",
            vanilla_dir.to_str().unwrap(),
            "--vanilla-cache",
            cache.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("CW100"));
    let (_, filtered_fingerprint, filtered) = cwtools_info::vanilla_cache::load(&cache).unwrap();
    assert!(filtered_fingerprint.contains("|rp:"));
    assert!(
        !filtered
            .aux
            .loc_keys
            .iter()
            .any(|(_, keys)| keys.iter().any(|key| key == "hidden_key"))
    );

    let config = tmp.path().join("cwtools.toml");
    std::fs::write(&config, "parents = [\"parent\"]\n").unwrap();
    let filtered_bytes = std::fs::read(&cache).unwrap();
    for use_config in [false, true] {
        let mut command = cwtools();
        command.args([
            "fix",
            "--game",
            "hoi4",
            "--directory",
            mod_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--vanilla",
            vanilla_dir.to_str().unwrap(),
            "--vanilla-cache",
            cache.to_str().unwrap(),
        ]);
        if use_config {
            command.args(["--config", config.to_str().unwrap()]);
        } else {
            command.args(["--parent", parent_dir.to_str().unwrap()]);
        }
        command
            .assert()
            .success()
            .stderr(predicate::str::contains("does not read config key `parents`").not())
            .stderr(predicate::str::contains("stale").not());
        assert_eq!(std::fs::read(&cache).unwrap(), filtered_bytes);
    }

    cwtools()
        .args([
            "validate",
            "--game",
            "hoi4",
            "--directory",
            mod_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--parent",
            parent_dir.to_str().unwrap(),
            "--vanilla-cache",
            cache.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("CW100"));
}
