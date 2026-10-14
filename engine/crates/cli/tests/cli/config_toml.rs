use crate::common::{config_body, config_mod, cwtools, fixtures_dir};
use predicates::prelude::*;

// ── Config file (cwtools.toml) ───────────────────────────────────────────────
//
// Discovery walks up from --directory (or the CWD), so every test here anchors
// on a tempdir: the tree above it holds no cwtools.toml.

#[test]
fn test_validate_ignore_file_excludes_localisation_diagnostics() {
    let tmp = config_mod(&config_body());
    let loc = tmp.path().join("mod/localisation");
    std::fs::create_dir_all(&loc).unwrap();
    std::fs::write(
        loc.join("ignored_l_english.yml"),
        "\u{feff}l_english:\n broken:0 \"unterminated\n",
    )
    .unwrap();

    cwtools()
        .arg("validate")
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("CW268"));
    cwtools()
        .args(["validate", "--ignore-file", "ignored*"])
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("CW268").not());
}

#[test]
fn test_config_supplies_game_directory_and_rules() {
    let tmp = config_mod(&config_body());
    cwtools()
        .arg("validate")
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Validation complete"))
        .stdout(predicate::str::contains("CW107"))
        .stderr(predicate::str::contains("Using config"))
        .stderr(predicate::str::contains("applied: game, directory, rules"));
}

#[test]
fn test_config_announce_lists_all_applied_keys_in_order() {
    let body = format!(
        "{}no-vanilla-cache = true\n\
refresh-vanilla-cache = true\n\
report-type = \"csv\"\n\
min-severity = \"info\"\n\
fail-on = \"none\"\n\
ignore-files = [\"never-match\"]\n\
ignore-dirs = [\"never-match\"]\n\
loc-languages = [\"english\"]\n\
case-sensitive-files = false\n\
ignore-codes = [\"CW107\"]\n\
only-codes = [\"CW107\"]\n\
allow-empty = true\n",
        config_body()
    );
    let tmp = config_mod(&body);
    let output = cwtools()
        .arg("validate")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // macOS's /var is a symlink to /private/var, and the child resolves the
    // config from its canonical current directory. Keep the exact announcement
    // assertion while matching that platform's path spelling; Windows keeps
    // the lexical path because `canonicalize` adds an extended-length prefix.
    #[cfg(unix)]
    let config_path = std::fs::canonicalize(tmp.path().join("cwtools.toml")).unwrap();
    #[cfg(not(unix))]
    let config_path = tmp.path().join("cwtools.toml");
    let expected = format!(
        "Using config {} (applied: game, directory, rules, no-vanilla-cache, \
         refresh-vanilla-cache, case-sensitive-files, report-type, min-severity, \
         fail-on, ignore-files, ignore-dirs, loc-languages, ignore-codes, \
         only-codes, allow-empty)",
        config_path.display()
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr
            .lines()
            .find(|line| line.starts_with("Using config ")),
        Some(expected.as_str())
    );
}

#[test]
fn test_config_is_discovered_by_walking_up_from_the_directory() {
    let tmp = config_mod(&config_body());
    // Run from a directory well below the config, with no --config.
    let deep = tmp.path().join("mod").join("events");
    cwtools()
        .arg("validate")
        .current_dir(&deep)
        .assert()
        .success()
        .stderr(predicate::str::contains("Using config"));
}

/// A CI job runs from wherever the runner puts it; a relative `rules =` must
/// still resolve.
#[test]
fn test_config_relative_paths_resolve_against_the_config_file() {
    let tmp = config_mod("game = \"stellaris\"\ndirectory = \"mod\"\nrules = \"rules\"\n");
    // Copy the rules fixture next to the config so `rules = "rules"` resolves.
    let rules = tmp.path().join("rules");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::copy(
        fixtures_dir().join("rules").join("test.cwt"),
        rules.join("test.cwt"),
    )
    .unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    cwtools()
        .args(["validate", "--config"])
        .arg(tmp.path().join("cwtools.toml"))
        .current_dir(elsewhere.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Validation complete"));
}

#[test]
fn test_flags_override_config_values() {
    // The config names a game the fixture rules aren't for; the flag wins, and
    // the overridden key is not listed as applied.
    let tmp = config_mod(&config_body().replace("stellaris", "eu4"));
    cwtools()
        .args(["validate", "--game", "stellaris"])
        .current_dir(tmp.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("Validating stellaris files"))
        .stderr(predicate::str::contains("applied: directory, rules"));
}

#[test]
fn test_explicit_config_overrides_discovery() {
    let tmp = config_mod("game = \"eu4\"\n");
    let chosen = tmp.path().join("ci.toml");
    std::fs::write(&chosen, config_body()).unwrap();
    cwtools()
        .args(["validate", "--config"])
        .arg(&chosen)
        .current_dir(tmp.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("ci.toml"))
        .stderr(predicate::str::contains("Validating stellaris files"));
}

#[test]
fn test_config_covers_the_cache_flags() {
    let tmp = config_mod(&format!("{}no-vanilla-cache = true\n", config_body()));
    cwtools()
        .arg("validate")
        .current_dir(tmp.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("no-vanilla-cache"));
}

#[test]
fn test_config_supplies_the_code_filters() {
    let tmp = config_mod(&format!("{}ignore-codes = [\"CW107\"]\n", config_body()));
    cwtools()
        .arg("validate")
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("CW107").not())
        .stderr(predicate::str::contains("ignore-codes"));
}

