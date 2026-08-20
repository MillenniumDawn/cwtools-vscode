//! The only place `clearAllCaches` turns a cache directory into deletions.
//!
//! The cache root is `initializationOptions.cacheDir`, and the server has no say
//! in what a client sends, so nothing here derives a recursive delete from it.
//! Each half of the cache identifies its own entries — the parse caches through
//! [`workspace_cache::remove_all`], the base-game caches through
//! [`vanilla_cache::is_cache_file`] — and everything else is left where it is,
//! including a foreign directory that happens to be named `parse-cache` (#159).
//!
//! Only `remove_file` and empty-directory `remove_dir` are ever called, and
//! neither follows a symlink at the name it is handed: an entry swapped for a
//! link between its check and its removal costs the link, not its target. That
//! holds for the leaf being removed. A directory *component* replaced mid-walk
//! resolves through the new link like any other path would, so the checks here
//! are point-in-time and not a defence against a race.

use std::path::Path;

use cwtools_cache::workspace as workspace_cache;
use cwtools_info::vanilla_cache;

/// Delete every cwtools cache under `cache_dir`. Returns the number of cache
/// files removed and one `"<path>: <error>"` line per entry that resisted.
pub(crate) fn purge_caches(cache_dir: &Path) -> (usize, Vec<String>) {
    // A symlinked root is refused outright: every path below is derived from it,
    // so following one would move the whole purge somewhere the client named
    // only indirectly. Reported, not just logged, or the command answers
    // "cleared" having done nothing.
    if std::fs::symlink_metadata(cache_dir).is_ok_and(|metadata| metadata.is_symlink()) {
        tracing::warn!(path = %cache_dir.display(), "cache dir is a symlink; nothing purged");
        return (
            0,
            vec![format!(
                "{}: cannot purge a symlinked cache directory",
                cache_dir.display()
            )],
        );
    }
    // Canonical from here on, so every entry below is `<canonical root>/<one
    // component>` and containment needs no second opinion.
    let root = match std::fs::canonicalize(cache_dir) {
        Ok(root) => root,
        // No cache directory yet is the ordinary case; anything else (no
        // permission, a file in the way) is a purge that did not happen.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (0, Vec::new()),
        Err(error) => {
            return (0, vec![format!("{}: {}", cache_dir.display(), error)]);
        }
    };

    let removal = workspace_cache::remove_all(&root);
    let mut files = removal.files;
    let mut failures: Vec<String> = removal
        .failures
        .into_iter()
        .map(|(path, error)| format!("{}: {}", path.display(), error))
        .collect();

    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) => {
            failures.push(format!("{}: {}", root.display(), error));
            return (files, failures);
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let path = root.join(&name);
        if vanilla_cache::is_cache_file(&path) {
            match std::fs::remove_file(&path) {
                Ok(()) => files += 1,
                Err(error) => failures.push(format!("{}: {}", path.display(), error)),
            }
        } else if name.to_string_lossy().starts_with("vanilla-") {
            // Named like a base-game cache but not one of ours (no header, a
            // symlink, a directory). Logged by path so a stale cache from a
            // format that predates the header can still be found by hand.
            tracing::debug!(path = %path.display(), "not a base-game cache; left alone");
        }
    }
    (files, failures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A cache file in the shape the real writer leaves it.
    fn write_vanilla_cache(dir: &Path, fingerprint: &str) -> std::path::PathBuf {
        let path = dir.join(vanilla_cache::cache_file_name("hoi4", fingerprint));
        let empty = HashMap::new();
        vanilla_cache::save_per_type(&empty, "hoi4", fingerprint, &path, Default::default())
            .unwrap();
        path
    }

    #[test]
    fn purge_removes_the_caches_it_owns() {
        let tmp = tempfile::tempdir().unwrap();
        // The one thing that creates a parse-cache directory, signature and all.
        workspace_cache::validate_or_clear(tmp.path(), 0xdead_beef).unwrap();
        let cache = write_vanilla_cache(tmp.path(), "v1");

        let (removed, failures) = purge_caches(tmp.path());

        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(removed, 2, "settings.sig + the vanilla cache");
        assert!(!cache.exists());
        assert!(!tmp.path().join("parse-cache").exists());
    }

    #[test]
    fn purge_leaves_everything_it_does_not_own() {
        // The #159 report: `cacheDir` points at a directory holding unrelated
        // `parse-cache` and `vanilla-*` entries.
        let tmp = tempfile::tempdir().unwrap();
        let foreign_dir = tmp.path().join("parse-cache").join("important");
        std::fs::create_dir_all(&foreign_dir).unwrap();
        let foreign_file = foreign_dir.join("notes.txt");
        std::fs::write(&foreign_file, b"keep me").unwrap();
        let named_like_a_cache = tmp.path().join("vanilla-holiday.cwv");
        std::fs::write(&named_like_a_cache, b"keep me too").unwrap();
        let not_a_cache = tmp.path().join("vanilla-recipes.txt");
        std::fs::write(&not_a_cache, b"keep me three").unwrap();

        let (removed, failures) = purge_caches(tmp.path());

        assert_eq!(removed, 0);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(std::fs::read(&foreign_file).unwrap(), b"keep me");
        assert!(named_like_a_cache.exists());
        assert!(not_a_cache.exists());
    }

    #[cfg(unix)]
    #[test]
    fn purge_refuses_a_symlinked_cache_root() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        workspace_cache::validate_or_clear(&real, 1).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let (removed, failures) = purge_caches(&link);

        assert_eq!(removed, 0);
        assert_eq!(
            failures.len(),
            1,
            "a refusal must be reported: {failures:?}"
        );
        assert!(real.join("parse-cache").exists());
    }

    #[cfg(unix)]
    #[test]
    fn purge_does_not_follow_a_symlinked_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = write_vanilla_cache(outside.path(), "v1");
        let link = tmp
            .path()
            .join(vanilla_cache::cache_file_name("hoi4", "v2"));
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let (removed, failures) = purge_caches(tmp.path());

        assert_eq!(removed, 0);
        assert!(failures.is_empty(), "{failures:?}");
        assert!(target.exists(), "the symlink's target must survive");
    }

    #[test]
    fn purge_of_a_missing_cache_dir_is_a_no_op() {
        let tmp = tempfile::tempdir().unwrap();

        let (removed, failures) = purge_caches(&tmp.path().join("nothing-here"));

        assert_eq!(removed, 0);
        assert!(failures.is_empty(), "{failures:?}");
    }
}
