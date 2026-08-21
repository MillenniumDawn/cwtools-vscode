//! Persistent per-file parse cache for workspace scans and batch validation.
//!
//! Entries can be keyed by source path, mtime, and size so a hit skips reading
//! the source file. Content-keyed access remains available for in-memory text.
//! A `settings.sig` records the game, workspace root, and cache format version.

use std::fs;
use std::path::{Path, PathBuf};

use crate::convert::{
    archived_to_arena, arena_to_cached, cached_errors_to_parse, errors_to_cached,
};
use crate::io::{
    read_errors_from_file, serialize_errors_to_file, serialize_to_file, with_archived_file,
};
use cwtools_parser::ast::ParsedFile;
use cwtools_string_table::string_table::StringTable;

/// Cache format version. Bump when the `CachedFile` layout changes (or the
/// fingerprint algorithm changes) so stale `.cwb` files are ignored
/// automatically.
///
/// v2: switched fingerprinting from `DefaultHasher` (SipHash, toolchain-unstable)
/// to a stable FNV-1a. The version is folded into the fingerprint, so old
/// SipHash-keyed cache directories no longer match and are treated as a miss
/// (one-time cold rebuild).
/// v3: dropped `CachedNode`/`CachedChild::Node` from the `CachedFile` layout.
/// v4: workspace scans discard comments before caching because only open-document
/// semantic-token parsing needs them.
/// v5: recovered parse errors are persisted with the AST.
/// v6: Leaf records the exact value range.
/// v7: dropped ruleset shape from the fingerprint. A `.cwt` edit cannot change
/// how a script file parses, so it must not clear the directory.
/// v8: folded the display locale into the fingerprint. The `.cwe` sidecar holds
/// each file's diagnostics as finished strings, and those are written in the
/// language the server was started in, so a cache built under one locale would
/// otherwise be served verbatim under another.
const CACHE_VERSION: u32 = 8;

/// Whether platform metadata provides a reliable no-read change stamp.
pub const PATH_METADATA_CACHE_SUPPORTED: bool = cfg!(unix);

// ── Fingerprinting ──────────────────────────────────────────────────────────

/// FNV-1a over `bytes`, continuing from `hash`. A stable, dependency-free hash
/// (unlike `std::hash::DefaultHasher`, whose SipHash output isn't guaranteed
/// stable across Rust toolchains) so cache keys stay comparable across restarts.
/// Mirrors `cwtools_info::vanilla_cache::fnv1a`.
fn fnv1a(bytes: &[u8], mut hash: u64) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// FNV-1a offset basis — the conventional seed for a fresh hash.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// Content hash of a file's text. FNV-1a is fast for short-to-medium files and
/// the collision surface is tiny (local cache only, not security-critical).
pub fn content_hash(text: &str) -> u64 {
    fnv1a(text.as_bytes(), FNV_OFFSET)
}

/// Source metadata captured before reading and parsing a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceCacheKey {
    hash: u64,
}

/// Capture the path, mtime, and size key used for a no-read cache lookup.
pub fn source_cache_key(path: &Path) -> Option<SourceCacheKey> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok());
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut hash = fnv1a(absolute.to_string_lossy().as_bytes(), FNV_OFFSET);
    hash = fnv1a(b"\x1e", hash);
    hash = fnv1a(&metadata.len().to_le_bytes(), hash);
    if let Some(modified) = modified {
        hash = fnv1a(&modified.as_secs().to_le_bytes(), hash);
        hash = fnv1a(&modified.subsec_nanos().to_le_bytes(), hash);
    } else {
        hash = fnv1a(&[0; 12], hash);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        hash = fnv1a(&metadata.dev().to_le_bytes(), hash);
        hash = fnv1a(&metadata.ino().to_le_bytes(), hash);
        hash = fnv1a(&metadata.ctime().to_le_bytes(), hash);
        hash = fnv1a(&metadata.ctime_nsec().to_le_bytes(), hash);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        hash = fnv1a(&metadata.creation_time().to_le_bytes(), hash);
        hash = fnv1a(&metadata.file_attributes().to_le_bytes(), hash);
    }
    Some(SourceCacheKey { hash })
}

/// Settings fingerprint: encodes everything that changes how a file parses.
/// If the fingerprint differs from `settings.sig`, the cached workspace
/// directory is stale and must be cleared. The ruleset is not part of this:
/// a `.cwb` entry is a parsed AST, which does not depend on `.cwt` rules.
pub fn settings_fingerprint(language: &str, workspace_root: &Path) -> u64 {
    fingerprint(language, workspace_root, cwtools_i18n::locale().tag())
}

fn fingerprint(language: &str, workspace_root: &Path, locale: &str) -> u64 {
    // A record separator between fields so concatenation is unambiguous
    // (`a` + `bc` can't collide with `ab` + `c`).
    let mut h = FNV_OFFSET;
    let sep = |h: u64| fnv1a(b"\x1e", h);
    // Game/language — changes scope definitions, keywords, etc.
    h = fnv1a(language.as_bytes(), h);
    h = sep(h);
    // Workspace root — distinguishes two mods opened in different windows.
    let workspace_root = fs::canonicalize(workspace_root).unwrap_or_else(|_| {
        std::path::absolute(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf())
    });
    h = fnv1a(workspace_root.to_string_lossy().as_bytes(), h);
    h = sep(h);
    // Display locale — the `.cwe` sidecar stores rendered diagnostic text.
    h = fnv1a(locale.as_bytes(), h);
    h = sep(h);
    // Bump version together so a format change also invalidates.
    fnv1a(&CACHE_VERSION.to_le_bytes(), h)
}