#[test]
fn test_malformed_config_fails_loudly_naming_the_file_and_line() {
    let tmp = config_mod("game = \"stellaris\"\ngmae = \"stellaris\"\n");
    cwtools()
        .arg("validate")
        .current_dir(tmp.path())
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("cwtools.toml:2"))
        .stderr(predicate::str::contains("unknown key `gmae`"))
        // Never a silent fallback to defaults.
        .stdout(predicate::str::contains("Validation complete").not());
}

#[test]
fn test_config_with_a_bad_value_fails() {
    let tmp = config_mod("game = \"hoi5\"\n");
    cwtools()
        .arg("validate")
        .current_dir(tmp.path())
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("unknown game `hoi5`"));
}

#[test]
fn test_missing_explicit_config_fails() {
    let tmp = tempfile::tempdir().unwrap();
    cwtools()
        .args(["validate", "--config"])
        .arg(tmp.path().join("nope.toml"))
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("no such config file"));
}

#[test]
fn test_missing_required_setting_is_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    cwtools()
        .args(["validate", "--directory"])
        .arg(tmp.path())
        .args(["--rules", fixtures_dir().join("rules").to_str().unwrap()])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("--game <GAME>"))
        .stderr(predicate::str::contains("cwtools.toml"));
}

#[test]
fn test_loc_reads_the_config_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let loc = tmp.path().join("mod").join("localisation");
    std::fs::create_dir_all(&loc).unwrap();
    std::fs::copy(
        fixtures_dir().join("loc_error/localisation/l_english.yml"),
        loc.join("l_english.yml"),
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("cwtools.toml"),
        "directory = \"mod\"\nignore-codes = [\"CW225\"]\n",
    )
    .unwrap();
    // CW225 is the only (Error-severity) diagnostic, so suppressing it also
    // flips the exit code.
    cwtools()
        .arg("loc")
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("CW225").not());
}

/// Diagnostic hashes are relative to the mod root, so `directory = "."` in a
/// config must resolve to the same root — and therefore the same baseline —
/// as passing the directory on the command line. The config lives inside the
/// mod dir itself (as a real mod's `cwtools.toml` would, next to `common/`,
/// `events/`, etc.), not `config_mod`'s layout one level up: `directory = "."`
/// there names the mod's *parent*, a different root that happens to contain
/// the same one file, which isn't the scenario this test is about.
#[test]
fn test_config_directory_dot_keeps_baseline_hashes_stable() {
    let tmp = tempfile::tempdir().unwrap();
    let mod_dir = tmp.path().join("mod");
    let events = mod_dir.join("events");
    std::fs::create_dir_all(&events).unwrap();
    std::fs::copy(
        fixtures_dir().join("discover/mod_a/events/test.txt"),
        events.join("test.txt"),
    )
    .unwrap();
    std::fs::write(mod_dir.join("cwtools.toml"), "directory = \".\"\n").unwrap();
    let rules_dir = fixtures_dir().join("rules");
    let hashes = |args: &[&str], cwd: &std::path::Path| {
        let out = tmp.path().join("h.txt");
        cwtools()
            .arg("validate")
            .args([
                "--game",
                "stellaris",
                "--rules",
                rules_dir.to_str().unwrap(),
                "--output-hashes",
                out.to_str().unwrap(),
            ])
            .args(args)
            .current_dir(cwd)
            .assert()
            .success();
        std::fs::read_to_string(&out).unwrap()
    };
    let by_flag = hashes(&["--directory", mod_dir.to_str().unwrap()], tmp.path());
    let by_config = hashes(&[], &mod_dir);
    assert!(!by_flag.trim().is_empty(), "the fixture emits CW107");
    assert_eq!(
        by_flag, by_config,
        "config `directory = \".\"` must not shift the digests"
    );
}

/// A shared config carries keys for every subcommand; the ones this command
/// won't act on are named rather than quietly dropped.
#[test]
fn test_config_warns_about_keys_the_command_ignores() {
    let tmp = tempfile::tempdir().unwrap();
    let loc = tmp.path().join("mod").join("localisation");
    std::fs::create_dir_all(&loc).unwrap();
    std::fs::copy(
        fixtures_dir().join("loc/localisation/l_english.yml"),
        loc.join("l_english.yml"),
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("cwtools.toml"),
        "directory = \"mod\"\nvanilla = \"game\"\ncase-sensitive-files = false\n",
    )
    .unwrap();
    cwtools()
        .arg("loc")
        .current_dir(tmp.path())
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "sets vanilla, case-sensitive-files, which `loc` does not read",
        ));
}
