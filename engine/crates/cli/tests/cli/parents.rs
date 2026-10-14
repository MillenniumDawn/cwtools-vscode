use crate::common::{cwtools, write_tree};
use predicates::prelude::*;

fn parent_file_reference_workspace() -> tempfile::TempDir {
    let tmp = write_tree(&[
        (
            "rules/gfx.cwt",
            r#"
spriteType = { texturefile = filepath }
types = { type[spriteType] = { path = "gfx" } }
"#,
        ),
        (
            "mod/gfx/ref.gfx",
            "spriteType = { texturefile = \"gfx/parent_icon.dds\" }\n",
        ),
        ("parent/gfx/parent_icon.dds", ""),
    ]);
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
    let tmp = write_tree(&[("cwtools.toml", "parents = [\"\"]\n")]);
    let config = tmp.path().join("cwtools.toml");
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
    let tmp = write_tree(&[
        (
            "parent/descriptor.mod",
            "name = \"Parent\"\nreplace_path = \"localisation\"\n",
        ),
        (
            "mod/common/things/test.txt",
            "thing = { name = hidden_key }\n",
        ),
        (
            "mod/localisation/mod_l_english.yml",
            "l_english:\n visible_key:0 \"Visible\"\n",
        ),
        (
            "vanilla/localisation/hidden_l_english.yml",
            "l_english:\n hidden_key:0 \"Hidden\"\n",
        ),
        ("mod/script.txt", "anyway_valid = { }\n"),
        (
            "rules/things.cwt",
            "types = { type[thing] = { path = \"game/common/things\" } }\nthing = { name = localisation }\n",
        ),
    ]);
    let mod_dir = tmp.path().join("mod");
    let parent_dir = tmp.path().join("parent");
    let rules_dir = tmp.path().join("rules");
    let vanilla_dir = tmp.path().join("vanilla");
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