// ── Directory layout ────────────────────────────────────────────────────────

/// The one directory this module owns under a cache root.
const PARSE_CACHE_DIR: &str = "parse-cache";

/// Resolve the workspace parse-cache directory.
///
/// Layout: `<cache_dir>/parse-cache/<workspace-fingerprint-hex>/`
///
/// Returns `None` if no base cache dir can be resolved.
fn workspace_cache_dir(cache_dir: &Path, fingerprint: u64) -> PathBuf {
    cache_dir
        .join(PARSE_CACHE_DIR)
        .join(format!("{:016x}", fingerprint))
}

/// Path of the `settings.sig` file inside a workspace cache directory.
fn settings_sig_path(dir: &Path) -> PathBuf {
    dir.join("settings.sig")
}

/// Path of a per-file `.cwb` cache entry.
fn file_cache_path(dir: &Path, hash: u64) -> PathBuf {
    dir.join(format!("{:016x}.cwb", hash))
}

fn error_cache_path(dir: &Path, hash: u64) -> PathBuf {
    dir.join(format!("{:016x}.cwe", hash))
}

// ── Settings sig ────────────────────────────────────────────────────────────

/// Read the stored fingerprint from `settings.sig`. Returns `None` if the file
/// doesn't exist or can't be parsed.
fn read_settings_sig(dir: &Path) -> Option<u64> {
    let bytes = fs::read(settings_sig_path(dir)).ok()?;
    if bytes.len() != 8 {
        return None;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes);
    Some(u64::from_le_bytes(buf))
}

/// Write the current fingerprint to `settings.sig`.
fn write_settings_sig(dir: &Path, sig: u64) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(settings_sig_path(dir), sig.to_le_bytes())
}

/// Validate (and update) the settings signature. Returns `true` if the cache is
/// still valid; `false` if the directory was cleared and must be rebuilt.
pub fn validate_or_clear(cache_dir: &Path, fingerprint: u64) -> std::io::Result<bool> {
    // `cargo test` shares one SCRATCH_HOME across parallel test binaries; two
    // `validate_or_clear` with different fingerprints can race: one prunes an
    // empty fingerprint dir while the other is creating it. Treat a transient
    // NotFound/AlreadyExists/Interrupted as a retry rather than a hard
    // "parse cache unavailable" warning.
    match try_validate_or_clear(cache_dir, fingerprint) {
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::AlreadyExists
                    | std::io::ErrorKind::Interrupted
            ) =>
        {
            try_validate_or_clear(cache_dir, fingerprint)
        }
        other => other,
    }
}

fn try_validate_or_clear(cache_dir: &Path, fingerprint: u64) -> std::io::Result<bool> {
    let dir = workspace_cache_dir(cache_dir, fingerprint);
    // The path is built from a cache root the LSP client chose, and both arms
    // below write or delete through it. Anything already sitting there that is
    // not a directory of ours is refused rather than cleared: a symlink here
    // would send `clear_cache_dir` at its target's files (#159).
    if fs::symlink_metadata(&dir).is_ok_and(|metadata| !metadata.is_dir()) {
        return Err(std::io::Error::other(
            "cannot use a cache directory that is a symlink or a file",
        ));
    }
    match read_settings_sig(&dir) {
        Some(stored) if stored == fingerprint => {
            prune(cache_dir, fingerprint);
            Ok(true)
        }
        _ => {
            clear_cache_dir(&dir)?;
            write_settings_sig(&dir, fingerprint)?;
            prune_all_cache_dirs(cache_dir, &dir);
            Ok(false)
        }
    }
}

/// Enforce the per-fingerprint and global cache caps after a batch of writes.
pub fn prune(cache_dir: &Path, fingerprint: u64) {
    let dir = workspace_cache_dir(cache_dir, fingerprint);
    prune_cache_dir(&dir);
    prune_all_cache_dirs(cache_dir, &dir);
}

// ── Ownership-checked removal ───────────────────────────────────────────────

/// What [`remove_all`] managed to do.
#[derive(Debug, Default)]
pub struct Removal {
    /// Cache files removed.
    pub files: usize,
    /// Entries that could not be removed, each with the error that stopped it.
    pub failures: Vec<(PathBuf, std::io::Error)>,
}

/// Remove every parse cache under `cache_dir`, and nothing else.
///
/// Reach for this to clear the caches on demand (the LSP's `clearAllCaches`).
/// The cache root is whatever an LSP client asked for, so a path is removed
/// only when it is positively one of ours: a `parse-cache/` holding
/// fingerprint-named directories, each carrying the `settings.sig` written when
/// it was created. Nothing here recurses and no symlink is followed, so a
/// foreign directory that happens to be named `parse-cache` loses nothing.
pub fn remove_all(cache_dir: &Path) -> Removal {
    let root = cache_dir.join(PARSE_CACHE_DIR);
    let mut removal = Removal::default();
    // No cache yet is the ordinary case and says nothing. Anything else that
    // stops us reading the root (a file or a symlink in the way, no
    // permission) has to reach the caller: silence there reads as success.
    match fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            removal
                .failures
                .push((root, std::io::Error::other("not a directory")));
            return removal;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return removal,
        Err(error) => {
            removal.failures.push((root, error));
            return removal;
        }
    }
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) => {
            removal.failures.push((root, error));
            return removal;
        }
    };
    let mut owned = 0usize;
    let mut swept_all = true;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !is_owned_cache_dir(&dir) {
            tracing::debug!(path = %dir.display(), "not a cwtools cache directory; left alone");
            swept_all = false;
            continue;
        }
        owned += 1;
        swept_all &= remove_owned_cache_dir(&dir, &mut removal);
    }
    // Cosmetic, and earned only by having cleared something out of it: an
    // empty `parse-cache` we never wrote to is not ours to remove. The next
    // scan recreates our own.
    if swept_all && owned > 0 {
        let _ = fs::remove_dir(&root);
    }
    removal
}

