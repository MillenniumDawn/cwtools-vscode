//! A scratch home for the binaries a test spawns.

use std::path::PathBuf;
use std::sync::LazyLock;

/// It has to outlive the children, so it is a static: leaking the dir at process
/// exit is the point, the OS temp cleaner takes it from there.
static SCRATCH_HOME: LazyLock<tempfile::TempDir> =
    LazyLock::new(|| tempfile::tempdir().expect("failed to create scratch home"));

/// `HOME` and the cache dirs, pointed at a scratch tree, as `(name, value)`
/// pairs for `Command::envs`.
///
/// Pinning them keeps a spawned binary from discovering a locally installed base
/// game (and spending the test scanning it) and from writing into the developer's
/// real cwtools cache. Apply them before any per-test override: the later `env`
/// call wins.
pub fn scratch_home_env() -> [(&'static str, PathBuf); 3] {
    let home = SCRATCH_HOME.path();
    [
        ("HOME", home.to_path_buf()),
        ("XDG_CACHE_HOME", home.join("cache")),
        ("LOCALAPPDATA", home.join("localappdata")),
    ]
}
