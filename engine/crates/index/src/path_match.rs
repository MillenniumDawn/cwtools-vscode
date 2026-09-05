//! Path matching: whether a logical file path is covered by a type's
//! `path`/`path_file`/`path_extension` options.

use cwtools_rules::rules_types::PathOptions;

/// True if `needle` occurs in `haystack` as a whole path segment (or run of
/// segments), e.g. `gfx/models` is contained in `dlc/dlc022/gfx/models/units`.
/// Both inputs must already be lowercased and use '/' separators. This is THE
/// segment scan for both the indexer and the validator
/// (`cwtools_validation::resolve` imports it), so a file is INDEXED by the same
/// type that VALIDATES it. A bare `starts_with` would miss base-game content
/// nested under `dlc/<id>/…`, leaving its instances unindexed while the
/// referencing files still validate (false CW500s).
pub fn path_contains_segment(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        let abs = start + pos;
        let left_ok = abs == 0 || haystack.as_bytes().get(abs - 1) == Some(&b'/');
        let right = abs + needle.len();
        let right_ok = right == haystack.len() || haystack.as_bytes().get(right) == Some(&b'/');
        if left_ok && right_ok {
            return true;
        }
        // Advance by the char width at `abs` to avoid splitting a multi-byte
        // UTF-8 sequence (paths are ASCII-dominated but latent on non-Latin dirs).
        let char_width = haystack[abs..].chars().next().map_or(1, char::len_utf8);
        start = abs + char_width;
        if start >= haystack.len() {
            break;
        }
    }
    false
}

/// The one per-pattern directory test shared by the indexer (`check_path_dir`)
/// and the validator (`find_type_by_path_and_key`). `path_strict` means the
/// file sits DIRECTLY in the pattern directory: the dir must equal the pattern,
/// be the documented logical DLC shape `dlc/<nonempty-id>/<pattern>` (so
/// base-game content nested under `dlc/<id>/…` still matches, while e.g.
/// `not_dlc/<pattern>` does not), or be an absolute dir whose suffix is
/// `/<pattern>` at a segment boundary — the fallback for LSP logical paths
/// carrying no workspace prefix. Non-strict allows the pattern anywhere as a
/// whole segment run. Both inputs must be lowercased, '/'-separated, with no
/// trailing slash.
pub fn dir_matches_pattern(dir_lower: &str, pat_lower: &str, strict: bool) -> bool {
    if strict {
        if dir_lower == pat_lower {
            return true;
        }
        if is_absolute_normalized(dir_lower) {
            // Suffix is `/<pattern>` at a segment boundary.
            return dir_lower.len() > pat_lower.len()
                && dir_lower.ends_with(pat_lower)
                && dir_lower.as_bytes()[dir_lower.len() - pat_lower.len() - 1] == b'/';
        }
        // Relative dirs: only the documented DLC wrapper may prefix the pattern.
        let Some(rest) = dir_lower.strip_prefix("dlc/") else {
            return false;
        };
        match rest.split_once('/') {
            Some((id, tail)) => !id.is_empty() && tail == pat_lower,
            None => false,
        }
    } else {
        path_contains_segment(dir_lower, pat_lower)
    }
}

/// Host-independent absolute test for normalized dirs: a leading `/` covers
/// POSIX and UNC, `x:/` is a Windows drive letter. `Path::is_absolute` would
/// flip meaning between hosts, so it must not be used here.
fn is_absolute_normalized(dir_lower: &str) -> bool {
    if dir_lower.starts_with('/') {
        return true;
    }
    let b = dir_lower.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/'
}

/// Returns true when `logical_path` (e.g. `"events/my_events.txt"`) is covered
/// by `path_options`. The directory must equal the pattern when `path_strict`
/// (or sit in the logical `dlc/<id>/<pattern>` shape, or be an absolute dir
/// ending in the pattern — the fallback for LSP logical paths with no
/// workspace prefix), else contain it as a path segment (so base-game content
/// nested under `dlc/<id>/…` is indexed by the same type that validates it).
/// Also enforces `path_file` (exact filename match) and `path_extension`
/// (extension match), mirroring the validator's `find_type_by_path_and_key`.
pub fn check_path_dir(opts: &PathOptions, logical_path: &str) -> bool {
    check_path_dir_norm(opts, &NormalizedPath::new(logical_path))
}

/// A logical path pre-split into lowercase directory + basename. Compute once
/// per file and reuse across every type's [`check_path_dir_norm`] probe instead
/// of re-normalising and re-lowercasing the same path per type.
pub struct NormalizedPath {
    dir_lower: String,
    basename_lower: String,
}

impl NormalizedPath {
    pub fn new(logical_path: &str) -> Self {
        let norm = logical_path.replace('\\', "/");
        let basename = norm.rsplit('/').next().unwrap_or(&norm);
        let basename_lower = basename.to_ascii_lowercase();
        let dir = match norm.rfind('/') {
            Some(idx) => &norm[..idx],
            None => "",
        };
        let dir_lower = dir.to_ascii_lowercase();
        Self {
            dir_lower,
            basename_lower,
        }
    }
}