/// Whether `path` is a directory in its own right, as opposed to a symlink to
/// one. Every step of a removal asks this before it opens or unlinks anything.
fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

/// Whether `dir` is one of the per-fingerprint directories this module writes:
/// a real directory named by a fingerprint, carrying the `settings.sig` that
/// [`validate_or_clear`] leaves there. The signature is the ownership marker —
/// it predates any need for one, because `validate_or_clear` is the only thing
/// that creates these directories and it writes the signature as it does.
fn is_owned_cache_dir(dir: &Path) -> bool {
    if !is_real_dir(dir) {
        return false;
    }
    let named_by_fingerprint = dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            // `{:016x}`, as `workspace_cache_dir` formats it.
            name.len() == 16
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        });
    named_by_fingerprint
        && fs::symlink_metadata(settings_sig_path(dir))
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() == 8)
}

/// Remove the files inside a cache directory that has already cleared
/// [`is_owned_cache_dir`], then the directory itself. Returns whether it went
/// completely: a subdirectory (nothing this module writes) is left alone, and
/// so is its parent.
fn remove_owned_cache_dir(dir: &Path, removal: &mut Removal) -> bool {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            removal.failures.push((dir.to_path_buf(), error));
            return false;
        }
    };
    let mut emptied = true;
    for entry in entries.flatten() {
        let path = entry.path();
        // `remove_file` unlinks the name it is given and never follows a
        // symlink to its target, so nothing destructive can slip through the
        // window between this check and that call.
        if !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
            emptied = false;
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => removal.files += 1,
            Err(error) => {
                removal.failures.push((path, error));
                emptied = false;
            }
        }
    }
    if emptied && let Err(error) = fs::remove_dir(dir) {
        removal.failures.push((dir.to_path_buf(), error));
        return false;
    }
    emptied
}

// ── Bounded cleanup ───────────────────────────────────────────────────────────

fn clear_cache_dir(dir: &Path) -> std::io::Result<()> {
    let files = match fs::read_dir(dir) {
        Ok(files) => Some(files),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if let Some(files) = files {
        for entry in files {
            let path = entry?.path();
            if path.is_file() {
                fs::remove_file(path)?;
            }
        }
    }
    fs::create_dir_all(dir)
}

/// Cap on the number of `.cwb` entries kept in a single workspace cache dir.
/// Each source version gets its own entry, so old versions must be bounded.
const MAX_CACHE_ENTRIES: usize = 50_000;

/// Cap on total `.cwb` and parse-error sidecar bytes in one workspace cache dir.
const MAX_CACHE_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB

/// Caps across all workspace/settings fingerprints under one cache root.
const MAX_TOTAL_CACHE_ENTRIES: usize = 100_000;
const MAX_TOTAL_CACHE_BYTES: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB

/// Prune to ~80% of the caps so we don't re-prune on every scan.
const PRUNE_TARGET_RATIO: f64 = 0.8;

/// If the cache dir exceeds either cap (entry count or total size), delete the
/// oldest `.cwb` entries by mtime until it's back under ~80% of both caps. One
/// directory scan; runs at most once per workspace scan. No-op when under cap.
fn prune_cache_dir(dir: &Path) {
    prune_cache_dir_with_caps(dir, MAX_CACHE_ENTRIES, MAX_CACHE_BYTES);
}

fn prune_all_cache_dirs(cache_dir: &Path, current_dir: &Path) {
    prune_all_cache_dirs_with_caps(
        cache_dir,
        MAX_TOTAL_CACHE_ENTRIES,
        MAX_TOTAL_CACHE_BYTES,
        Some(current_dir),
    );
}

fn prune_all_cache_dirs_with_caps(
    cache_dir: &Path,
    max_entries: usize,
    max_bytes: u64,
    current_dir: Option<&Path>,
) {
    let root = cache_dir.join(PARSE_CACHE_DIR);
    let Ok(dirs) = fs::read_dir(root) else {
        return;
    };
    // Ownership-checked, not just `is_dir`: the cache root comes from the LSP
    // client, and the sweep below deletes. An unrelated directory that happens
    // to sit under a `parse-cache` is none of our business (#159).
    let dirs: Vec<PathBuf> = dirs
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_owned_cache_dir(path))
        .collect();
    if dirs.len() > 2 {
        let mut entries = Vec::new();
        let mut total_bytes = 0u64;
        for dir in &dirs {
            collect_cache_entries(dir, &mut entries, &mut total_bytes);
        }
        prune_entries(entries, total_bytes, max_entries, max_bytes);
    }
    remove_empty_cache_dirs(&dirs, current_dir);
}

/// Sweep the cache dirs that no longer hold an entry. `dirs` has already
/// cleared [`is_owned_cache_dir`] — this deletes, so it is not the place to
/// meet a path that only looks like ours.
fn remove_empty_cache_dirs(dirs: &[PathBuf], current_dir: Option<&Path>) {
    for dir in dirs {
        if current_dir.is_some_and(|current| current == dir) || count_cache_entries(dir) != 0 {
            continue;
        }
        let mut removal = Removal::default();
        remove_owned_cache_dir(dir, &mut removal);
        for (path, error) in removal.failures {
            tracing::debug!(path = %path.display(), %error, "could not remove empty cache dir");
        }
    }
}

