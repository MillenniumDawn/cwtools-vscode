//! Test-only baseline primitives for the editor baseline implementation.
//!
//! The next integration component removes the `cfg(test)` module gate once these
//! helpers are wired into configuration and diagnostic publication.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

const MAX_BASELINE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BaselineStatus {
    Disabled,
    Missing {
        file: PathBuf,
    },
    Loaded {
        file: PathBuf,
        hashes: HashSet<String>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BaselineError {
    InvalidTarget(String),
    Read { file: PathBuf, message: String },
    TooLarge { file: PathBuf },
    Malformed { file: PathBuf, line: usize },
}

impl BaselineError {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::InvalidTarget(message) => {
                format!("Invalid diagnostics baseline target: {message}")
            }
            Self::Read { file, message } => format!(
                "Could not read diagnostics baseline {}: {message}",
                file.display()
            ),
            Self::TooLarge { file } => format!(
                "Diagnostics baseline {} exceeds the 1 MiB limit",
                file.display()
            ),
            Self::Malformed { file, line } => format!(
                "Malformed diagnostics baseline {} at line {line}: expected a 16-digit hexadecimal hash or comment",
                file.display()
            ),
        }
    }
}

/// Resolve a workspace-relative file while applying the editor's regular-file,
/// canonical-containment, and symlink checks. The workspace root must exist.
pub(crate) fn resolve_target(root: &Path, relative: &str) -> Result<PathBuf, BaselineError> {
    let relative_path = Path::new(relative);
    if relative.is_empty()
        || relative_path.is_absolute()
        || relative_path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(BaselineError::InvalidTarget(format!(
            "{relative:?} must be a non-empty workspace-relative path without '..'"
        )));
    }
    let canonical_root = std::fs::canonicalize(root).map_err(|error| {
        BaselineError::InvalidTarget(format!(
            "workspace root {} cannot be resolved: {error}",
            root.display()
        ))
    })?;
    let target = canonical_root.join(relative_path);
    crate::access::editable_target(&target, std::slice::from_ref(&canonical_root)).map_err(
        |refusal| {
            BaselineError::InvalidTarget(format!("{} ({})", target.display(), refusal.reason()))
        },
    )
}

/// Load the CLI-compatible newline-separated hash file. Blank lines and `#`
/// comments are ignored; the file is bounded before its contents are parsed.
pub(crate) fn load(root: &Path, setting: Option<&str>) -> Result<BaselineStatus, BaselineError> {
    let Some(setting) = setting.filter(|value| !value.is_empty()) else {
        return Ok(BaselineStatus::Disabled);
    };
    let file = resolve_target(root, setting)?;
    let metadata = match std::fs::symlink_metadata(&file) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BaselineStatus::Missing { file });
        }
        Err(error) => {
            return Err(BaselineError::Read {
                file,
                message: error.to_string(),
            });
        }
    };
    if metadata.len() > MAX_BASELINE_BYTES {
        return Err(BaselineError::TooLarge { file });
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    std::fs::File::open(&file)
        .and_then(|input| input.take(MAX_BASELINE_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| BaselineError::Read {
            file: file.clone(),
            message: error.to_string(),
        })?;
    if bytes.len() as u64 > MAX_BASELINE_BYTES {
        return Err(BaselineError::TooLarge { file });
    }
    let text = String::from_utf8(bytes).map_err(|error| BaselineError::Read {
        file: file.clone(),
        message: format!("baseline is not UTF-8: {error}"),
    })?;
    let mut hashes = HashSet::new();
    for (index, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.len() != 16 || !line.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(BaselineError::Malformed {
                file,
                line: index + 1,
            });
        }
        hashes.insert(line.to_ascii_lowercase());
    }
    Ok(BaselineStatus::Loaded { file, hashes })
}

/// Match either the current content digest or the previous line-number digest.
pub(crate) fn matches(status: &BaselineStatus, current: &str, legacy: &str) -> bool {
    match status {
        BaselineStatus::Loaded { hashes, .. } => {
            hashes.contains(current) || (!legacy.is_empty() && hashes.contains(legacy))
        }
        BaselineStatus::Disabled | BaselineStatus::Missing { .. } => false,
    }
}

