//! The upstream rules the engine tests read, at the commit `rules-pins.json`
//! pins. Nothing is committed here: the first test to ask fetches the commit
//! into the user cache dir with the same git recipe the extension uses, and every
//! run after that reads the cached copy.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const STELLARIS_REPO: &str = "https://github.com/cwtools/cwtools-stellaris-config";

/// The `config` directory of `cwtools-stellaris-config` at the pinned commit.
///
/// Panics when the commit is neither cached nor fetchable. A skip would drop
/// the tests that read it without saying so.
pub fn stellaris_config() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| checkout(&stellaris_pin()).join("config"))
}

// Read at run time, so a bumped pin takes effect without a rebuild.
fn stellaris_pin() -> String {
    let pins = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../rules-pins.json");
    let text = std::fs::read_to_string(pins).unwrap_or_else(|err| panic!("read {pins}: {err}"));
    let manifest: serde_json::Value =
        serde_json::from_str(&text).expect("rules-pins.json is valid JSON");
    let sha = manifest["pins"]["stellaris"]
        .as_str()
        .expect("rules-pins.json pins stellaris");
    assert!(
        sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
        "rules-pins.json pins stellaris at `{sha}`, not a full commit SHA"
    );
    sha.to_owned()
}

/// `XDG_CACHE_HOME` or `LOCALAPPDATA`, else `~/.cache`, else the temp dir.
fn cache_dir() -> PathBuf {
    let set = |var| std::env::var_os(var).filter(|value| !value.is_empty());
    set("XDG_CACHE_HOME")
        .or_else(|| set("LOCALAPPDATA"))
        .map(PathBuf::from)
        .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("cwtools")
        .join("test-rules")
}

fn checkout(sha: &str) -> PathBuf {
    let cache = cache_dir();
    let dir = cache.join(format!("stellaris-{sha}"));
    if dir.is_dir() {
        return dir;
    }
    std::fs::create_dir_all(&cache)
        .unwrap_or_else(|err| panic!("cannot create {}: {err}", cache.display()));
    // Fetched beside the entry and renamed in, so a second test binary never
    // reads a half-written checkout.
    let staging = tempfile::tempdir_in(&cache)
        .unwrap_or_else(|err| panic!("cannot create a directory in {}: {err}", cache.display()));
    git(staging.path(), sha, &["init", "--quiet"]);
    git(
        staging.path(),
        sha,
        &["fetch", "--quiet", "--depth", "1", STELLARIS_REPO, sha],
    );
    git(
        staging.path(),
        sha,
        &["checkout", "--quiet", "--detach", sha],
    );
    if let Err(err) = std::fs::rename(staging.path(), &dir)
        && !dir.is_dir()
    {
        panic!("cannot move the fetched rules to {}: {err}", dir.display());
    }
    dir
}

fn git(dir: &Path, sha: &str, args: &[&str]) {
    let fail = |why: String| -> ! {
        panic!(
            "cwtools-stellaris-config at {sha}, the commit rules-pins.json pins, is not cached \
             and `git {}` failed: {why}\nThe Stellaris rule tests need git and network access \
             once per pin.",
            args.join(" ")
        )
    };
    match Command::new("git").arg("-C").arg(dir).args(args).output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => fail(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
        Err(err) => fail(err.to_string()),
    }
}