fn count_cache_entries(dir: &Path) -> usize {
    let Ok(files) = fs::read_dir(dir) else {
        return 0;
    };
    files
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "cwb")
        })
        .count()
}

/// Cap-parameterized core of [`prune_cache_dir`] (lets tests use small caps
/// instead of writing 50k files).
fn prune_cache_dir_with_caps(dir: &Path, max_entries: usize, max_bytes: u64) {
    let mut entries = Vec::new();
    let mut total_bytes = 0u64;
    collect_cache_entries(dir, &mut entries, &mut total_bytes);
    prune_entries(entries, total_bytes, max_entries, max_bytes);
}

fn collect_cache_entries(
    dir: &Path,
    entries: &mut Vec<(std::time::SystemTime, u64, PathBuf)>,
    total_bytes: &mut u64,
) {
    let Ok(files) = fs::read_dir(dir) else {
        return;
    };
    for entry in files.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "cwb") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let sidecar_size = path
            .with_extension("cwe")
            .metadata()
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let size = metadata.len().saturating_add(sidecar_size);
        let mtime = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
        *total_bytes = total_bytes.saturating_add(size);
        entries.push((mtime, size, path));
    }
}

fn prune_entries(
    mut entries: Vec<(std::time::SystemTime, u64, PathBuf)>,
    total_bytes: u64,
    max_entries: usize,
    max_bytes: u64,
) {
    if entries.len() <= max_entries && total_bytes <= max_bytes {
        return;
    }
    entries.sort_by_key(|(mtime, _, _)| *mtime);

    let target_count = (max_entries as f64 * PRUNE_TARGET_RATIO) as usize;
    let target_bytes = (max_bytes as f64 * PRUNE_TARGET_RATIO) as u64;
    let mut cur_count = entries.len();
    let mut cur_bytes = total_bytes;
    for (_, size, path) in entries {
        if cur_count <= target_count && cur_bytes <= target_bytes {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            let _ = fs::remove_file(path.with_extension("cwe"));
            cur_count -= 1;
            cur_bytes = cur_bytes.saturating_sub(size);
        }
    }
}

// ── Per-file load / store ───────────────────────────────────────────────────

fn load_hash(
    cache_dir: &Path,
    fingerprint: u64,
    hash: u64,
    table: &StringTable,
    load_errors: bool,
) -> Option<ParsedFile> {
    let dir = workspace_cache_dir(cache_dir, fingerprint);
    let path = file_cache_path(&dir, hash);
    let (arena, root_children) =
        with_archived_file(&path, |archived| archived_to_arena(archived, table))
            .ok()
            .and_then(Result::ok)?;
    let errors = if load_errors {
        cached_errors_to_parse(read_errors_from_file(&error_cache_path(&dir, hash)).ok()?)
    } else {
        Vec::new()
    };
    Some(ParsedFile {
        arena,
        root_children,
        errors,
    })
}

fn store_hash(
    cache_dir: &Path,
    fingerprint: u64,
    hash: u64,
    parsed: &ParsedFile,
    table: &StringTable,
) {
    let dir = workspace_cache_dir(cache_dir, fingerprint);
    let path = file_cache_path(&dir, hash);
    let cached = arena_to_cached(&parsed.arena, &parsed.root_children, table);
    if let Err(error) = serialize_to_file(&cached, &path) {
        tracing::warn!(path = %path.display(), error = %error, "parse cache write failed");
        return;
    }
    let error_path = error_cache_path(&dir, hash);
    if let Err(error) = serialize_errors_to_file(&errors_to_cached(&parsed.errors), &error_path) {
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&error_path);
        tracing::warn!(path = %error_path.display(), error = %error, "parse error cache write failed");
    }
}

fn load_path_inner(
    cache_dir: &Path,
    fingerprint: u64,
    source_path: &Path,
    table: &StringTable,
    load_errors: bool,
) -> Option<(ParsedFile, SourceCacheKey)> {
    if !PATH_METADATA_CACHE_SUPPORTED {
        return None;
    }
    let key = source_cache_key(source_path)?;
    let parsed = load_hash(cache_dir, fingerprint, key.hash, table, load_errors)?;
    (source_cache_key(source_path).as_ref() == Some(&key)).then_some((parsed, key))
}

/// Load a cache entry keyed by the source path, mtime, and size.
pub fn load_path(
    cache_dir: &Path,
    fingerprint: u64,
    source_path: &Path,
    table: &StringTable,
) -> Option<(ParsedFile, SourceCacheKey)> {
    load_path_inner(cache_dir, fingerprint, source_path, table, true)
}

/// Load an indexing-only cache entry without its parse-error sidecar.
pub fn load_path_for_index(
    cache_dir: &Path,
    fingerprint: u64,
    source_path: &Path,
    table: &StringTable,
) -> Option<(ParsedFile, SourceCacheKey)> {
    load_path_inner(cache_dir, fingerprint, source_path, table, false)
}

/// Persist an entry only if the source metadata still matches the snapshot
/// captured before the source was read.
pub fn store_path(
    cache_dir: &Path,
    fingerprint: u64,
    source_path: &Path,
    source_key: &SourceCacheKey,
    parsed: &ParsedFile,
    table: &StringTable,
) {
    if PATH_METADATA_CACHE_SUPPORTED && source_cache_key(source_path).as_ref() == Some(source_key) {
        store_hash(cache_dir, fingerprint, source_key.hash, parsed, table);
    }
}