/// Stable output representation shared with the CLI's sorted, unique hash file.
pub(crate) fn render_hashes<'a>(hashes: impl IntoIterator<Item = &'a str>) -> String {
    let mut hashes: Vec<&str> = hashes.into_iter().collect();
    hashes.sort_unstable();
    hashes.dedup();
    hashes.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_driver::diagnostic_hash::{diag_hash, legacy_diag_hash};
    use std::fs;

    fn workspace() -> tempfile::TempDir {
        tempfile::tempdir().expect("workspace")
    }

    #[test]
    fn disabled_missing_and_loaded_states() {
        let root = workspace();
        assert_eq!(load(root.path(), None).unwrap(), BaselineStatus::Disabled);
        assert!(matches!(
            load(root.path(), Some("")).unwrap(),
            BaselineStatus::Disabled
        ));
        assert!(matches!(
            load(root.path(), Some("baseline.txt")).unwrap(),
            BaselineStatus::Missing { .. }
        ));
        fs::write(
            root.path().join("baseline.txt"),
            "# previous hashes\n\n0123456789abcdef\n",
        )
        .unwrap();
        let BaselineStatus::Loaded { hashes, .. } =
            load(root.path(), Some("baseline.txt")).unwrap()
        else {
            panic!("expected loaded baseline")
        };
        assert!(hashes.contains("0123456789abcdef"));
    }

    #[test]
    fn current_and_legacy_cli_digests_match_and_content_digest_roundtrips() {
        let root = workspace();
        let source = root.path().join("script.txt");
        fs::write(&source, "bad = yes\n").unwrap();
        let current = diag_hash(
            root.path(),
            source.to_str().unwrap(),
            "CW282",
            "invalid",
            "bad = yes",
        );
        let legacy = legacy_diag_hash(root.path(), source.to_str().unwrap(), "CW282", "invalid", 1);
        fs::write(
            root.path().join("baseline.txt"),
            format!("{current}\n{legacy}\n"),
        )
        .unwrap();
        let status = load(root.path(), Some("baseline.txt")).unwrap();
        assert!(matches(&status, &current, ""));
        assert!(matches(&status, "different-content", &legacy));
        assert_eq!(
            render_hashes([legacy.as_str(), current.as_str(), current.as_str()]),
            format!("{current}\n{legacy}")
        );
    }

    #[test]
    fn malformed_and_unreadable_baselines_return_actionable_errors() {
        let root = workspace();
        fs::write(root.path().join("bad.txt"), "not-a-hash\n").unwrap();
        let malformed = load(root.path(), Some("bad.txt")).unwrap_err();
        assert!(matches!(
            malformed,
            BaselineError::Malformed { line: 1, .. }
        ));
        assert!(malformed.message().contains("line 1"));
        fs::create_dir(root.path().join("directory.txt")).unwrap();
        assert!(matches!(
            load(root.path(), Some("directory.txt")),
            Err(BaselineError::InvalidTarget(_))
        ));
        fs::write(
            root.path().join("large.txt"),
            vec![b'a'; MAX_BASELINE_BYTES as usize + 1],
        )
        .unwrap();
        assert!(matches!(
            load(root.path(), Some("large.txt")),
            Err(BaselineError::TooLarge { .. })
        ));
    }

    #[test]
    fn target_resolution_rejects_absolute_parent_and_symlink_escape() {
        let root = workspace();
        let outside = workspace();
        for invalid in ["../outside.txt", "/tmp/outside.txt"] {
            assert!(matches!(
                resolve_target(root.path(), invalid),
                Err(BaselineError::InvalidTarget(_))
            ));
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
            assert!(matches!(
                resolve_target(root.path(), "linked/outside.txt"),
                Err(BaselineError::InvalidTarget(_))
            ));
        }
    }

    #[test]
    fn hash_output_is_sorted_unique_and_empty_is_empty() {
        assert_eq!(render_hashes(["b", "a", "b"]), "a\nb");
        assert_eq!(render_hashes(std::iter::empty()), "");
    }
}
