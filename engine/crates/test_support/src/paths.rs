//! Paths the engine's tests and benches build from the same few places.

use std::path::PathBuf;

/// An absolute path spelled the host's way, for tests that must not hardcode a
/// leading `/`: on Windows that names the current drive's root, which
/// `Url::from_file_path` and `Path::is_absolute` both refuse.
pub fn abs(tail: &str) -> String {
    if cfg!(windows) {
        format!("C:/{tail}")
    } else {
        format!("/{tail}")
    }
}

/// [`abs`] as a `PathBuf`.
pub fn abs_path(tail: &str) -> PathBuf {
    PathBuf::from(abs(tail))
}

/// The `engine/testfiles` fixture root, shared by the parser, cache, driver and
/// `cwtools_lsp` tests. Join `performancetest2` for the Stellaris mod slice.
pub fn testfiles() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testfiles")
}

/// The folder holding the sibling checkouts the benches read: `CWTOOLS_PROJECTS`,
/// else the folder this repo sits in.
pub fn projects_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CWTOOLS_PROJECTS") {
        return Some(PathBuf::from(dir));
    }
    // From crates/test_support, the repo root is ../../.., and siblings sit next to it.
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    repo.join("..").canonicalize().ok()
}

/// A checkout for the benches: the folder `var` names, else `name` under
/// [`projects_dir`]. `None` when neither exists, so a bench can say why it
/// measured nothing.
pub fn checkout(var: &str, name: &str) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var(var) {
        return Some(PathBuf::from(dir));
    }
    let dir = projects_dir()?.join(name);
    dir.is_dir().then_some(dir)
}

/// The `cwtools-hoi4-config/Config` [`checkout`], named by `CWTOOLS_RULES`.
pub fn rules_dir() -> Option<PathBuf> {
    checkout("CWTOOLS_RULES", "cwtools-hoi4-config/Config")
}