/// As [`check_path_dir`], but takes a pre-normalised path so callers looping over
/// all types pay the normalisation cost once per file rather than per type.
pub fn check_path_dir_norm(opts: &PathOptions, np: &NormalizedPath) -> bool {
    let basename_lower: &str = &np.basename_lower;

    // path_file: exact filename constraint (precomputed by reindex when available).
    if let Some(pf_lower) = &opts.path_file_lower {
        if basename_lower != pf_lower.as_str() {
            return false;
        }
    } else if let Some(pf) = &opts.path_file
        && basename_lower != pf.to_ascii_lowercase().as_str()
    {
        return false;
    }

    // path_extension: file extension constraint (precomputed by reindex when available).
    let check_ext = |ext: &str| {
        if !ext.is_empty() {
            let has_ext = basename_lower.rsplit('.').next().is_some_and(|e| e == ext);
            if !has_ext {
                return false;
            }
        }
        true
    };
    if let Some(ext) = &opts.path_ext_lower {
        if !check_ext(ext) {
            return false;
        }
    } else if let Some(ext) = &opts.path_extension {
        let ext = ext.to_ascii_lowercase();
        let ext = ext.strip_prefix('.').unwrap_or(&ext);
        if !check_ext(ext) {
            return false;
        }
    }

    if opts.paths.is_empty() {
        return true;
    }

    let dir_lower = np.dir_lower.as_str();

    if opts.paths_lower.is_empty() && !opts.paths.is_empty() {
        // Fallback for PathOptions built without reindex() (e.g. tests).
        for p in &opts.paths {
            let pat = p.replace('\\', "/");
            let pat = pat.trim_matches('/');
            let pat_lower = pat.to_ascii_lowercase();
            if dir_matches_pattern(dir_lower, &pat_lower, opts.path_strict) {
                return true;
            }
        }
        return false;
    }

    for pat_lower in &opts.paths_lower {
        if dir_matches_pattern(dir_lower, pat_lower, opts.path_strict) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_rules::rules_types::{RuleSet, TypeDefinition};

    /// A strict `common/foo` PathOptions, either raw (paths only, no reindex)
    /// or reindexed (paths_lower populated via `RuleSet::reindex`).
    fn strict_opts(reindexed: bool) -> PathOptions {
        let mut rs = RuleSet::new();
        rs.types.push(TypeDefinition {
            name: "foo".to_string(),
            name_field: None,
            path_options: PathOptions {
                paths: vec!["common/foo".to_string()],
                path_strict: true,
                ..Default::default()
            },
            subtypes: Vec::new(),
            type_key_filter: None,
            skip_root_key: Vec::new(),
            starts_with: None,
            type_per_file: false,
            key_prefix: None,
            warning_only: false,
            unique: false,
            should_be_referenced: false,
            localisation: Vec::new(),
            graph_related_types: Vec::new(),
            modifiers: Vec::new(),
        });
        if reindexed {
            rs.reindex();
        }
        rs.types[0].path_options.clone()
    }

    /// (logical_path, expected) for strict `common/foo`. The negative cases pin
    /// the segment boundary: a nested subdir, a non-DLC prefix, a non-segment
    /// prefix, and an off-by-one sibling must all be rejected. Relative dirs
    /// accept only the exact pattern or the documented `dlc/<id>/<pattern>`
    /// shape; absolute dirs (POSIX/UNC `/…`, Windows `c:/…` — the LSP fallback
    /// when no workspace prefix applies) keep the pre-existing suffix match,
    /// which is why the absolute `not_dlc/…` row is still accepted.
    const CASES: &[(&str, bool)] = &[
        ("common/foo/00_foo.txt", true),
        ("dlc/dlc022/common/foo/00_foo.txt", true),
        ("dlc//common/foo/00_foo.txt", false),
        ("/home/user/mod/common/foo/00_foo.txt", true),
        ("//server/share/common/foo/00_foo.txt", true),
        ("C:/Users/mod/common/foo/00_foo.txt", true),
        ("/home/user/mod/not_dlc/common/foo/00_foo.txt", true),
        ("common/foo/subdir/00_foo.txt", false),
        ("dlc/dlc022/common/foo/subdir/00_foo.txt", false),
        ("not_dlc/common/foo/00_foo.txt", false),
        ("not_common/foo/00_foo.txt", false),
        ("common/foo2/00_foo.txt", false),
        ("/home/user/mod/common/foo2/00_foo.txt", false),
    ];

    #[test]
    fn strict_matching_raw_path_options() {
        let opts = strict_opts(false);
        assert!(
            opts.paths_lower.is_empty(),
            "raw opts must hit the fallback branch"
        );
        for &(path, expected) in CASES {
            assert_eq!(
                check_path_dir(&opts, path),
                expected,
                "raw strict match for {path:?}"
            );
        }
    }

    #[test]
    fn strict_matching_reindexed_path_options() {
        let opts = strict_opts(true);
        assert_eq!(opts.paths_lower, vec!["common/foo".to_string()]);
        for &(path, expected) in CASES {
            assert_eq!(
                check_path_dir(&opts, path),
                expected,
                "reindexed strict match for {path:?}"
            );
        }
    }
}
