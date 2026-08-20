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
/// and the validator (`find_type_by_path_and_key`).
/// `path_strict` means the file sits DIRECTLY in the pattern directory: the dir
/// must equal the pattern or end with `/<pattern>` (so base-game content nested
/// under `dlc/<id>/…` still matches). Non-strict allows the pattern anywhere as
/// a whole segment run. Both inputs must be lowercased, '/'-separated, with no
/// trailing slash.
pub fn dir_matches_pattern(dir_lower: &str, pat_lower: &str, strict: bool) -> bool {
    if strict {
        dir_lower == pat_lower
            || (dir_lower.len() > pat_lower.len()
                && dir_lower.ends_with(pat_lower)
                && dir_lower.as_bytes()[dir_lower.len() - pat_lower.len() - 1] == b'/')
    } else {
        path_contains_segment(dir_lower, pat_lower)
    }
}

/// Returns true when `logical_path` (e.g. `"events/my_events.txt"`) is covered
/// by `path_options`. The directory must equal the pattern when `path_strict`,
/// else contain it as a path segment (so base-game content nested under
/// `dlc/<id>/…` is indexed by the same type that validates it).
///
/// Also enforces `path_file` (exact filename match) and `path_extension` (extension
/// match), mirroring the validator's `find_type_by_path_and_key` behaviour.
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
