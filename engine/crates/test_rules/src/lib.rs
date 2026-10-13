//! The upstream rules the engine tests read, at the commits `rules-pins.json`
//! pins. Nothing is committed here: the first test to ask fetches the commit
//! into the user cache dir with the same git recipe the extension uses, and every
//! run after that reads the cached copy.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The `config` directory of `cwtools-stellaris-config` at the pinned commit.
///
/// Panics when the commit is neither cached nor fetchable. A skip would drop
/// the tests that read it without saying so.
pub fn stellaris_config() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| checkout("stellaris", "cwtools/cwtools-stellaris-config").join("config"))
}

/// The `Config` directory of `cwtools-hoi4-config` at the pinned commit.
/// Panics like [`stellaris_config`].
pub fn hoi4_config() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| checkout("hoi4", "cwtools/cwtools-hoi4-config").join("Config"))
}

// Read at run time, so a bumped pin takes effect without a rebuild.
fn pin(game: &str) -> String {
    let pins = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../rules-pins.json");
    let text = std::fs::read_to_string(pins).unwrap_or_else(|err| panic!("read {pins}: {err}"));
    let manifest: serde_json::Value =
        serde_json::from_str(&text).expect("rules-pins.json is valid JSON");
    let sha = manifest["pins"][game]
        .as_str()
        .unwrap_or_else(|| panic!("rules-pins.json does not pin {game}"));
    assert!(
        sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
        "rules-pins.json pins {game} at `{sha}`, not a full commit SHA"
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

/// The GitHub repo `repo` at the commit pinned for `game`.
fn checkout(game: &str, repo: &str) -> PathBuf {
    let sha = pin(game);
    let cache = cache_dir();
    let dir = cache.join(format!("{game}-{sha}"));
    if dir.is_dir() {
        return dir;
    }
    std::fs::create_dir_all(&cache)
        .unwrap_or_else(|err| panic!("cannot create {}: {err}", cache.display()));
    // Fetched beside the entry and renamed in, so a second test binary never
    // reads a half-written checkout.
    let staging = tempfile::tempdir_in(&cache)
        .unwrap_or_else(|err| panic!("cannot create a directory in {}: {err}", cache.display()));
    let git = |args: &[&str]| {
        let why = match Command::new("git")
            .arg("-C")
            .arg(staging.path())
            .args(args)
            .output()
        {
            Ok(output) if output.status.success() => return,
            Ok(output) => String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            Err(err) => err.to_string(),
        };
        panic!(
            "{repo} at {sha}, the commit rules-pins.json pins for {game}, is not cached and \
             `git {}` failed: {why}\nThe tests that read it need git and network access once \
             per pin.",
            args.join(" ")
        );
    };
    let url = format!("https://github.com/{repo}");
    git(&["init", "--quiet"]);
    git(&["fetch", "--quiet", "--depth", "1", &url, &sha]);
    git(&["checkout", "--quiet", "--detach", &sha]);
    if let Err(err) = std::fs::rename(staging.path(), &dir)
        && !dir.is_dir()
    {
        panic!("cannot move the fetched rules to {}: {err}", dir.display());
    }
    dir
}
