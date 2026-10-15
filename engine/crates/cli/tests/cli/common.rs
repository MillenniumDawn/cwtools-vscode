use assert_cmd::Command;
use cwtools_test_support::scratch_home_env;
pub(crate) use cwtools_test_support::write_tree;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

pub(crate) fn cwtools() -> Command {
    let mut cmd = Command::cargo_bin("cwtools").unwrap();
    cmd.env("RUST_LOG", "");
    // So a run never picks up a locally installed base game and never writes
    // into the developer's real cwtools cache.
    cmd.envs(scratch_home_env());
    // Same reason as HOME: a suite run from a git hook inherits the hook's
    // GIT_DIR, and `--since` would resolve against that repo, not the fixture.
    for key in INHERITED_GIT_ENV {
        cmd.env_remove(key);
    }
    cmd
}

pub(crate) fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// The text of the fixture at `rel`, to hand to [`write_tree`].
pub(crate) fn fixture_text(rel: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(rel)).unwrap()
}

/// Variables git exports to a hook. The pre-push hook runs `cargo test
/// --workspace`, so these tests inherit them, and an inherited `GIT_DIR`
/// outranks `-C`: without this the fixture's `init`/`config`/`add`/`commit`
/// run against the developer's own checkout, writing a test identity into its
/// config and committing the fixture over its branch. Also cleared for the
/// binary under test, so `--since` is exercised against the fixture rather
/// than whatever repo invoked the suite.
pub(crate) const INHERITED_GIT_ENV: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_DIR",
    "GIT_INDEX_FILE",
    "GIT_NAMESPACE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX",
    "GIT_WORK_TREE",
];

/// `validate` over `dir` against the rules fixture, with `extra` appended.
pub(crate) fn validate_dir(dir: &Path, extra: &[&str]) -> assert_cmd::assert::Assert {
    let rules_dir = fixtures_dir().join("rules");
    let mut cmd = cwtools();
    cmd.args([
        "validate",
        "--game",
        "stellaris",
        "--directory",
        dir.to_str().unwrap(),
        "--rules",
        rules_dir.to_str().unwrap(),
    ]);
    cmd.args(extra);
    cmd.assert()
}

/// A rules directory with one hard problem (a reference to an undefined type)
/// and one soft one (a `## cardinality` the loader can't parse).
pub(crate) fn broken_rules_dir() -> TempDir {
    write_tree(&[(
        "broken.cwt",
        "types = { type[test_type] = { path = \"game/common\" } }\n\
         ## cardinality = 0..x\n\
         r = { a = <undefined_type> }\n",
    )])
}

/// A temp mod with one `common/` file carrying an empty `if` (CW121, fixable).
pub(crate) fn fix_mod() -> TempDir {
    write_tree(&[("common/hint.txt", "x = { if = { } }\n")])
}

/// A mod root whose only diagnostic is the CW107 from `mod_a`'s event, with
/// `cwtools.toml` written at `root/cwtools.toml`.
pub(crate) fn config_mod(body: &str) -> TempDir {
    let event = fixture_text("discover/mod_a/events/test.txt");
    write_tree(&[("mod/events/test.txt", &event), ("cwtools.toml", body)])
}

/// The `game`/`directory`/`rules` triple as a config body, with `directory`
/// written relative so the resolve-against-the-config rule is under test.
pub(crate) fn config_body() -> String {
    format!(
        "game = \"stellaris\"\ndirectory = \"mod\"\nrules = {:?}\n",
        fixtures_dir().join("rules").to_str().unwrap()
    )
}