fn load_inner(
    cache_dir: &Path,
    fingerprint: u64,
    text: &str,
    table: &StringTable,
    load_errors: bool,
) -> Option<ParsedFile> {
    load_hash(
        cache_dir,
        fingerprint,
        content_hash(text),
        table,
        load_errors,
    )
}

/// Load a cache entry keyed by the source text.
pub fn load(
    cache_dir: &Path,
    fingerprint: u64,
    text: &str,
    table: &StringTable,
) -> Option<ParsedFile> {
    load_inner(cache_dir, fingerprint, text, table, true)
}

/// Load an indexing-only cache entry without its parse-error sidecar.
pub fn load_for_index(
    cache_dir: &Path,
    fingerprint: u64,
    text: &str,
    table: &StringTable,
) -> Option<ParsedFile> {
    load_inner(cache_dir, fingerprint, text, table, false)
}

/// Persist an entry keyed by the source text.
pub fn store(
    cache_dir: &Path,
    fingerprint: u64,
    text: &str,
    parsed: &ParsedFile,
    table: &StringTable,
) {
    store_hash(cache_dir, fingerprint, content_hash(text), parsed, table);
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_parser::ast::ParseError;
    use cwtools_parser::parser::parse_string;

    #[test]
    fn content_hash_is_deterministic_and_distinguishes() {
        assert_eq!(content_hash("foo = 1"), content_hash("foo = 1"));
        assert_ne!(content_hash("foo = 1"), content_hash("foo = 2"));
    }

    #[test]
    fn settings_fingerprint_stable_and_sensitive() {
        let root = Path::new("/tmp/ws");
        let base = settings_fingerprint("hoi4", root);
        // Identical inputs -> identical fingerprint.
        assert_eq!(base, settings_fingerprint("hoi4", root));
        // A language/game change must invalidate.
        assert_ne!(base, settings_fingerprint("stellaris", root));
        // A workspace-root change must invalidate.
        assert_ne!(base, settings_fingerprint("hoi4", Path::new("/tmp/other")));
        // So must a display-locale change: the `.cwe` sidecar holds rendered
        // diagnostics, which are written in the locale's language. Driven
        // through `fingerprint` rather than the process-global locale so this
        // stays independent of whatever else is running in the test binary.
        assert_ne!(
            fingerprint("hoi4", root, "en"),
            fingerprint("hoi4", root, "de")
        );
        assert_eq!(base, fingerprint("hoi4", root, "en"));
        let absolute = fs::canonicalize(".").unwrap();
        assert_eq!(
            settings_fingerprint("hoi4", Path::new(".")),
            settings_fingerprint("hoi4", &absolute)
        );
    }

    #[test]
    fn validate_or_clear_first_miss_then_hit() {
        let tmp = tempfile::tempdir().unwrap();
        let fp = 0xdead_beef_u64;
        // No settings.sig yet -> not valid (dir created + sig written).
        assert!(!validate_or_clear(tmp.path(), fp).unwrap());
        // Same fingerprint on the next scan -> valid.
        assert!(validate_or_clear(tmp.path(), fp).unwrap());
    }

    #[test]
    fn validate_or_clear_reports_an_unusable_cache_root() {
        let tmp = tempfile::tempdir().unwrap();
        let blocker = tmp.path().join("not-a-directory");
        fs::write(&blocker, b"x").unwrap();

        assert!(validate_or_clear(&blocker, 1).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn validate_or_clear_refuses_a_symlinked_cache_dir() {
        // A settings change takes the clearing branch, which used to read_dir
        // and remove_file straight through a symlink sitting at the fingerprint
        // path: every regular file in its target went (#159). No race needed.
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("payroll.csv");
        fs::write(&victim, b"keep me").unwrap();
        let dir = workspace_cache_dir(tmp.path(), 42);
        fs::create_dir_all(dir.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(outside.path(), &dir).unwrap();

        assert!(validate_or_clear(tmp.path(), 42).is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"keep me");
        assert!(
            !settings_sig_path(&dir).exists(),
            "a signature was written through the link"
        );
    }

    #[test]
    fn store_then_load_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let table = StringTable::new();
        let fp = 1234;
        validate_or_clear(tmp.path(), fp).unwrap(); // create the dir + sig
        let text = "foo = { bar = 1 baz = \"two\" }\n";
        let parsed = parse_string(text, &table);

        // Miss before anything is stored.
        assert!(load(tmp.path(), fp, text, &table).is_none());

        store(tmp.path(), fp, text, &parsed, &table);

        // Hit after store, with equivalent structure and no errors.
        let loaded = load(tmp.path(), fp, text, &table).expect("expected a cache hit");
        assert_eq!(loaded.root_children.len(), parsed.root_children.len());
        assert!(loaded.errors.is_empty());
    }

    #[test]
    fn an_over_cap_entry_misses_instead_of_failing_the_caller() {
        // The whole degradation contract behind the input bounds (#162): a
        // refused entry has to look like a cold cache, so the caller re-parses
        // the source, rather than surfacing as an error nobody handles.
        let tmp = tempfile::tempdir().unwrap();
        let table = StringTable::new();
        let fp = 162;
        validate_or_clear(tmp.path(), fp).unwrap();
        let text = "a = 1\n";
        let parsed = parse_string(text, &table);
        store(tmp.path(), fp, text, &parsed, &table);
        assert!(load(tmp.path(), fp, text, &table).is_some());

        // Sparsely extended past the read cap, so the entry keeps its valid
        // header and is refused on size alone.
        let dir = workspace_cache_dir(tmp.path(), fp);
        let path = file_cache_path(&dir, content_hash(text));
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(crate::io::MAX_ARCHIVE_FILE_BYTES + 1)
            .unwrap();

        assert!(load(tmp.path(), fp, text, &table).is_none());
    }

    #[test]
    fn load_misses_on_changed_text() {
        let tmp = tempfile::tempdir().unwrap();
        let table = StringTable::new();
        let fp = 99;
        validate_or_clear(tmp.path(), fp).unwrap();
        let text = "a = 1\n";
        let parsed = parse_string(text, &table);
        store(tmp.path(), fp, text, &parsed, &table);
        // Edited content hashes to a different .cwb path -> miss (forces re-parse).
        assert!(load(tmp.path(), fp, "a = 2\n", &table).is_none());
    }

    #[test]
    fn store_preserves_parse_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let table = StringTable::new();
        let fp = 7;
        validate_or_clear(tmp.path(), fp).unwrap();
        let text = "x = 1\n";
        let mut parsed = parse_string(text, &table);
        parsed.errors.push(ParseError::Pos(3, 4, "boom".into()));
        store(tmp.path(), fp, text, &parsed, &table);

        let loaded = load(tmp.path(), fp, text, &table).expect("expected a cache hit");
        assert!(matches!(
            loaded.errors.as_slice(),
            [ParseError::Pos(3, 4, message)] if message == "boom"
        ));

        let dir = workspace_cache_dir(tmp.path(), fp);
        fs::write(error_cache_path(&dir, content_hash(text)), b"broken").unwrap();
        assert!(load(tmp.path(), fp, text, &table).is_none());
    }

    #[test]
    fn index_load_skips_parse_error_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let table = StringTable::new();
        let fp = 7;
        validate_or_clear(tmp.path(), fp).unwrap();
        let text = "x = 1\n";
        let mut parsed = parse_string(text, &table);
        parsed.errors.push(ParseError::Pos(3, 4, "boom".into()));
        store(tmp.path(), fp, text, &parsed, &table);

        let indexed = load_for_index(tmp.path(), fp, text, &table).expect("expected a cache hit");
        assert!(indexed.errors.is_empty());

        let dir = workspace_cache_dir(tmp.path(), fp);
        fs::write(error_cache_path(&dir, content_hash(text)), b"broken").unwrap();
        assert!(load_for_index(tmp.path(), fp, text, &table).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn path_key_hits_until_source_metadata_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source.txt");
        fs::write(&source, "a = 1\n").unwrap();
        let table = StringTable::new();
        let fp = 8;
        validate_or_clear(tmp.path(), fp).unwrap();
        let parsed = parse_string("a = 1\n", &table);

        let source_key = source_cache_key(&source).unwrap();
        store_path(tmp.path(), fp, &source, &source_key, &parsed, &table);
        assert!(load_path(tmp.path(), fp, &source, &table).is_some());

        fs::write(&source, "a = 200\n").unwrap();
        assert!(load_path(tmp.path(), fp, &source, &table).is_none());
    }

    /// Off `PATH_METADATA_CACHE_SUPPORTED` there is no reliable no-read stamp, so
    /// a store must not produce a hit: the caller re-reads and keys on content
    /// instead of being served an entry nothing can invalidate.
    #[cfg(not(unix))]
    #[test]
    fn path_key_never_hits_without_metadata_support() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source.txt");
        fs::write(&source, "a = 1\n").unwrap();
        let table = StringTable::new();
        let fp = 8;
        validate_or_clear(tmp.path(), fp).unwrap();
        let parsed = parse_string("a = 1\n", &table);

        let source_key = source_cache_key(&source).unwrap();
        store_path(tmp.path(), fp, &source, &source_key, &parsed, &table);
        assert!(load_path(tmp.path(), fp, &source, &table).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn path_key_detects_same_size_edit_with_restored_mtime() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source.txt");
        fs::write(&source, "a = 1\n").unwrap();
        let modified = source.metadata().unwrap().modified().unwrap();
        let table = StringTable::new();
        let fp = 9;
        validate_or_clear(tmp.path(), fp).unwrap();
        let parsed = parse_string("a = 1\n", &table);
        let source_key = source_cache_key(&source).unwrap();
        store_path(tmp.path(), fp, &source, &source_key, &parsed, &table);

        fs::write(&source, "a = 2\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&source)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert!(load_path(tmp.path(), fp, &source, &table).is_none());
    }

    #[test]
    fn path_store_skips_a_source_changed_during_parse() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source.txt");
        fs::write(&source, "a = 1\n").unwrap();
        let table = StringTable::new();
        let fp = 10;
        validate_or_clear(tmp.path(), fp).unwrap();
        let source_key = source_cache_key(&source).unwrap();
        let parsed = parse_string("a = 1\n", &table);

        fs::write(&source, "a = 200\n").unwrap();
        store_path(tmp.path(), fp, &source, &source_key, &parsed, &table);
        assert!(load_hash(tmp.path(), fp, source_key.hash, &table, true).is_none());
    }

    /// Cold (parse + store) vs warm (deserialize) over the real Millennium Dawn
    /// corpus. The cache only earns its keep if `load` beats `parse_string`.
    ///
    /// Ignored by default (needs the MD mod on disk + is slow). Run with:
    ///   cargo test -p cwtools_cache --lib -- \
    ///     --ignored --nocapture bench_parse_cache_vs_parse
    #[test]
    #[ignore]
    fn bench_parse_cache_vs_parse() {
        use std::time::Instant;

        let root = std::path::PathBuf::from(std::env::var("CWTOOLS_MD_DIR").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{home}/Documents/github-projects/Millennium-Dawn")
        }));
        let root = root.as_path();
        if !root.exists() {
            eprintln!("SKIP: {} not present", root.display());
            return;
        }
        let mut files = Vec::new();
        for sub in ["common", "events", "history"] {
            collect_txt(&root.join(sub), &mut files);
        }
        eprintln!("corpus: {} .txt files", files.len());

        let table = StringTable::new();
        let tmp = tempfile::tempdir().unwrap();
        let fp = 0xabc;
        validate_or_clear(tmp.path(), fp).unwrap();

        // Cold pass: parse + persist.
        let t0 = Instant::now();
        let mut parsed_ok = 0usize;
        for path in &files {
            let Some(source_key) = source_cache_key(path) else {
                continue;
            };
            if let Ok(text) = std::fs::read_to_string(path) {
                let parsed = parse_string(&text, &table);
                store_path(tmp.path(), fp, path, &source_key, &parsed, &table);
                parsed_ok += 1;
            }
        }
        let cold = t0.elapsed();

        // Warm pass: deserialize from cache.
        let t1 = Instant::now();
        let mut hits = 0usize;
        for path in &files {
            if load_path(tmp.path(), fp, path, &table).is_some() {
                hits += 1;
            }
        }
        let warm = t1.elapsed();

        eprintln!(
            "cold parse+store: {:.3}s ({} parsed)\nwarm load:        {:.3}s ({} hits)\nspeedup: {:.2}x",
            cold.as_secs_f64(),
            parsed_ok,
            warm.as_secs_f64(),
            hits,
            cold.as_secs_f64() / warm.as_secs_f64().max(1e-9),
        );
        assert_eq!(hits, parsed_ok, "every stored file should hit when warm");
    }

    fn collect_txt(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_txt(&p, out);
            } else if p.extension().is_some_and(|e| e == "txt") {
                out.push(p);
            }
        }
    }

    #[test]
    fn prune_cache_dir_evicts_oldest_over_entry_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let max_entries = 10usize;
        // Write more `.cwb` files than the (small) cap, staggered mtimes.
        let n = max_entries + 5;
        let base = std::time::SystemTime::now() - std::time::Duration::from_secs(n as u64 + 10);
        let mut paths = Vec::with_capacity(n);
        for i in 0..n {
            let p = dir.join(format!("{:016x}.cwb", i as u64));
            fs::write(&p, b"x").unwrap();
            filetime_set(&p, base + std::time::Duration::from_secs(i as u64));
            paths.push(p);
        }
        assert_eq!(count_cwb(dir), n);
        prune_cache_dir_with_caps(dir, max_entries, u64::MAX);
        let remaining = count_cwb(dir);
        // Pruned down to ~80% of the cap.
        let target = (max_entries as f64 * PRUNE_TARGET_RATIO) as usize;
        assert!(
            remaining <= target + 1,
            "pruned to {remaining}, want ~{target}"
        );
        // The oldest entries are gone; the newest survives.
        assert!(paths.last().unwrap().exists(), "newest entry was evicted");
        assert!(!paths.first().unwrap().exists(), "oldest entry survived");
    }

    #[test]
    fn global_prune_bounds_entries_across_fingerprints() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::time::SystemTime::now() - std::time::Duration::from_secs(20);
        let mut paths = Vec::new();
        for fingerprint in [1u64, 2, 3] {
            // A real cache dir, signature and all: the sweep only touches dirs
            // it can identify as ours (#159).
            let dir = workspace_cache_dir(tmp.path(), fingerprint);
            fs::create_dir_all(&dir).unwrap();
            fs::write(settings_sig_path(&dir), fingerprint.to_le_bytes()).unwrap();
            for i in 0..4u64 {
                let path = dir.join(format!("{fingerprint}-{i}.cwb"));
                fs::write(&path, b"x").unwrap();
                filetime_set(
                    &path,
                    base + std::time::Duration::from_secs(paths.len() as u64),
                );
                paths.push(path);
            }
        }

        prune_all_cache_dirs_with_caps(tmp.path(), 5, u64::MAX, None);
        let remaining: usize = [1u64, 2, 3]
            .iter()
            .map(|fingerprint| count_cwb(&workspace_cache_dir(tmp.path(), *fingerprint)))
            .sum();
        assert!(remaining <= 4);
        assert!(paths.last().unwrap().exists());
        assert!(!paths.first().unwrap().exists());
    }

    // ── Ownership-checked removal (#159) ────────────────────────────────────

    /// A per-fingerprint cache dir in the shape `validate_or_clear` + `store`
    /// leave it: the 8-byte `settings.sig` plus one entry.
    fn owned_cache_dir(root: &Path, fingerprint: u64) -> PathBuf {
        let dir = workspace_cache_dir(root, fingerprint);
        fs::create_dir_all(&dir).unwrap();
        fs::write(settings_sig_path(&dir), fingerprint.to_le_bytes()).unwrap();
        fs::write(file_cache_path(&dir, 1), b"cwb").unwrap();
        dir
    }

    #[test]
    fn remove_all_clears_owned_cache_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = owned_cache_dir(tmp.path(), 0xdead_beef);

        let removal = remove_all(tmp.path());

        assert!(removal.failures.is_empty(), "{:?}", removal.failures);
        assert_eq!(removal.files, 2, "settings.sig + one entry");
        assert!(!dir.exists());
        assert!(!tmp.path().join(PARSE_CACHE_DIR).exists());
    }

    #[test]
    fn remove_all_leaves_a_foreign_parse_cache_directory() {
        // The cache root is client input (#159), so a directory that merely
        // happens to be named `parse-cache` must come through untouched.
        let tmp = tempfile::tempdir().unwrap();
        let foreign = tmp.path().join(PARSE_CACHE_DIR).join("important");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("notes.txt"), b"keep me").unwrap();
        // A fingerprint-shaped name is not ownership on its own, and neither is
        // a `settings.sig` of the wrong size.
        let lookalike = tmp.path().join(PARSE_CACHE_DIR).join("deadbeefdeadbeef");
        fs::create_dir_all(&lookalike).unwrap();
        fs::write(settings_sig_path(&lookalike), b"no").unwrap();
        fs::write(lookalike.join("payroll.csv"), b"keep me").unwrap();

        let removal = remove_all(tmp.path());

        assert!(removal.failures.is_empty(), "{:?}", removal.failures);
        assert_eq!(removal.files, 0);
        assert!(foreign.join("notes.txt").exists());
        assert!(lookalike.join("payroll.csv").exists());
    }

    #[test]
    fn remove_all_leaves_an_empty_foreign_parse_cache_directory() {
        // Removing the root is only earned by having cleared something out of
        // it: an empty directory we never wrote to is not ours to delete.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(PARSE_CACHE_DIR);
        fs::create_dir_all(&root).unwrap();

        let removal = remove_all(tmp.path());

        assert!(removal.failures.is_empty(), "{:?}", removal.failures);
        assert!(root.exists());
    }

    #[test]
    fn remove_all_reports_a_parse_cache_it_cannot_use() {
        // "Caches cleared (0 files)" when the root is unusable reads as success
        // and isn't: the caller has to hear about it.
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(PARSE_CACHE_DIR), b"not a cache").unwrap();

        let removal = remove_all(tmp.path());

        assert_eq!(removal.files, 0);
        assert_eq!(removal.failures.len(), 1, "{:?}", removal.failures);
    }

    #[cfg(unix)]
    #[test]
    fn remove_all_does_not_follow_a_symlinked_cache_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = owned_cache_dir(outside.path(), 1);
        fs::create_dir_all(tmp.path().join(PARSE_CACHE_DIR)).unwrap();
        std::os::unix::fs::symlink(
            &target,
            tmp.path().join(PARSE_CACHE_DIR).join("0123456789abcdef"),
        )
        .unwrap();

        let removal = remove_all(tmp.path());

        assert_eq!(removal.files, 0);
        assert!(removal.failures.is_empty(), "{:?}", removal.failures);
        assert!(settings_sig_path(&target).exists(), "target was followed");
    }

    #[test]
    fn global_prune_leaves_a_foreign_directory_under_parse_cache() {
        // Regression for #159: the empty-dir sweep used to delete every file in
        // any `parse-cache/<x>/` holding no `.cwb`, whatever `<x>` was — and it
        // runs on every scan, no command needed.
        let tmp = tempfile::tempdir().unwrap();
        let foreign = tmp.path().join(PARSE_CACHE_DIR).join("Documents");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("taxes.pdf"), b"keep me").unwrap();
        let owned = owned_cache_dir(tmp.path(), 7);
        fs::remove_file(file_cache_path(&owned, 1)).unwrap();

        prune_all_cache_dirs_with_caps(tmp.path(), 5, u64::MAX, None);

        assert!(foreign.join("taxes.pdf").exists(), "foreign file deleted");
        assert!(!owned.exists(), "an empty owned cache dir is still swept");
    }

    #[test]
    fn prune_cache_dir_noop_under_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for i in 0..10u64 {
            fs::write(dir.join(format!("{:016x}.cwb", i)), b"x").unwrap();
        }
        prune_cache_dir_with_caps(dir, 50, u64::MAX);
        assert_eq!(count_cwb(dir), 10);
    }

    fn count_cwb(dir: &Path) -> usize {
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "cwb"))
            .count()
    }

    fn filetime_set(path: &Path, t: std::time::SystemTime) {
        // Set mtime via `File::set_modified` (stable since 1.75), no extra crate.
        let f = fs::File::options().write(true).open(path).unwrap();
        f.set_modified(t).unwrap();
    }

    #[test]
    fn different_fingerprint_uses_separate_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let table = StringTable::new();
        let text = "k = 1\n";
        let parsed = parse_string(text, &table);
        validate_or_clear(tmp.path(), 1).unwrap();
        store(tmp.path(), 1, text, &parsed, &table);
        validate_or_clear(tmp.path(), 2).unwrap();
        // Same text, different settings fingerprint -> different dir -> miss.
        assert!(load(tmp.path(), 2, text, &table).is_none());
        // Initializing another workspace must not delete the original cache.
        assert!(load(tmp.path(), 1, text, &table).is_some());
    }

    #[test]
    fn concurrent_validate_or_clear_shares_a_cache_root() {
        // `cargo test --workspace` shares one SCRATCH_HOME across parallel
        // test binaries; two `validate_or_clear` racing on the same root must
        // not leave it unusable (#159 follow-up, Windows/macOS flake).
        // Empty dirs are pruned, so we only assert no hard error and that a
        // fresh fingerprint can still be created afterwards.
        use std::sync::Arc;
        let tmp = tempfile::tempdir().unwrap();
        let cache_root = Arc::new(tmp.path().to_path_buf());
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let root = Arc::clone(&cache_root);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    validate_or_clear(&root, 0x1000 + i).is_ok()
                })
            })
            .collect();
        for h in handles {
            assert!(h.join().unwrap(), "concurrent validate_or_clear failed");
        }
        // A fresh fingerprint must still be creatable after the race.
        assert!(!validate_or_clear(cache_root.as_path(), 0x9fff).unwrap_or(true));
        assert!(validate_or_clear(cache_root.as_path(), 0x9fff).unwrap());
    }
}
