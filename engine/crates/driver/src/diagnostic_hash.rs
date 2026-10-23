//! Stable diagnostic digests shared by batch and editor diagnostics.

use cwtools_file_manager::path_identity;
use std::borrow::Cow;
use std::path::Path;

/// FNV-1a-64 hex digest of `parts`, joined by `|`.
fn fnv1a_digest(parts: [&str; 4]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut mix = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    };
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            mix(b'|');
        }
        for b in part.bytes() {
            mix(b);
        }
    }
    format!("{:016x}", h)
}

/// `file` relative to `root` and `/`-separated, for hashing. Falls back to
/// `file` when it isn't under `root`; this is lexical and does no filesystem IO.
fn relative_file(file: &str, root: &Path) -> String {
    let mut file = file.replace('\\', "/");
    let root_str = root.to_string_lossy().replace('\\', "/");
    if let Some(drive) = drive_letter(&root_str)
        && !file.contains(':')
        && let Some(rest) = file.strip_prefix('/')
    {
        file = format!("{drive}:/{rest}");
    }
    match Path::new(&file).strip_prefix(Path::new(&root_str)) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => file,
    }
}

fn drive_letter(path: &str) -> Option<char> {
    let mut chars = path.chars();
    let drive = chars.next().filter(char::is_ascii_alphabetic)?;
    (chars.next() == Some(':')).then_some(drive)
}

/// Stable content-based digest of a diagnostic. `line_text` is the trimmed
/// offending line, and `file` is normalized relative to `root` before hashing.
pub fn diag_hash(root: &Path, file: &str, code: &str, message: &str, line_text: &str) -> String {
    fnv1a_digest([relative_file(file, root).as_str(), code, message, line_text])
}

/// Previous line-number-based digest, retained to match existing baselines.
pub fn legacy_diag_hash(root: &Path, file: &str, code: &str, message: &str, line: u32) -> String {
    fnv1a_digest([
        relative_file(file, root).as_str(),
        code,
        message,
        &line.to_string(),
    ])
}

fn relative_native_path_identity(root: &Path, file: &Path) -> Cow<'static, str> {
    let absolute_root = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
    let absolute_file = std::path::absolute(file).unwrap_or_else(|_| file.to_path_buf());
    let relative = absolute_file
        .strip_prefix(&absolute_root)
        .unwrap_or(&absolute_file);
    Cow::Owned(path_identity(relative).into_owned())
}

/// Stable diagnostic digest using the native path for lossless non-UTF-8
/// identity; UTF-8 paths retain the historical string normalization exactly.
pub fn diag_hash_for_native_path(
    root: &Path,
    file_key: &str,
    native_file: &Path,
    code: &str,
    message: &str,
    line_text: &str,
) -> String {
    if native_file.to_str().is_some() {
        return diag_hash(root, file_key, code, message, line_text);
    }
    let identity = relative_native_path_identity(root, native_file);
    fnv1a_digest([&identity, code, message, line_text])
}

/// Legacy line-number digest using native path identity for non-UTF-8 paths.
pub fn legacy_diag_hash_for_native_path(
    root: &Path,
    file_key: &str,
    native_file: &Path,
    code: &str,
    message: &str,
    line: u32,
) -> String {
    if native_file.to_str().is_some() {
        return legacy_diag_hash(root, file_key, code, message, line);
    }
    let identity = relative_native_path_identity(root, native_file);
    fnv1a_digest([&identity, code, message, &line.to_string()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_test_support::abs;
    use std::path::PathBuf;

    #[test]
    fn stable_hash_golden_vector() {
        assert_eq!(
            legacy_diag_hash(
                Path::new("."),
                "common/ideas/x.txt",
                "CW282",
                "redundant default",
                12
            ),
            "8e7fd969bd9ea463"
        );
    }

    #[test]
    fn content_hash_is_stable_across_path_spellings() {
        let root = PathBuf::from(abs("repo/mod"));
        let spellings = [
            abs("repo/mod/common/x.txt"),
            "common/x.txt".to_string(),
            abs("repo/mod/./common/x.txt"),
            r"\repo\mod\common\x.txt".to_string(),
        ];
        let hashes: Vec<_> = spellings
            .iter()
            .map(|file| diag_hash(&root, file, "CW282", "m", "cost = 150"))
            .collect();
        assert!(hashes.iter().all(|hash| hash == &hashes[0]));
    }

    #[test]
    fn relative_path_preserves_outside_root_fallback() {
        let root = PathBuf::from(abs("repo/mod"));
        let outside = abs("vanilla/common/x.txt");
        assert_eq!(relative_file(&outside, &root), outside);
    }

    #[test]
    fn relative_path_handles_windows_drive_spelling() {
        let root = PathBuf::from(abs("repo/mod"));
        let win = r"C:\repo\mod\common\x.txt";
        let got = relative_file(win, &root);
        if cfg!(windows) {
            assert_eq!(got, "common/x.txt");
        } else {
            assert_eq!(got, win.replace('\\', "/"));
        }
    }

    #[cfg(unix)]
    #[cfg_attr(target_os = "macos", ignore = "macOS rejects non-UTF-8 filenames")]
    #[test]
    fn non_utf8_native_identity_is_root_relative_for_both_digests() {
        use std::os::unix::ffi::OsStringExt;

        let first_root = tempfile::tempdir_in(".").unwrap();
        let second_root = tempfile::tempdir_in(".").unwrap();
        let filename = std::ffi::OsString::from_vec(b"script-\x80.txt".to_vec());
        let first_file = std::path::absolute(first_root.path())
            .unwrap()
            .join(&filename);
        let second_file = std::path::absolute(second_root.path())
            .unwrap()
            .join(&filename);
        std::fs::write(&first_file, "bad = 1\n").unwrap();
        std::fs::write(&second_file, "bad = 1\n").unwrap();
        let first_key = path_identity(&first_file);
        let second_key = path_identity(&second_file);

        let first = diag_hash_for_native_path(
            first_root.path(),
            &first_key,
            &first_file,
            "CW282",
            "message",
            "bad = 1",
        );
        let second = diag_hash_for_native_path(
            second_root.path(),
            &second_key,
            &second_file,
            "CW282",
            "message",
            "bad = 1",
        );
        let first_legacy = legacy_diag_hash_for_native_path(
            first_root.path(),
            &first_key,
            &first_file,
            "CW282",
            "message",
            7,
        );
        let second_legacy = legacy_diag_hash_for_native_path(
            second_root.path(),
            &second_key,
            &second_file,
            "CW282",
            "message",
            7,
        );
        assert_eq!(first, second);
        assert_eq!(first_legacy, second_legacy);
    }
}
