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

fn launcher_parent_workspace() -> (tempfile::TempDir, tempfile::TempDir) {
    let tmp = parent_file_reference_workspace();
    std::fs::write(
        tmp.path().join("mod/descriptor.mod"),
        "name = Submod\ndependencies = { \"Parent\" }\n",
    )
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let documents = if cfg!(target_os = "linux") {
        ".local/share"
    } else {
        "Documents"
    };
    let launcher = home
        .path()
        .join(documents)
        .join("Paradox Interactive/Hearts of Iron IV/mod");
    std::fs::create_dir_all(&launcher).unwrap();
    std::fs::write(
        launcher.join("parent.mod"),
        format!(
            "name = \"Parent\"\npath = \"{}\"\n",
            tmp.path().join("parent").display()
        ),
    )
    .unwrap();
    (tmp, home)
}

#[test]
fn validate_discovers_descriptor_parent_from_launcher_without_parent_option() {
    let (tmp, home) = launcher_parent_workspace();
    cwtools()
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
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
            "--report-type",
            "cli",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW113").not());
}

#[test]
fn validate_parent_flag_overrides_discovered_descriptor_dependencies() {
    let (tmp, home) = launcher_parent_workspace();
    cwtools()
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
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
        ])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("CW113"));
}

#[test]
fn validate_config_parents_override_discovered_descriptor_dependencies() {
    let (tmp, home) = launcher_parent_workspace();
    let config = tmp.path().join("cwtools.toml");
    std::fs::write(
        &config,
        "game = \"hoi4\"\ndirectory = \"mod\"\nrules = \"rules\"\nvanilla = \"vanilla\"\nparents = [\"other-parent\"]\n",
    )
    .unwrap();
    cwtools()
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .args(["validate", "--config", config.to_str().unwrap()])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("CW113"));
}

#[test]
fn fix_does_not_edit_the_external_parent_directory() {
    let (tmp, home) = launcher_parent_workspace();
    let parent_file = tmp.path().join("parent/common/hint.txt");
    std::fs::create_dir_all(parent_file.parent().unwrap()).unwrap();
    let original = "x = { if = { } }\n";
    std::fs::write(&parent_file, original).unwrap();
    cwtools()
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(tmp.path())
        .args([
            "fix",
            "--game",
            "hoi4",
            "--directory",
            "mod",
            "--rules",
            "rules",
            "--vanilla",
            "vanilla",
            "--apply",
        ])
        .assert()
        .success();
    assert_eq!(std::fs::read_to_string(parent_file).unwrap(), original);
}

#[cfg(unix)]
#[cfg_attr(target_os = "macos", ignore = "macOS rejects non-UTF-8 filenames")]
#[test]
fn validate_parent_flag_preserves_a_non_utf8_parent_path() {
    use std::os::unix::ffi::OsStrExt;

    let tmp = parent_file_reference_workspace();
    let parent = tmp.path().join(std::ffi::OsStr::from_bytes(b"parent-\xff"));
    std::fs::create_dir_all(parent.join("gfx")).unwrap();
    std::fs::write(parent.join("gfx/parent_icon.dds"), "").unwrap();
    let mut command = cwtools();
    command
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
        ])
        .arg("--parent")
        .arg(parent.as_os_str())
        .args(["--report-type", "cli"]);
    command
        .assert()
        .success()
        .stdout(predicate::str::contains("CW113").not());
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
fn an_empty_config_parent_list_still_discovers_descriptor_parents() {
    let (tmp, home) = launcher_parent_workspace();
    let config = tmp.path().join("cwtools.toml");
    std::fs::write(
        &config,
        "game = \"hoi4\"\ndirectory = \"mod\"\nrules = \"rules\"\nvanilla = \"vanilla\"\nparents = []\n",
    )
    .unwrap();
    cwtools()
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .args([
            "validate",
            "--config",
            config.to_str().unwrap(),
            "--report-type",
            "cli",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("CW113").not())
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
