use crate::common::{cwtools, fix_mod, fixtures_dir};
use predicates::prelude::*;

// ── Automatic base-game cache ────────────────────────────────────────────────
//
// `--vanilla` keeps its index under the OS cache dir (XDG_CACHE_HOME here) so a
// repeat run doesn't re-parse the install; `--no-vanilla-cache` opts out.

/// Run `validate --vanilla <fixture>` with `cache` as the cache home, and return
/// the number of cache files that exist afterwards.
fn validate_with_cache_home(cache_home: &std::path::Path, extra: &[&str]) -> usize {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    let mut cmd = cwtools();
    cmd.env("XDG_CACHE_HOME", cache_home).args([
        "validate",
        "--game",
        "stellaris",
        "--directory",
        discover_dir.to_str().unwrap(),
        "--rules",
        rules_dir.to_str().unwrap(),
        "--vanilla",
        discover_dir.to_str().unwrap(),
    ]);
    cmd.args(extra).assert().success();
    let dir = cache_home.join("cwtools");
    std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "cwv"))
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn test_validate_vanilla_writes_and_reuses_the_cache() {
    let cache = tempfile::tempdir().unwrap();
    assert_eq!(
        validate_with_cache_home(cache.path(), &[]),
        1,
        "the first --vanilla run should write a cache"
    );
    assert_eq!(
        validate_with_cache_home(cache.path(), &[]),
        1,
        "a repeat run reuses that cache rather than writing another"
    );
    assert_eq!(
        validate_with_cache_home(cache.path(), &["--refresh-vanilla-cache"]),
        1,
        "--refresh-vanilla-cache overwrites in place"
    );
}

// ── Vanilla-gated check families ─────────────────────────────────────────────
//
// Without a base-game index CW113/CW222/CW500 (and, on Stellaris, CW227/CW229/
// CW250) report nothing, which reads as a clean file. The run has to say so.

#[test]
fn test_validate_notices_the_checks_a_missing_base_game_disables() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            discover_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--no-vanilla-cache",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("no base-game data loaded"))
        .stderr(predicate::str::contains(
            "CW113, CW222, CW227, CW229, CW250, CW500",
        ));
}

#[test]
fn test_validate_stays_quiet_about_the_gate_with_a_base_game() {
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            discover_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--vanilla",
            discover_dir.to_str().unwrap(),
            "--no-vanilla-cache",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("no base-game data loaded").not());
}

/// The contract the whole `.cwv` corruption suite exists to protect: a cache
/// that fails to load has to degrade to a re-index, not to a failed run and not
/// to silently missing base-game data. The unit tests prove `load` returns an
/// error; this proves the run recovers from it.
#[test]
fn test_validate_recovers_from_a_corrupt_vanilla_cache() {
    let cache = tempfile::tempdir().unwrap();
    assert_eq!(validate_with_cache_home(cache.path(), &[]), 1);

    let dir = cache.path().join("cwtools");
    let cwv = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "cwv"))
        .expect("the first run wrote a cache");

    // Damage a run in the middle of the compressed body. One flipped bit would
    // do in principle, but roughly one in two hundred lands in frame bits the
    // decoder ignores and reproduces the cache exactly (the sweep in
    // `vanilla_cache.rs` measures 4 of 858), and the byte layout depends on the
    // order the install walked, so a single fixed bit is not a stable choice
    // across platforms. A damaged run cannot decode on any of them.
    let mut bytes = std::fs::read(&cwv).unwrap();
    assert!(
        bytes.len() > 64,
        "cache too small to damage: {}",
        bytes.len()
    );
    let middle = bytes.len() / 2;
    for byte in &mut bytes[middle..middle + 16] {
        *byte ^= 0xff;
    }
    std::fs::write(&cwv, &bytes).unwrap();

    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .env("XDG_CACHE_HOME", cache.path())
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            discover_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--vanilla",
            discover_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("could not read base-game cache"))
        // The gate notice fires only when no base-game data loaded at all, so
        // its absence is what proves the install was re-indexed rather than the
        // run carrying on blind.
        .stderr(predicate::str::contains("no base-game data loaded").not());

    // And the damaged cache was replaced, so the next run is a hit again.
    assert_eq!(
        validate_with_cache_home(cache.path(), &[]),
        1,
        "the recovery run should have rewritten the cache"
    );
}

#[test]
fn test_validate_no_vanilla_cache_writes_nothing() {
    let cache = tempfile::tempdir().unwrap();
    assert_eq!(
        validate_with_cache_home(cache.path(), &["--no-vanilla-cache"]),
        0,
        "--no-vanilla-cache must not touch the cache dir"
    );
}

#[test]
fn test_validate_no_vanilla_cache_does_not_write_vanilla_parse_entries() {
    let cache = tempfile::tempdir().unwrap();
    let vanilla = tempfile::tempdir().unwrap();
    let scripts = vanilla.path().join("common").join("things");
    std::fs::create_dir_all(&scripts).unwrap();
    std::fs::write(scripts.join("x.txt"), "thing = { }\n").unwrap();

    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .env("XDG_CACHE_HOME", cache.path())
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            discover_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--vanilla",
            vanilla.path().to_str().unwrap(),
            "--no-vanilla-cache",
        ])
        .assert()
        .success();

    let fingerprint = cwtools_cache::workspace::settings_fingerprint("stellaris", vanilla.path());
    let vanilla_parse_cache = cache
        .path()
        .join("cwtools")
        .join("parse-cache")
        .join(format!("{fingerprint:016x}"));
    assert!(
        !vanilla_parse_cache.exists(),
        "--no-vanilla-cache must reparse vanilla instead of writing .cwb entries"
    );
}

#[test]
fn test_validate_missing_vanilla_directory_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no_such_vanilla");
    let discover_dir = fixtures_dir().join("discover").join("mod_a");
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args([
            "validate",
            "--game",
            "stellaris",
            "--directory",
            discover_dir.to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--vanilla",
            missing.to_str().unwrap(),
            "--no-vanilla-cache",
        ])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("discovery failed for vanilla"))
        .stderr(predicate::str::contains("report nothing"))
        .stdout(predicate::str::contains("[CW113]").not())
        .stderr(predicate::str::contains(missing.to_str().unwrap()));
}

#[test]
fn test_fix_missing_vanilla_directory_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no_such_vanilla");
    let mod_dir = fix_mod();
    let rules_dir = fixtures_dir().join("rules");
    cwtools()
        .args([
            "fix",
            "--game",
            "stellaris",
            "--directory",
            mod_dir.path().to_str().unwrap(),
            "--rules",
            rules_dir.to_str().unwrap(),
            "--vanilla",
            missing.to_str().unwrap(),
            "--no-vanilla-cache",
        ])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("discovery failed for vanilla"))
        .stderr(predicate::str::contains("report nothing"))
        .stdout(predicate::str::contains("[CW113]").not())
        .stderr(predicate::str::contains(missing.to_str().unwrap()));
}
