use cwtools_parser::ast::{Arena, Child, ParseError};
use cwtools_parser::parser::{parse_string, parse_string_without_comments};
use cwtools_string_table::string_table::StringTable;
use std::path::{Path, PathBuf};
use thiserror::Error;

// ── Encoding helper ───────────────────────────────────────────────────────────

/// Windows-1252 → Unicode mapping for the 0x80-0x9F range (the gap not covered
/// by ISO-8859-1).  Index 0 = byte 0x80, index 31 = byte 0x9F.
///
/// Source: https://encoding.spec.whatwg.org/index-windows-1252.txt
const CP1252_HIGH: [char; 32] = [
    '\u{20AC}', // 0x80 €
    '\u{FFFD}', // 0x81 (undefined → replacement char)
    '\u{201A}', // 0x82 ‚
    '\u{0192}', // 0x83 ƒ
    '\u{201E}', // 0x84 „
    '\u{2026}', // 0x85 …
    '\u{2020}', // 0x86 †
    '\u{2021}', // 0x87 ‡
    '\u{02C6}', // 0x88 ˆ
    '\u{2030}', // 0x89 ‰
    '\u{0160}', // 0x8A Š
    '\u{2039}', // 0x8B ‹
    '\u{0152}', // 0x8C Œ
    '\u{FFFD}', // 0x8D (undefined)
    '\u{017D}', // 0x8E Ž
    '\u{FFFD}', // 0x8F (undefined)
    '\u{FFFD}', // 0x90 (undefined)
    '\u{2018}', // 0x91 '
    '\u{2019}', // 0x92 '
    '\u{201C}', // 0x93 "
    '\u{201D}', // 0x94 "
    '\u{2022}', // 0x95 •
    '\u{2013}', // 0x96 –
    '\u{2014}', // 0x97 —
    '\u{02DC}', // 0x98 ˜
    '\u{2122}', // 0x99 ™
    '\u{0161}', // 0x9A š
    '\u{203A}', // 0x9B ›
    '\u{0153}', // 0x9C œ
    '\u{FFFD}', // 0x9D (undefined)
    '\u{017E}', // 0x9E ž
    '\u{0178}', // 0x9F Ÿ
];

/// Decode a single byte as Windows-1252.
#[inline]
fn cp1252_byte(b: u8) -> char {
    if b < 0x80 {
        b as char
    } else if b <= 0x9F {
        CP1252_HIGH[(b - 0x80) as usize]
    } else {
        // 0xA0-0xFF: identical to Latin-1 / Unicode
        b as char
    }
}

/// How a file was encoded on disk, detected while reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEncoding {
    /// Valid UTF-8 starting with the UTF-8 BOM (`EF BB BF`). What Paradox wants
    /// for localisation files.
    Utf8Bom,
    /// Valid UTF-8 but with no BOM.
    Utf8NoBom,
    /// Not valid UTF-8 (decoded via Windows-1252 fallback).
    NonUtf8,
}

const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

/// Read a file as text: try UTF-8 first, fall back to Windows-1252.
///
/// Pre-Jomini games (CK2, EU4, VIC2, HOI4 old mods) often encode files in
/// Windows-1252.  Blindly using `read_to_string` fails on any accented byte
/// outside ASCII (e.g. `é` = 0xE9).  This helper avoids that breakage.
pub fn read_text(path: &Path) -> Result<String, FileError> {
    read_text_with_encoding(path).map(|(s, _)| s)
}

/// As [`read_text`], but also reports how the file was encoded so callers can
/// enforce encoding rules (e.g. localisation must be UTF-8 BOM).
pub fn read_text_with_encoding(path: &Path) -> Result<(String, FileEncoding), FileError> {
    Ok(decode_bytes(std::fs::read(path)?))
}

/// Read a file as text through a hard byte cap. Opens once and reads at most
/// `max_bytes`; a file that reports a larger length, grows under us, or is a
/// special file that reports length 0 is refused rather than truncated (a
/// truncated script would parse into garbage). Returns the raw byte count read
/// so callers can enforce a per-scan total-byte budget.
///
/// This is the bounded read used by every bulk discovery/scan path. The
/// unbounded [`read_text`] stays for single-file explicit reads (a file the
/// user opened by name), which aren't fed by symlink-following discovery.
pub fn read_text_capped(path: &Path, max_bytes: u64) -> Result<(String, u64), FileError> {
    read_text_capped_with_encoding(path, max_bytes).map(|(s, _, n)| (s, n))
}

/// Like [`read_text_capped`], but also reports the detected encoding.
pub fn read_text_capped_with_encoding(
    path: &Path,
    max_bytes: u64,
) -> Result<(String, FileEncoding, u64), FileError> {
    use std::io::Read as _;
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    // `take` bounds the allocation as well as the read, so a file that grows
    // under us or misreports its length still can't outrun the cap.
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let n = bytes.len() as u64;
    if n > max_bytes {
        return Err(FileError::OverLimit {
            path: path.to_path_buf(),
            limit: max_bytes,
        });
    }
    let (text, enc) = decode_bytes(bytes);
    Ok((text, enc, n))
}

/// Decode already-read file bytes by the same rules as [`read_text`], for
/// callers that must own the read itself (the LSP's URI access boundary reads
/// through a cap) but still need script files to decode identically.
pub fn decode_bytes(bytes: Vec<u8>) -> (String, FileEncoding) {
    let has_bom = bytes.starts_with(&UTF8_BOM);
    // Fast path: valid UTF-8 (includes pure ASCII). The BOM, when present, is
    // valid UTF-8 (U+FEFF) and is kept in the string — existing parsers already
    // tolerate a leading BOM character.
    let bytes = match String::from_utf8(bytes) {
        Ok(s) => {
            let enc = if has_bom {
                FileEncoding::Utf8Bom
            } else {
                FileEncoding::Utf8NoBom
            };
            return (s, enc);
        }
        Err(e) => e.into_bytes(),
    };
    // Not valid UTF-8: strip a leading BOM if any, then decode as Windows-1252.
    let body = if has_bom { &bytes[3..] } else { &bytes[..] };
    let text = body.iter().map(|&b| cp1252_byte(b)).collect();
    (text, FileEncoding::NonUtf8)
}

/// How the file should be treated during discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileKind {
    /// Paradox script (.txt / .gui / .gfx) — parsed into an AST.
    Script,
    /// Localisation (.yml / .csv) — not script-parsed, stored separately.
    Localisation,
    /// Binary / asset file (.dds, .png, .tga, .wav, .lua, .mesh, .shader, etc.)
    /// — existence is noted but the file is not read.
    Resource,
}

/// File extensions treated as Paradox script (the set discovered and validated).
/// The single source of truth for "what's a script file" — workspace discovery
/// in the LSP and the CLI driver both filter by this list.
pub const SCRIPT_EXTENSIONS: &[&str] = &["txt", "gui", "gfx", "sfx", "asset", "map"];

/// True for a localisation file extension (case-insensitive): `yml`, `yaml`,
/// `csv`. The single source of truth so script discovery, the localisation
/// walker, and the LSP's loc-file predicate all agree — previously the loc
/// walker missed `.yaml`.
pub fn is_loc_ext(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("yml")
        || ext.eq_ignore_ascii_case("yaml")
        || ext.eq_ignore_ascii_case("csv")
}

/// Classify a file by its extension, matching F# FileManager.fs:215-273.
pub(crate) fn classify_extension(path: &Path) -> FileKind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if SCRIPT_EXTENSIONS.contains(&ext.as_str()) {
        FileKind::Script
    } else if is_loc_ext(&ext) {
        FileKind::Localisation
    } else {
        FileKind::Resource
    }
}

/// Directory names skipped everywhere during discovery: VCS, build output, and
/// editor/tooling dirs that never hold game content (walking them double-counts
/// files, e.g. a `.claude` worktree mirroring the whole mod tree). The single
/// source of truth, shared with the localisation walker via [`is_excluded_dir`].
pub const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".claude",
    "target",
    ".vs",
    "node_modules",
    "out",
    "dist",
    "bin",
    "obj",
    ".idea",
    ".vscode",
];

/// Directory names skipped ONLY at the workspace root. A top-level `resources/`
/// is dev scratch the game never loads, but nested `common/resources/` defines
/// the `resource` type (oil, steel, …) and must be indexed. Shared via
/// [`is_excluded_root_dir`].
pub const EXCLUDED_ROOT_DIRS: &[&str] = &["resources"];

/// True for a directory name skipped everywhere during discovery (see
/// [`EXCLUDED_DIRS`]). Case-insensitive.
pub fn is_excluded_dir(name: &str) -> bool {
    EXCLUDED_DIRS.iter().any(|d| name.eq_ignore_ascii_case(d))
}

/// True for a directory name skipped only at the workspace root (see
/// [`EXCLUDED_ROOT_DIRS`]). Case-insensitive.
pub fn is_excluded_root_dir(name: &str) -> bool {
    EXCLUDED_ROOT_DIRS
        .iter()
        .any(|d| name.eq_ignore_ascii_case(d))
}

/// True if `path` is a script file the discovery filters accept: a script
/// extension, matching an include pattern, not on the exclude list, and within
/// the size guard. Shared by single-root discovery ([`FileManager::collect_paths`])
/// and the multi-mod path so both apply identical file-level rules.
///
/// `relative` is the file's root-relative path, for the exclude patterns that
/// address a location rather than a name (see [`ignore_glob_match`]).
fn accept_script_file(cfg: &FileManagerConfig, path: &Path, relative: &str) -> bool {
    if classify_extension(path) != FileKind::Script {
        return false;
    }
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if !cfg
        .file_patterns
        .iter()
        .any(|pat| glob_match(pat, file_name))
    {
        return false;
    }
    if cfg
        .exclude_patterns
        .iter()
        .any(|pat| ignore_glob_match(pat, file_name, relative))
    {
        return false;
    }
    if cfg.max_file_size > 0
        && let Ok(meta) = path.metadata()
        && meta.len() > cfg.max_file_size
    {
        return false;
    }
    true
}

#[derive(Debug, Error)]
pub enum FileError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// The configured root isn't a directory. Distinct from an empty walk: a
    /// path that doesn't resolve must not read as "this mod has no files".
    #[error("directory does not exist: {0}")]
    MissingRoot(PathBuf),
    /// A file exceeded the hard read cap for a scan. Distinct from a parse
    /// error: the file was refused before reading, so a special file that
    /// reports length 0 (e.g. `/dev/zero`) can't be read to EOF.
    #[error("file exceeds the {limit} byte read cap: {path}")]
    OverLimit { path: PathBuf, limit: u64 },
}

/// A discovered script file before its source is read or parsed.
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub path: PathBuf,
    pub logical_path: String,
}

/// A discovered script file with its parsed AST.
pub struct ParsedFile {
    /// Absolute path on disk.
    pub path: PathBuf,
    /// Game-relative logical path (e.g. `common/scripted_effects/foo.txt`).
    pub logical_path: String,
    pub arena: Arena,
    pub root_children: Vec<Child>,
    /// Non-fatal parse errors (file was partially parsed; validate what survived).
    pub errors: Vec<ParseError>,
}

/// Paradox `.mod` descriptor fields.
#[derive(Debug, Clone)]
pub struct ModDescriptor {
    pub name: String,
    pub path: Option<String>,
    pub replace_paths: Vec<String>,
}

/// Directory classification mirroring F# `DirectoryType`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryType {
    Vanilla,
    Mod,
    MultipleMod,
    Unknown,
}

/// Configuration for file discovery.
pub struct FileManagerConfig {
    /// Root directory to search.
    pub root: PathBuf,
    /// Subdirectories to include (e.g., "common", "events").
    pub include_dirs: Vec<String>,
    /// Glob patterns for files (e.g., "*.txt").
    pub file_patterns: Vec<String>,
    /// Patterns to exclude, matched by [`ignore_glob_match`]: a bare name glob
    /// (`*.md`) matches the file name at any depth, one with a separator
    /// (`gfx/**/*.txt`) matches the root-relative path.
    pub exclude_patterns: Vec<String>,
    /// Directory names to skip entirely (exact, case-insensitive).
    pub exclude_dirs: Vec<String>,
    /// Directory glob patterns to skip entirely. Like `exclude_dirs` but each
    /// entry is a glob, matched by [`ignore_glob_match`] against the directory's
    /// basename or, when it carries a separator, its root-relative path.
    /// Layers on top of `exclude_dirs` — both lists are checked.
    pub exclude_dir_patterns: Vec<String>,
    /// Directory names skipped ONLY at the workspace root (exact, case-insensitive).
    /// Use for names that are dev-scratch at the top level but a real game folder
    /// when nested — e.g. a root `resources/` is scratch, but `common/resources/`
    /// defines the `resource` type (oil, steel, …) and must be indexed.
    pub exclude_root_dirs: Vec<String>,
    /// Skip files larger than this (bytes). 0 = no limit.
    pub max_file_size: u64,
    /// Per-scan resource budget (file count and total bytes).
    pub scan_budget: ScanBudget,
}

/// Per-scan resource budget. Guards one discovery/read pass against a
/// pathological tree (a symlink to `/`, a special file that reports length 0,
/// or a huge number of files) so startup and CLI validation can't allocate
/// without bound. A limit of 0 disables it.
#[derive(Debug, Clone, Copy)]
pub struct ScanBudget {
    /// Maximum number of files accepted by one discovery walk.
    pub max_files: usize,
    /// Maximum total bytes read across all files in one scan.
    pub max_bytes: u64,
    /// Hard per-file read cap (bytes). 0 = no per-file cap.
    pub max_file_size: u64,
}

impl Default for ScanBudget {
    fn default() -> Self {
        Self {
            max_files: 100_000,
            max_bytes: 2 * 1024 * 1024 * 1024, // 2 GiB
            // Loc and rules files can legitimately run to a few MB; the CLI's
            // script cap (2 MB) is separate (`FileManagerConfig::max_file_size`).
            max_file_size: 64 * 1024 * 1024, // 64 MB
        }
    }
}

/// Atomic running total of bytes read in one scan, shared across the parallel
/// read fan-out so the whole scan stops adding files once the total-byte budget
/// is exhausted.
#[derive(Debug, Default)]
pub struct ScanBytes(std::sync::atomic::AtomicU64);

impl ScanBytes {
    pub fn new() -> Self {
        Self(std::sync::atomic::AtomicU64::new(0))
    }

    /// Reserve `n` bytes against `max_bytes` (0 = unlimited). Returns true if
    /// the reservation is accepted, false if it would exceed the budget.
    pub fn try_reserve(&self, n: u64, max_bytes: u64) -> bool {
        use std::sync::atomic::Ordering;
        if max_bytes == 0 {
            return true;
        }
        let mut cur = self.0.load(Ordering::Relaxed);
        loop {
            if cur.saturating_add(n) > max_bytes {
                return false;
            }
            match self
                .0
                .compare_exchange_weak(cur, cur + n, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return true,
                Err(actual) => cur = actual,
            }
        }
    }
}

impl Default for FileManagerConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            include_dirs: vec![
                "common".into(),
                "events".into(),
                "history".into(),
                "gfx".into(),
                "interface".into(),
                "decisions".into(),
                "missions".into(),
                "sound".into(),
                "music".into(),
            ],
            file_patterns: SCRIPT_EXTENSIONS.iter().map(|e| format!("*.{e}")).collect(),
            exclude_patterns: vec![
                // Free-form text/markdown files that aren't Paradox script —
                // matching `*.txt` would otherwise send them through the full
                // validator. Users can opt back in by clearing the list.
                "Changelog.txt".into(),
                "README.txt".into(),
                "LICENSE.txt".into(),
                "README.md".into(),
                "LICENSE.md".into(),
                "*.md".into(),
            ],
            exclude_dirs: EXCLUDED_DIRS.iter().map(|s| s.to_string()).collect(),
            exclude_dir_patterns: vec![],
            exclude_root_dirs: EXCLUDED_ROOT_DIRS.iter().map(|s| s.to_string()).collect(),
            max_file_size: 2 * 1024 * 1024, // 2 MB
            scan_budget: ScanBudget::default(),
        }
    }
}

pub struct FileManager {
    pub config: FileManagerConfig,
    pub string_table: StringTable,
}

impl FileManager {
    pub fn new(config: FileManagerConfig) -> Self {
        Self {
            config,
            string_table: StringTable::new(),
        }
    }

    pub fn with_string_table(config: FileManagerConfig, table: StringTable) -> Self {
        Self {
            config,
            string_table: table,
        }
    }

    /// Discover all matching script files under the configured root without
    /// reading or parsing them. Non-script files are silently skipped.
    pub fn discover_files(&self) -> Result<Vec<DiscoveredFile>, FileError> {
        let mut paths: Vec<(PathBuf, String)> = Vec::new();
        let root = &self.config.root;
        if !root.is_dir() {
            return Err(FileError::MissingRoot(root.clone()));
        }

        for include_dir in &self.config.include_dirs {
            let dir = if include_dir == "." {
                root.clone()
            } else {
                root.join(include_dir)
            };
            if !dir.exists() {
                continue;
            }
            self.collect_paths(&dir, &mut paths)?;
        }

        Ok(paths
            .into_iter()
            .map(|(path, logical_path)| DiscoveredFile { path, logical_path })
            .collect())
    }

    /// Discover and parse all matching script files under the configured root.
    /// Non-script files (localisation, resources) are silently skipped.
    pub fn discover_and_parse(&mut self) -> Result<Vec<ParsedFile>, FileError> {
        use rayon::prelude::*;

        let table = &self.string_table;
        let max_file_size = self.config.max_file_size;
        let max_bytes = self.config.scan_budget.max_bytes;
        let bytes = ScanBytes::new();
        let files = self
            .discover_files()?
            .into_par_iter()
            .filter_map(|file| {
                let content = match read_text_capped(&file.path, max_file_size) {
                    Ok((content, n)) => {
                        if !bytes.try_reserve(n, max_bytes) {
                            eprintln!(
                                "warn: skipping {}: scan byte budget exceeded",
                                file.path.display()
                            );
                            return None;
                        }
                        content
                    }
                    Err(e) => {
                        eprintln!("warn: skipping {}: {}", file.path.display(), e);
                        return None;
                    }
                };
                let parsed = parse_string_without_comments(&content, table);
                Some(ParsedFile {
                    path: file.path,
                    logical_path: file.logical_path,
                    arena: parsed.arena,
                    root_children: parsed.root_children,
                    errors: parsed.errors,
                })
            })
            .collect();

        Ok(files)
    }

    /// Walk `dir` collecting (path, logical_path) for every file that passes the
    /// extension/pattern/size filters. Reading and parsing happen later, in
    /// parallel; this pass is just filesystem traversal.
    fn collect_paths(&self, dir: &Path, out: &mut Vec<(PathBuf, String)>) -> Result<(), FileError> {
        let root_prefix = normalize_root_prefix(&self.config.root);
        let is_root_level = dir == self.config.root.as_path();
        let cfg = &self.config;
        // Accept only script files that pass the shared file-level filter; each
        // yields (path, logical_path). The extension test comes first here: it
        // rejects most of a mod's tree (art, sound) before the logical path an
        // exclude pattern may match against is built.
        let mut accept = |path: &Path| -> Option<(PathBuf, String)> {
            if classify_extension(path) != FileKind::Script {
                return None;
            }
            let logical_path = compute_logical_path_with_root(path, &root_prefix);
            if !accept_script_file(cfg, path, &logical_path) {
                return None;
            }
            Some((path.to_path_buf(), logical_path))
        };
        let mut on_err = |path: &Path, e: std::io::Error| {
            eprintln!("warn: skipping {}: {}", path.display(), FileError::from(e));
        };
        let mut state = WalkState {
            out: Vec::new(),
            remaining_files: self.config.scan_budget.max_files,
        };
        walk_dir_generic(
            dir,
            WalkRoot {
                prefix: &root_prefix,
                is_root_level,
            },
            cfg,
            &[],
            &mut accept,
            &mut on_err,
            &mut state,
        )
        .map_err(FileError::from)?;
        out.extend(state.out);
        Ok(())
    }

    /// Discover and parse the script files of a `MultipleMod` workspace: a
    /// directory whose `mod/` (or `mods/`) folder holds `.mod` descriptors, each
    /// pointing at a mod root ([`classify_directory`] returned `MultipleMod`).
    ///
    /// The mods are layered by [`discover_files_multi_mod`]'s load order — a
    /// later-resolved mod (mods are name-sorted, so the alphabetically-greater
    /// name) wins a shared logical path, and a mod's `replace_path` suppresses
    /// lower-priority files under that prefix. The surviving script files use
    /// the same filters as [`Self::discover_files`]. Vanilla is not folded in;
    /// the driver indexes the base game separately for reference only.
    pub fn discover_files_multi_mod(&self) -> Vec<DiscoveredFile> {
        let mods = expand_multiple_mods(&self.config.root);
        discover_files_multi_mod(
            None,
            &mods,
            &self.config.include_dirs,
            self.config.scan_budget,
        )
        .into_iter()
        .filter(|(path, logical_path)| accept_script_file(&self.config, path, logical_path))
        .map(|(path, logical_path)| DiscoveredFile { path, logical_path })
        .collect()
    }

    /// Discover and parse the script files of a `MultipleMod` workspace.
    pub fn discover_and_parse_multi_mod(&mut self) -> Result<Vec<ParsedFile>, FileError> {
        use rayon::prelude::*;

        let table = &self.string_table;
        let max_file_size = self.config.max_file_size;
        let max_bytes = self.config.scan_budget.max_bytes;
        let bytes = ScanBytes::new();
        let files = self
            .discover_files_multi_mod()
            .into_par_iter()
            .filter_map(|file| {
                let content = match read_text_capped(&file.path, max_file_size) {
                    Ok((content, n)) => {
                        if !bytes.try_reserve(n, max_bytes) {
                            eprintln!(
                                "warn: skipping {}: scan byte budget exceeded",
                                file.path.display()
                            );
                            return None;
                        }
                        content
                    }
                    Err(e) => {
                        eprintln!("warn: skipping {}: {}", file.path.display(), e);
                        return None;
                    }
                };
                let parsed = parse_string_without_comments(&content, table);
                Some(ParsedFile {
                    path: file.path,
                    logical_path: file.logical_path,
                    arena: parsed.arena,
                    root_children: parsed.root_children,
                    errors: parsed.errors,
                })
            })
            .collect();

        Ok(files)
    }

    pub fn parse_single_file(&mut self, path: &Path) -> Result<ParsedFile, FileError> {
        let content = read_text(path)?;
        let logical_path = compute_logical_path(path, &self.config.root);
        let parsed = parse_string(&content, &self.string_table);
        Ok(ParsedFile {
            path: path.to_path_buf(),
            logical_path,
            arena: parsed.arena,
            root_children: parsed.root_children,
            errors: parsed.errors,
        })
    }
}

/// Compute the logical (game-relative) path by stripping the root prefix.
///
/// Given `root = /mnt/mod` and `path = /mnt/mod/common/effects/foo.txt`,
/// returns `common/effects/foo.txt`.
pub(crate) fn compute_logical_path(path: &Path, root: &Path) -> String {
    compute_logical_path_with_root(path, &normalize_root_prefix(root))
}

/// Normalise `root` to a forward-slash, trailing-slash prefix once, so callers
/// that strip many paths against the same root don't redo the work per file.
fn normalize_root_prefix(root: &Path) -> String {
    let s = normalize_slashes(root.to_string_lossy());
    if s.ends_with('/') {
        s.into_owned()
    } else {
        format!("{}/", s)
    }
}

/// Like [`compute_logical_path`] but takes a root prefix already normalised by
/// [`normalize_root_prefix`].
fn compute_logical_path_with_root(path: &Path, root_prefix: &str) -> String {
    let path_str = normalize_slashes(path.to_string_lossy());

    if let Some(rel) = path_str.strip_prefix(root_prefix) {
        rel.to_string()
    } else {
        // fallback: just the file name
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string()
    }
}

/// The forward-slash spelling of `path`, the form a path glob is written in.
/// Windows hands back `\` separators, so a match target passes through here
/// before [`ignore_glob_match`] sees it.
pub fn to_slash_path(path: &Path) -> String {
    normalize_slashes(path.to_string_lossy()).into_owned()
}

/// Convert backslashes to forward slashes, avoiding a full scan/allocation when
/// the string contains none (the common case on Unix).
fn normalize_slashes(s: std::borrow::Cow<'_, str>) -> std::borrow::Cow<'_, str> {
    if s.contains('\\') {
        std::borrow::Cow::Owned(s.replace('\\', "/"))
    } else {
        s
    }
}

/// Parse a Paradox `.mod` descriptor file (plain key=value Paradox script).
///
/// Mirrors F# FileManager.fs:91-125: extracts `name`, `path`, and
/// `replace_path` entries.
pub(crate) fn parse_mod_descriptor(path: &Path) -> Result<ModDescriptor, FileError> {
    let raw = read_text(path)?;
    // Strip UTF-8 BOM (U+FEFF) so the first key isn't parsed as "\u{FEFF}name".
    let content = raw.strip_prefix('\u{FEFF}').unwrap_or(&raw);
    Ok(parse_mod_descriptor_str(content))
}

fn parse_mod_descriptor_str(content: &str) -> ModDescriptor {
    let mut name = String::new();
    let mut mod_path = None;
    let mut replace_paths = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let key = k.trim();
            let val = descriptor_value(v);
            match key {
                "name" => name = val,
                "path" | "archive" => mod_path = Some(val),
                "replace_path" => replace_paths.push(val),
                _ => {}
            }
        }
    }

    ModDescriptor {
        name,
        path: mod_path,
        replace_paths,
    }
}

/// Extract a `.mod` value. A quoted value is the text between the quotes, so a
/// trailing inline comment or an `=` inside the quotes is handled correctly
/// (`replace_path = "common/ideas" # keep` -> `common/ideas`). An unquoted value
/// runs up to an inline `#` comment. The old `trim_matches('"')` left the closing
/// quote in place whenever anything followed it.
fn descriptor_value(v: &str) -> String {
    let v = v.trim();
    if let Some(rest) = v.strip_prefix('"') {
        match rest.split_once('"') {
            Some((inner, _)) => inner.to_string(),
            None => rest.to_string(),
        }
    } else {
        v.split('#').next().unwrap_or(v).trim().to_string()
    }
}

// ── Multi-mod expansion ───────────────────────────────────────────────────────

/// A resolved mod entry: its descriptor plus the on-disk root directory.
#[derive(Debug, Clone)]
pub struct ResolvedMod {
    pub descriptor: ModDescriptor,
    /// Absolute path to the mod root directory.
    pub root: PathBuf,
}

/// Scan a `MultipleMod` workspace directory for `.mod` descriptors and resolve
/// each to a concrete mod root.
///
/// Mirrors F# FileManager.fs:64-90: reads every `*.mod` file inside the
/// `mod/` (or `mods/`) subfolder, parses it, and returns a `ResolvedMod` for
/// each descriptor whose `path` resolves to an existing directory.
///
/// `workspace` must be the directory that `classify_directory` returned
/// `MultipleMod` for.
pub fn expand_multiple_mods(workspace: &Path) -> Vec<ResolvedMod> {
    let mut out = Vec::new();

    for mod_folder_name in &["mod", "mods"] {
        let mod_folder = workspace.join(mod_folder_name);
        if !mod_folder.is_dir() {
            continue;
        }

        let entries = match std::fs::read_dir(&mod_folder) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .map(|e| e.eq_ignore_ascii_case("mod"))
                .unwrap_or(false)
                && let Ok(desc) = parse_mod_descriptor(&path)
                && let Some(mod_path) = &desc.path
            {
                // `path` can be relative (to the workspace) or absolute
                let root = if std::path::Path::new(mod_path).is_absolute() {
                    PathBuf::from(mod_path)
                } else {
                    workspace.join(mod_path)
                };
                if root.is_dir() {
                    out.push(ResolvedMod {
                        descriptor: desc,
                        root,
                    });
                }
            }
        }
    }

    // Sort by name for deterministic ordering
    out.sort_by(|a, b| a.descriptor.name.cmp(&b.descriptor.name));
    out
}

/// Discover files across multiple mods, honouring `replace_path`.
///
/// Mirrors F# FileManager.fs:91-147:
/// * Mods are layered: later mods in `mods` take priority over earlier ones
///   (typically the caller orders them from lowest to highest priority).
/// * A mod's `replace_path` entries suppress *all* files whose logical path
///   starts with that prefix that were contributed by lower-priority sources
///   (including vanilla).
///
/// Returns `(mod_root, files_from_that_root)` pairs so callers know the origin.
pub fn discover_files_multi_mod(
    vanilla_root: Option<&Path>,
    mods: &[ResolvedMod],
    include_dirs: &[String],
    budget: ScanBudget,
) -> Vec<(PathBuf, String)> {
    // Collect (logical_path, absolute_path, source_priority) triples.
    // Higher priority index wins.
    use std::collections::HashMap;

    let mut best: HashMap<String, (PathBuf, usize)> = HashMap::new();
    let mut remaining = budget.max_files;

    // Build ordered list: vanilla is priority 0, mods are 1..=n
    let mut sources: Vec<(usize, &Path, &[String])> = Vec::new();

    if let Some(v) = vanilla_root {
        sources.push((0, v, include_dirs));
    }
    for (i, m) in mods.iter().enumerate() {
        sources.push((i + 1, &m.root, include_dirs));
    }

    // Collect candidate files from all sources
    for (priority, root, dirs) in &sources {
        let root_prefix = normalize_root_prefix(root);
        for include_dir in *dirs {
            let dir = if *include_dir == "." {
                root.to_path_buf()
            } else {
                root.join(include_dir)
            };
            if !dir.is_dir() {
                continue;
            }
            collect_files_recursive(&dir, &root_prefix, *priority, &mut best, &mut remaining);
        }
    }

    // Apply replace_path suppression: for each mod (in priority order, highest
    // first), any file whose logical path starts with a replace_path prefix and
    // originates from a *lower* priority source is removed.
    // Lowercase each logical path once, rather than per replace_path entry below.
    let logical_lower: HashMap<String, String> = best
        .keys()
        .map(|k| (k.clone(), k.to_ascii_lowercase()))
        .collect();
    for (i, m) in mods.iter().enumerate().rev() {
        let mod_priority = i + 1;
        for rp in &m.descriptor.replace_paths {
            // Normalize: backslash → slash (Windows-authored .mod files), trim
            // leading/trailing slashes, then lowercase for case-insensitive match.
            let prefix_lower = rp.replace('\\', "/").trim_matches('/').to_ascii_lowercase();
            let prefix_lower_slash = format!("{}/", prefix_lower);
            best.retain(|logical, (_path, file_prio)| {
                // If the file's logical path is under this replace_path and
                // comes from a lower-priority source → suppress it.
                let ll = &logical_lower[logical.as_str()];
                let under_prefix = *ll == prefix_lower || ll.starts_with(&prefix_lower_slash);
                if under_prefix && *file_prio < mod_priority {
                    return false;
                }
                true
            });
        }
    }

    let mut result: Vec<(PathBuf, String)> = best
        .into_iter()
        .map(|(logical, (abs_path, _prio))| (abs_path, logical))
        .collect();
    result.sort_by(|a, b| a.1.cmp(&b.1));
    result
}

fn collect_files_recursive(
    dir: &Path,
    root_prefix: &str,
    priority: usize,
    out: &mut std::collections::HashMap<String, (PathBuf, usize)>,
    remaining_files: &mut usize,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if *remaining_files == 0 {
            break;
        }
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        // Reject symlinks and non-regular files outright (see `walk_dir_generic`).
        if ft.is_symlink() || !(ft.is_dir() || ft.is_file()) {
            continue;
        }
        if ft.is_dir() {
            // Skip VCS/build/editor dirs, same as the single-root walk, so a
            // mod's nested `.git`/`target`/… never contributes files.
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if is_excluded_dir(name) {
                continue;
            }
            collect_files_recursive(&path, root_prefix, priority, out, remaining_files);
        } else {
            *remaining_files -= 1;
            let logical = compute_logical_path_with_root(&path, root_prefix);
            // Higher priority wins
            let entry = out.entry(logical).or_insert((path.clone(), priority));
            if priority > entry.1 {
                *entry = (path, priority);
            }
        }
    }
}

/// Recursively collect every file under `root` whose extension is in
/// `extensions`, skipping engine/IDE directories and free-form text files.
///
/// This is the whole-tree walker used by the LSP full-workspace pass. The skip
/// lists (directories and free-form filenames) come from
/// `FileManagerConfig::default()` so they are defined in exactly one place and
/// stay consistent with the CLI's `discover_and_parse`. `extra_file_globs` and
/// `extra_dir_globs` layer on top of those defaults (they extend, never
/// replace, the engine baseline). Each directory's entries are sorted, so the
/// traversal order is deterministic and independent of the filesystem's
/// `read_dir` order.
#[deprecated(note = "use cwtools_driver::{workspace_discovery_config, discover_workspace_files}")]
pub fn walk_workspace_files(
    root: &Path,
    extensions: &[&str],
    extra_file_globs: &[String],
    extra_dir_globs: &[String],
    budget: ScanBudget,
) -> Vec<PathBuf> {
    let cfg = FileManagerConfig::default();
    let root_prefix = normalize_root_prefix(root);
    // Only a pattern that addresses a location reads the relative path, so a
    // workspace configured with plain name globs never builds one.
    let needs_relative =
        has_path_pattern(&cfg.exclude_patterns) || has_path_pattern(extra_file_globs);
    // Accept any file whose extension is requested and which isn't a free-form
    // excluded filename. No per-file size guard (unlike the CLI walker); the
    // read cap and byte budget are enforced when the LSP reads each file.
    let mut accept = |path: &Path| -> Option<PathBuf> {
        let ext = path.extension().and_then(|e| e.to_str())?;
        if !extensions.contains(&ext) {
            return None;
        }
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let relative = if needs_relative {
            compute_logical_path_with_root(path, &root_prefix)
        } else {
            String::new()
        };
        if cfg
            .exclude_patterns
            .iter()
            .any(|pat| ignore_glob_match(pat, file_name, &relative))
            || extra_file_globs
                .iter()
                .any(|pat| ignore_glob_match(pat, file_name, &relative))
        {
            return None;
        }
        Some(path.to_path_buf())
    };
    // The LSP walk silently ignores unreadable directories.
    let mut on_err = |_: &Path, _: std::io::Error| {};
    let mut state = WalkState {
        out: Vec::new(),
        remaining_files: budget.max_files,
    };
    let _ = walk_dir_generic(
        root,
        WalkRoot {
            prefix: &root_prefix,
            is_root_level: true,
        },
        &cfg,
        extra_dir_globs,
        &mut accept,
        &mut on_err,
        &mut state,
    );
    state.out
}

/// Shared directory traversal for both discovery walkers. Sorts each
/// directory's entries for deterministic order, applies the config's
/// directory-exclusion lists (plus `extra_dir_globs`), and passes every
/// regular file to `accept`; whatever `accept` returns is collected into
/// `state.out`. Symlinks and non-regular files (fifos, sockets, devices) are
/// rejected outright: a symlink can point outside the root or into a cycle,
/// and a special file can report length 0 and be read to EOF. Regular entries
/// reuse the `file_type` from `read_dir` to avoid a second stat. Read errors on
/// child directories go to `on_dir_err`; the top-level read error is returned.
/// The walk stops once `state.remaining_files` reaches 0.
///
/// Mutable state threaded through [`walk_dir_generic`]: the collected output
/// and the remaining per-scan file budget.
struct WalkState<T> {
    out: Vec<T>,
    remaining_files: usize,
}

/// Where the walk started, threaded down the recursion: the normalized root
/// prefix a directory's path glob is matched relative to, and whether `dir` is
/// the root itself, since only its direct children get `exclude_root_dirs`.
/// Every recursive call clears `is_root_level`.
#[derive(Clone, Copy)]
struct WalkRoot<'a> {
    prefix: &'a str,
    is_root_level: bool,
}

fn walk_dir_generic<T>(
    dir: &Path,
    root: WalkRoot<'_>,
    cfg: &FileManagerConfig,
    extra_dir_globs: &[String],
    accept: &mut dyn FnMut(&Path) -> Option<T>,
    on_dir_err: &mut dyn FnMut(&Path, std::io::Error),
    state: &mut WalkState<T>,
) -> std::io::Result<()> {
    // Only a directory pattern that addresses a location needs the relative
    // path built per entry; the usual all-names lists skip it entirely.
    let dir_paths_needed =
        has_path_pattern(&cfg.exclude_dir_patterns) || has_path_pattern(extra_dir_globs);
    // Collect (sort-key, path, file_type) once so sorting doesn't re-allocate an
    // OsString per comparison and directory tests reuse the readdir file type.
    let mut entries: Vec<(std::ffi::OsString, PathBuf, std::fs::FileType)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let Ok(ft) = entry.file_type() else { continue };
        entries.push((entry.file_name(), entry.path(), ft));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    for (_name, path, ft) in entries {
        if state.remaining_files == 0 {
            break;
        }
        // `file_type()` from `read_dir` doesn't follow symlinks, so this is a
        // single stat-free check that rejects symlinks and special files.
        if ft.is_symlink() || !(ft.is_dir() || ft.is_file()) {
            continue;
        }
        if ft.is_dir() {
            let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let dir_relative = if dir_paths_needed {
                compute_logical_path_with_root(&path, root.prefix)
            } else {
                String::new()
            };
            // Root-anchored excludes apply only to direct children of the root.
            let skip = cfg
                .exclude_dirs
                .iter()
                .any(|ex| dir_name.eq_ignore_ascii_case(ex))
                || cfg
                    .exclude_dir_patterns
                    .iter()
                    .any(|pat| ignore_glob_match(pat, dir_name, &dir_relative))
                || extra_dir_globs
                    .iter()
                    .any(|pat| ignore_glob_match(pat, dir_name, &dir_relative))
                || (root.is_root_level
                    && cfg
                        .exclude_root_dirs
                        .iter()
                        .any(|ex| dir_name.eq_ignore_ascii_case(ex)));
            if !skip
                && let Err(e) = walk_dir_generic(
                    &path,
                    WalkRoot {
                        is_root_level: false,
                        ..root
                    },
                    cfg,
                    extra_dir_globs,
                    accept,
                    on_dir_err,
                    state,
                )
            {
                on_dir_err(&path, e);
            }
            continue;
        }

        if let Some(item) = accept(&path) {
            state.remaining_files -= 1;
            state.out.push(item);
        }
    }
    Ok(())
}

/// Classify a directory following F# FileManager.fs:80-147.
///
/// - `Vanilla` if it contains `game/` or `common/` typical structure
/// - `Mod` if it looks like a single mod (has common/events/interface/gfx/localisation)
/// - `MultipleMod` if it contains a `mod/` or `mods/` folder with `.mod` files
/// - `Unknown` otherwise
pub fn classify_directory(dir: &Path) -> DirectoryType {
    let looks_like_game_folder = |d: &Path| -> bool {
        // Deliberately narrow: the Mod check below short-circuits MultipleMod, so
        // every name added here can hide a multi-mod workspace root.
        for sub in &["common", "events", "interface", "gfx", "localisation"] {
            if d.join(sub).is_dir() {
                return true;
            }
        }
        false
    };

    // Vanilla: contains a "game" sub-directory that itself looks like a game folder
    let game_sub = dir.join("game");
    if game_sub.is_dir() && looks_like_game_folder(&game_sub) {
        return DirectoryType::Vanilla;
    }

    // Mod: the directory itself looks like a mod
    if looks_like_game_folder(dir) {
        return DirectoryType::Mod;
    }

    // MultipleMod: contains mod/ or mods/ with .mod files
    for mod_folder_name in &["mod", "mods"] {
        let mod_folder = dir.join(mod_folder_name);
        if mod_folder.is_dir() {
            let has_mod_files = std::fs::read_dir(&mod_folder)
                .ok()
                .map(|mut entries| {
                    entries.any(|e| {
                        e.ok()
                            .and_then(|e| {
                                let p = e.path();
                                if p.extension()
                                    .map(|ex| ex.eq_ignore_ascii_case("mod"))
                                    .unwrap_or(false)
                                {
                                    Some(())
                                } else {
                                    None
                                }
                            })
                            .is_some()
                    })
                })
                .unwrap_or(false);
            if has_mod_files {
                return DirectoryType::MultipleMod;
            }
        }
    }

    DirectoryType::Unknown
}

/// Simple glob matching (supports `*` wildcard and `?`).
///
/// Handles:
/// - `*.ext` suffix matching
/// - `prefix*` prefix matching
/// - `?` single-char wildcard
/// - Directory-name plain equality
pub fn glob_match(pattern: &str, text: &str) -> bool {
    // Fast path for wildcard-free patterns (the default excludes are all
    // literal filenames): plain equality, skipping the DP matcher entirely.
    if !pattern.contains(['*', '?']) {
        return pattern == text;
    }
    // Fast path for *.ext — only valid when the remainder has no further wildcards.
    if let Some(suffix) = pattern.strip_prefix('*')
        && !suffix.contains(['*', '?'])
    {
        return text.ends_with(suffix);
    }
    // Fast path for prefix* — only valid when the prefix has no wildcards.
    if let Some(prefix) = pattern.strip_suffix('*')
        && !prefix.contains(['*', '?'])
    {
        return text.starts_with(prefix);
    }
    // General: treat * as "any chars", ? as "any single char"
    glob_match_general(pattern, text)
}

/// Greedy two-pointer wildcard match (`*` = any run, `?` = one char). The
/// classic backtracking algorithm: amortized O(m+n) with O(1) scratch, no
/// per-call allocations beyond the two char vectors. Worst case is O(m*n)
/// (a long literal segment after a `*` re-scanned against a near-miss text),
/// which the callers' pattern-length caps keep bounded (#169).
fn glob_match_general(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_greedy(&p, &t)
}

/// True if `pattern` addresses a location in the tree rather than a bare name.
/// Windows users write `\`, so both separators count.
fn is_path_pattern(pattern: &str) -> bool {
    pattern.contains(['/', '\\'])
}

/// True if any of `patterns` addresses a location, so a walk that finds none
/// can skip building a root-relative path per entry.
fn has_path_pattern(patterns: &[String]) -> bool {
    patterns.iter().any(|p| is_path_pattern(p))
}

/// Match one user ignore glob against a discovered entry. A pattern with no
/// separator is a name glob matched against `name` at any depth, which is what
/// every pattern meant before this existed. One with a separator addresses a
/// location and is matched against `relative`, the entry's path relative to the
/// mod or workspace root, forward-slashed. So `**/skip.txt`, the spelling the
/// VS Code client generates for every `errors.ignorefiles` entry, matches the
/// file it names instead of nothing at all (#244).
///
/// `relative` is read only for path patterns, so a caller whose lists hold none
/// can pass `""`.
pub fn ignore_glob_match(pattern: &str, name: &str, relative: &str) -> bool {
    if is_path_pattern(pattern) {
        path_glob_match(pattern, relative)
    } else {
        glob_match(pattern, name)
    }
}

/// Excluded by engine baseline or `extra_file_globs`.
pub fn is_ignored_logical_path(logical_path: &str, extra_file_globs: &[String]) -> bool {
    let cfg = FileManagerConfig::default();
    // Handle Windows separators.

    let normalized = if logical_path.contains('\\') {
        logical_path.replace('\\', "/")
    } else {
        logical_path.to_string()
    };
    let file_name = normalized.rsplit(['/', '\\']).next().unwrap_or(&normalized);
    cfg.exclude_patterns
        .iter()
        .any(|pat| ignore_glob_match(pat, file_name, &normalized))
        || extra_file_globs
            .iter()
            .any(|pat| ignore_glob_match(pat, file_name, &normalized))
}

/// Like `is_ignored_logical_path` but derives logical path from `root`.
pub fn is_ignored_file(
    root: &std::path::Path,
    path: &std::path::Path,
    extra_file_globs: &[String],
) -> bool {
    let logical = compute_logical_path(path, root);
    is_ignored_logical_path(&logical, extra_file_globs)
}

/// Excluded by engine baseline, file globs, engine directory lists, or directory
/// globs. This is the predicate workspace discovery applies, surfaced so the
/// LSP's incremental paths cannot drift from a full scan.
pub fn is_ignored_path(
    logical_path: &str,
    extra_file_globs: &[String],
    extra_dir_globs: &[String],
) -> bool {
    if is_ignored_logical_path(logical_path, extra_file_globs) {
        return true;
    }
    if extra_dir_globs.is_empty() {
        return false;
    }
    let normalized = if logical_path.contains('\\') {
        logical_path.replace('\\', "/")
    } else {
        logical_path.to_string()
    };
    let segments: Vec<&str> = normalized.split('/').collect();
    // A bare filename has no parent directories to match against.
    if segments.len() < 2 {
        return false;
    }
    let mut dir_relative = String::new();
    for (i, dir_name) in segments.iter().take(segments.len() - 1).enumerate() {
        if i > 0 {
            dir_relative.push('/');
        }
        dir_relative.push_str(dir_name);
        if extra_dir_globs
            .iter()
            .any(|pat| ignore_glob_match(pat, dir_name, &dir_relative))
        {
            return true;
        }
    }
    false
}

/// Path-aware glob: `**` spans any run of directories (including none), while
/// `*` and `?` stay inside one segment. That segment boundary is the whole
/// reason this can't be [`glob_match`] over the joined path, where `*` would
/// happily cross a `/`.
///
/// A leading separator anchors at the root, which `path` already is, so it only
/// says where matching starts. A trailing one means everything below, and is
/// read as a trailing `**`.
fn path_glob_match(pattern: &str, path: &str) -> bool {
    let mut segments: Vec<&str> = pattern.split(['/', '\\']).collect();
    if segments.first() == Some(&"") {
        segments.remove(0);
    }
    if segments.last() == Some(&"") {
        segments.pop();
        segments.push("**");
    }
    let text: Vec<&str> = path.split('/').collect();
    segments_greedy(&segments, &text)
}

/// [`glob_greedy`] one level up: the same backtracking walk with `**` as the
/// star and a whole segment as the unit, each pair compared by [`glob_match`]
/// so `*` and `?` keep their meaning inside a single name.
fn segments_greedy(p: &[&str], t: &[&str]) -> bool {
    let (m, n) = (p.len(), t.len());
    let mut i = 0; // position in t
    let mut j = 0; // position in p
    let mut star: Option<usize> = None; // most recent '**'
    let mut mark = 0; // t position the star run restarts from
    while i < n {
        if j < m && p[j] == "**" {
            star = Some(j);
            j += 1;
            mark = i;
        } else if j < m && glob_match(p[j], t[i]) {
            i += 1;
            j += 1;
        } else if let Some(s) = star {
            // The star swallows one more directory and the run re-tries.
            j = s + 1;
            mark += 1;
            i = mark;
        } else {
            return false;
        }
    }
    while j < m && p[j] == "**" {
        j += 1;
    }
    j == m
}

fn glob_greedy(p: &[char], t: &[char]) -> bool {
    let (m, n) = (p.len(), t.len());
    let mut i = 0; // position in t
    let mut j = 0; // position in p
    let mut star: Option<usize> = None; // most recent '*'
    let mut mark = 0; // t position the star segment restarts from
    while i < n {
        if j < m && p[j] == '*' {
            // Star before equality: a literal '*' in the text must not
            // consume the pattern's star and lose the backtrack point.
            star = Some(j);
            j += 1;
            mark = i;
        } else if j < m && (p[j] == '?' || p[j] == t[i]) {
            i += 1;
            j += 1;
        } else if let Some(s) = star {
            // The star matches one more char and the segment re-tries.
            j = s + 1;
            mark += 1;
            i = mark;
        } else {
            return false;
        }
    }
    while j < m && p[j] == '*' {
        j += 1;
    }
    j == m
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;

    #[test]
    fn mod_descriptor_robust_values() {
        // #213: trailing comments, quoted '=', and unquoted values must parse.
        let d = parse_mod_descriptor_str(
            "name = \"Test = Mod\"\n\
             path = \"mod/root\"  # the root\n\
             replace_path = \"common/ideas\"\n\
             replace_path = \"common/foo=bar\"\n\
             replace_path = \"events\" # keep vanilla out\n\
             replace_path = common/units\n\
             replace_path = common/raids # bare with comment\n\
             # a comment line\n\
             dependencies = { \"ModA\" \"ModB\" }\n",
        );
        assert_eq!(d.name, "Test = Mod");
        assert_eq!(d.path.as_deref(), Some("mod/root"));
        assert_eq!(
            d.replace_paths,
            vec![
                "common/ideas",
                "common/foo=bar",
                "events",
                "common/units",
                "common/raids",
            ]
        );
    }

    #[test]
    fn mod_descriptor_clean_lines_unchanged() {
        // The common case (clean quoted lines, as in the Millennium Dawn
        // descriptor) must parse identically to before.
        let d = parse_mod_descriptor_str(
            "name=\"Millennium Dawn\"\nreplace_path = \"common/ideas\"\nreplace_path = \"events\"\n",
        );
        assert_eq!(d.name, "Millennium Dawn");
        assert_eq!(d.replace_paths, vec!["common/ideas", "events"]);
    }

    #[test]
    fn glob_matching() {
        assert!(glob_match("*.txt", "foo.txt"));
        assert!(!glob_match("*.txt", "foo.png"));
        assert!(glob_match("*.cwt", "rules.cwt"));
        assert!(glob_match("foo*", "foobar"));
        assert!(!glob_match("foo*", "barfoo"));
        assert!(glob_match("f?o.txt", "foo.txt"));
        assert!(!glob_match("f?o.txt", "fooo.txt"));
    }

    #[test]
    fn glob_match_multi_wildcard() {
        // *foo* must not take the *.ext fast path and treat "foo*" as a literal suffix.
        assert!(glob_match("*foo*", "barfoobar"));
        assert!(glob_match("*foo*", "foo"));
        assert!(glob_match("*foo*", "xfoox"));
        assert!(!glob_match("*foo*", "bar"));
        // prefix* fast path must not trigger when the prefix itself contains ?.
        assert!(glob_match("fo?*", "foobar"));
        assert!(!glob_match("fo?*", "fo")); // needs at least one char after "fo"
    }

    #[test]
    fn path_globs_match_a_location_not_just_a_name() {
        // #244: the VS Code client rewrites every `errors.ignorefiles` entry to
        // `**/<name>`, which matched nothing at all while every ignore pattern
        // was a name glob.
        assert!(ignore_glob_match("**/skip.txt", "skip.txt", "skip.txt"));
        assert!(ignore_glob_match(
            "**/skip.txt",
            "skip.txt",
            "common/units/skip.txt"
        ));
        assert!(!ignore_glob_match(
            "**/skip.txt",
            "keep.txt",
            "common/keep.txt"
        ));
        // The shipped `cwtools.ignore_patterns` default, dead for the same reason.
        assert!(ignore_glob_match(
            "**/99_README**.txt",
            "99_README_units.txt",
            "common/99_README_units.txt"
        ));
        // A name glob keeps its old meaning: the name, at any depth, and it
        // never consults the relative path.
        assert!(ignore_glob_match("*.md", "notes.md", "docs/notes.md"));
        assert!(ignore_glob_match("*.md", "notes.md", ""));
        assert!(!ignore_glob_match("*.md", "notes.txt", "docs/notes.txt"));
    }

    #[test]
    fn path_globs_anchor_at_the_root_and_respect_segments() {
        // A separator anchors the pattern, unlike a bare name.
        assert!(ignore_glob_match(
            "common/foo.txt",
            "foo.txt",
            "common/foo.txt"
        ));
        assert!(!ignore_glob_match(
            "common/foo.txt",
            "foo.txt",
            "gfx/common/foo.txt"
        ));
        // A leading separator says the same thing; the target is already relative.
        assert!(ignore_glob_match(
            "/common/foo.txt",
            "foo.txt",
            "common/foo.txt"
        ));
        // `*` stays inside one segment; `**` spans any run of them, including none.
        assert!(!ignore_glob_match(
            "common/*.txt",
            "foo.txt",
            "common/units/foo.txt"
        ));
        assert!(ignore_glob_match(
            "common/**/*.txt",
            "foo.txt",
            "common/units/foo.txt"
        ));
        assert!(ignore_glob_match(
            "gfx/**/*.dds",
            "f.dds",
            "gfx/interface/goals/f.dds"
        ));
        assert!(ignore_glob_match(
            "common/**/foo.txt",
            "foo.txt",
            "common/foo.txt"
        ));
        // A trailing separator covers the tree below it.
        assert!(ignore_glob_match("build/", "foo.txt", "build/a/foo.txt"));
        assert!(!ignore_glob_match("build/", "foo.txt", "src/foo.txt"));
        // The Windows spelling of a path pattern means the same thing.
        assert!(ignore_glob_match(
            "common\\foo.txt",
            "foo.txt",
            "common/foo.txt"
        ));
    }

    /// The pre-#169 DP matcher, kept as the reference for the greedy
    /// matcher's equivalence tests.
    fn glob_dp_reference(p: &[char], t: &[char]) -> bool {
        let m = p.len();
        let n = t.len();
        let mut dp = vec![false; n + 1];
        dp[0] = true;
        for i in 1..=m {
            let mut prev_diag = dp[0];
            dp[0] = dp[0] && p[i - 1] == '*';
            for j in 1..=n {
                let above = dp[j];
                if p[i - 1] == '*' {
                    dp[j] = dp[j] || dp[j - 1];
                } else if p[i - 1] == '?' || p[i - 1] == t[j - 1] {
                    dp[j] = prev_diag;
                } else {
                    dp[j] = false;
                }
                prev_diag = above;
            }
        }
        dp[n]
    }

    fn reference_match(pattern: &str, text: &str) -> bool {
        let p: Vec<char> = pattern.chars().collect();
        let t: Vec<char> = text.chars().collect();
        glob_dp_reference(&p, &t)
    }

    #[test]
    fn glob_greedy_agrees_with_reference_dp_exhaustive() {
        // Every pattern/text pair over {a, b, *, ?} up to length 4 (341
        // strings each) must agree with the reference DP, fast paths included.
        let alphabet = ['a', 'b', '*', '?'];
        let mut strings: Vec<String> = vec![String::new()];
        for _ in 1..=4 {
            let prev: Vec<String> = strings.clone();
            for s in &prev {
                for &c in &alphabet {
                    let mut next = s.clone();
                    next.push(c);
                    strings.push(next);
                }
            }
        }
        for p in &strings {
            for t in &strings {
                assert_eq!(
                    glob_match(p, t),
                    reference_match(p, t),
                    "mismatch for pattern {p:?} text {t:?}"
                );
            }
        }
    }

    #[test]
    fn glob_greedy_agrees_with_reference_dp_randomized() {
        // Longer randomized cases; a fixed LCG so a failure reproduces.
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let alphabet = ['a', 'b', 'c', 'x', 'y', '*', '?'];
        for _ in 0..5000 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let plen = (state % 12) as usize;
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let tlen = (state % 20) as usize;
            let p: String = (0..plen)
                .map(|_| alphabet[((state >> 32) % 7) as usize])
                .collect();
            let t: String = (0..tlen)
                .map(|_| alphabet[((state >> 16) % 7) as usize])
                .collect();
            assert_eq!(
                glob_match(&p, &t),
                reference_match(&p, &t),
                "mismatch for pattern {p:?} text {t:?}"
            );
        }
    }

    #[test]
    fn glob_worst_case_pattern_completes() {
        // #169 regression: a 1 MB '?'-heavy pattern used to force ~255M DP
        // iterations per maximum-length filename, repeated across the walk.
        // The greedy matcher is linear here (milliseconds); the DP would run
        // tens of seconds in debug and time the suite out.
        let pat = "?".repeat(1024 * 1024);
        let text = "a".repeat(255);
        assert!(!glob_match(&pat, &text));
        // Star-segment worst case at the config length cap. The '?' keeps the
        // pattern off the *.ext fast path so the backtracking runs.
        let pat = format!("*{}?{}b", "a".repeat(512), "a".repeat(511));
        let text = format!("{}c", "a".repeat(255));
        assert!(!glob_match(&pat, &text));
        assert!(glob_match(
            &format!("*{}", "a".repeat(1024)),
            &"a".repeat(3000)
        ));
    }

    #[test]
    fn default_excludes_skip_changelog_and_markdown() {
        let cfg = FileManagerConfig::default();
        assert!(cfg.exclude_patterns.iter().any(|p| p == "Changelog.txt"));
        assert!(cfg.exclude_patterns.iter().any(|p| p == "*.md"));
    }

    #[test]
    fn exclude_dir_patterns_skips_matching_dirs() {
        use std::fs;
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path();

        // Layout:
        //   root/common/foo.txt          (include)
        //   root/temp/skipme.txt         (skip: dir matches "temp")
        //   root/template/keepme.txt     (include: dir does NOT match "temp")
        //   root/notes/Changelog.txt     (skip: filename matches)
        for rel in [
            "common/foo.txt",
            "temp/skipme.txt",
            "template/keepme.txt",
            "notes/Changelog.txt",
        ] {
            if let Some(parent) = std::path::Path::new(rel).parent() {
                fs::create_dir_all(root.join(parent)).unwrap();
            }
            fs::write(root.join(rel), "").unwrap();
        }

        let cfg = FileManagerConfig {
            root: root.to_path_buf(),
            include_dirs: vec![".".into()],
            exclude_dir_patterns: vec!["temp".into()],
            ..Default::default()
        };

        let fm = FileManager::new(cfg);
        let mut paths = Vec::new();
        fm.collect_paths(root, &mut paths).unwrap();
        let names: Vec<String> = paths.iter().map(|(_, lp)| lp.clone()).collect();

        assert!(names.iter().any(|n| n.ends_with("common/foo.txt")));
        assert!(
            names.iter().any(|n| n.ends_with("template/keepme.txt")),
            "template/ should NOT match the exact 'temp' pattern"
        );
        assert!(
            !names.iter().any(|n| n.ends_with("temp/skipme.txt")),
            "temp/ should be skipped by exclude_dir_patterns"
        );
        assert!(
            !names.iter().any(|n| n.ends_with("notes/Changelog.txt")),
            "Changelog.txt should be skipped by default exclude_patterns"
        );
    }

    /// A root-level `resources/` is dev scratch the game never loads, but
    /// `common/resources/` defines the `resource` type (oil, steel, …). The
    /// default excludes must skip the former and keep the latter, on BOTH the
    /// CLI (`collect_paths`) and LSP (`walk_workspace_files`) discovery paths.
    #[test]
    fn root_resources_skipped_but_common_resources_indexed() {
        use std::fs;
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path();
        for rel in ["common/resources/00_resources.txt", "resources/scratch.txt"] {
            fs::create_dir_all(root.join(Path::new(rel).parent().unwrap())).unwrap();
            fs::write(root.join(rel), "").unwrap();
        }

        // CLI path.
        let fm = FileManager::new(FileManagerConfig {
            root: root.to_path_buf(),
            include_dirs: vec![".".into()],
            ..Default::default()
        });
        let mut paths = Vec::new();
        fm.collect_paths(root, &mut paths).unwrap();
        let cli: Vec<String> = paths.iter().map(|(_, lp)| lp.clone()).collect();
        assert!(
            cli.iter()
                .any(|n| n.ends_with("common/resources/00_resources.txt")),
            "common/resources must be indexed: {cli:?}"
        );
        assert!(
            !cli.iter().any(|n| n.ends_with("resources/scratch.txt")),
            "root resources/ must be skipped: {cli:?}"
        );

        // LSP whole-tree path.
        let lsp = walk_workspace_files(root, &["txt"], &[], &[], ScanBudget::default());
        let lsp: Vec<String> = lsp
            .iter()
            .map(|p| normalize_slashes(p.to_string_lossy()).into_owned())
            .collect();
        assert!(
            lsp.iter()
                .any(|n| n.ends_with("common/resources/00_resources.txt")),
            "common/resources must be walked: {lsp:?}"
        );
        assert!(
            !lsp.iter().any(|n| n.ends_with("resources/scratch.txt")),
            "root resources/ must be skipped by whole-tree walk: {lsp:?}"
        );
    }

    #[test]
    fn classify_ext() {
        assert_eq!(classify_extension(Path::new("foo.txt")), FileKind::Script);
        assert_eq!(classify_extension(Path::new("foo.gui")), FileKind::Script);
        assert_eq!(classify_extension(Path::new("foo.gfx")), FileKind::Script);
        assert_eq!(classify_extension(Path::new("foo.asset")), FileKind::Script);
        assert_eq!(
            classify_extension(Path::new("foo.yml")),
            FileKind::Localisation
        );
        assert_eq!(
            classify_extension(Path::new("foo.csv")),
            FileKind::Localisation
        );
        assert_eq!(classify_extension(Path::new("foo.dds")), FileKind::Resource);
        assert_eq!(classify_extension(Path::new("foo.png")), FileKind::Resource);
    }

    #[test]
    fn logical_path_stripping() {
        let root = PathBuf::from("/mnt/mod");
        let path = PathBuf::from("/mnt/mod/common/effects/foo.txt");
        assert_eq!(compute_logical_path(&path, &root), "common/effects/foo.txt");
    }

    #[test]
    fn logical_path_fallback() {
        let root = PathBuf::from("/other");
        let path = PathBuf::from("/mnt/mod/foo.txt");
        assert_eq!(compute_logical_path(&path, &root), "foo.txt");
    }

    // ── CP-1252 / encoding tests ──────────────────────────────────────────────

    #[test]
    fn cp1252_e_acute_0xe9() {
        // 0xE9 in CP-1252 is U+00E9 (é), same as Latin-1 for bytes >= 0xA0
        assert_eq!(cp1252_byte(0xE9), 'é');
    }

    #[test]
    fn cp1252_euro_sign_0x80() {
        // 0x80 in CP-1252 is the Euro sign U+20AC — NOT U+0080
        assert_eq!(cp1252_byte(0x80), '€');
    }

    #[test]
    fn cp1252_ascii_passthrough() {
        assert_eq!(cp1252_byte(b'A'), 'A');
        assert_eq!(cp1252_byte(b'\n'), '\n');
    }

    #[test]
    fn read_text_cp1252_bytes_via_tmpfile() {
        use std::io::Write as _;

        // Build a sequence: "caf" + 0xE9 (é in CP-1252) + "\n"
        let bytes: &[u8] = b"caf\xE9\n";
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        tmp.write_all(bytes).expect("write");

        let text = read_text(tmp.path()).expect("read_text");
        assert_eq!(text, "caf\u{E9}\n", "0xE9 should decode as é (U+00E9)");
    }

    /// The bytes a loc file actually carries decide CW254, and every test of
    /// that code hands the enum over ready-made. These are the real ones.
    #[test]
    fn decode_bytes_reports_the_encoding_the_leading_bytes_describe() {
        let body = b"l_english:\n key: \"hi\"\n";

        let mut with_bom = UTF8_BOM.to_vec();
        with_bom.extend_from_slice(body);
        let (text, enc) = decode_bytes(with_bom);
        assert_eq!(enc, FileEncoding::Utf8Bom);
        assert!(
            text.starts_with('\u{FEFF}'),
            "the BOM stays in the text for the parsers to strip, got: {text:?}"
        );

        let (text, enc) = decode_bytes(body.to_vec());
        assert_eq!(enc, FileEncoding::Utf8NoBom);
        assert_eq!(text.as_bytes(), body);

        // A lone 0xE9 is valid CP-1252 and invalid UTF-8, so the sniff has to
        // fall through to the byte-wise decode rather than report a BOM-less
        // UTF-8 file.
        let (text, enc) = decode_bytes(b"key: \"caf\xE9\"\n".to_vec());
        assert_eq!(enc, FileEncoding::NonUtf8);
        assert!(text.contains('\u{E9}'), "got: {text:?}");
    }

    /// A UTF-16 file is not UTF-8 with a BOM, and it has to be reported as such
    /// rather than mistaken for one: `FF FE` / `FE FF` are not the UTF-8 BOM,
    /// and a UTF-16 body is not valid UTF-8 either.
    #[test]
    fn decode_bytes_does_not_take_a_utf16_bom_for_a_utf8_one() {
        for bom in [[0xFFu8, 0xFE], [0xFE, 0xFF]] {
            let mut bytes = bom.to_vec();
            bytes.extend_from_slice(&[0x6C, 0x00, 0x5F, 0x00]); // "l_" in UTF-16
            let (_, enc) = decode_bytes(bytes);
            assert_eq!(enc, FileEncoding::NonUtf8, "for BOM {bom:02X?}");
        }
    }

    /// A prefix of the BOM is not a BOM. Two of the three bytes and the file is
    /// still BOM-less, which is what CW254 reports.
    #[test]
    fn decode_bytes_needs_all_three_bom_bytes() {
        let mut bytes = UTF8_BOM[..2].to_vec();
        bytes.extend_from_slice(b"l_english:\n");
        let (_, enc) = decode_bytes(bytes);
        assert_eq!(enc, FileEncoding::NonUtf8, "0xEF 0xBB alone is not UTF-8");

        let (_, enc) = decode_bytes(UTF8_BOM.to_vec());
        assert_eq!(enc, FileEncoding::Utf8Bom, "a BOM and nothing else");
    }

    /// The whole path a real file travels: bytes on disk, read, sniffed. The
    /// enum every CW254 test builds by hand comes from here.
    #[test]
    fn read_text_with_encoding_sniffs_a_real_bom_off_disk() {
        use std::io::Write as _;

        let body = b"l_english:\n key: \"hi\"\n";
        let mut bommed = UTF8_BOM.to_vec();
        bommed.extend_from_slice(body);

        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        tmp.write_all(&bommed).expect("write");
        let (_, enc) = read_text_with_encoding(tmp.path()).expect("read");
        assert_eq!(enc, FileEncoding::Utf8Bom);

        let mut plain = tempfile::NamedTempFile::new().expect("tempfile");
        plain.write_all(body).expect("write");
        let (_, enc, len) = read_text_capped_with_encoding(plain.path(), 1024).expect("read");
        assert_eq!(enc, FileEncoding::Utf8NoBom);
        assert_eq!(len, body.len() as u64);
    }

    // ── multi-mod expand / replace_path tests ─────────────────────────────────

    #[test]
    fn multi_mod_replace_path_suppresses_vanilla() {
        use std::collections::HashMap;
        use std::fs;

        // Create a tiny temp filesystem:
        //   workspace/
        //     vanilla/common/foo.txt
        //     moda/common/foo.txt      (replaces common/)
        //     modb/events/bar.txt
        let workspace = tempfile::TempDir::new().expect("tmpdir");
        let wsp = workspace.path();

        let vanilla = wsp.join("vanilla");
        fs::create_dir_all(vanilla.join("common")).unwrap();
        fs::write(vanilla.join("common/foo.txt"), "vanilla").unwrap();

        let moda_root = wsp.join("moda");
        fs::create_dir_all(moda_root.join("common")).unwrap();
        fs::write(moda_root.join("common/foo.txt"), "moda").unwrap();

        let modb_root = wsp.join("modb");
        fs::create_dir_all(modb_root.join("events")).unwrap();
        fs::write(modb_root.join("events/bar.txt"), "modb").unwrap();

        let mods = vec![
            ResolvedMod {
                descriptor: ModDescriptor {
                    name: "ModA".into(),
                    path: Some(moda_root.to_str().unwrap().to_string()),
                    replace_paths: vec!["common".into()],
                },
                root: moda_root.clone(),
            },
            ResolvedMod {
                descriptor: ModDescriptor {
                    name: "ModB".into(),
                    path: Some(modb_root.to_str().unwrap().to_string()),
                    replace_paths: vec![],
                },
                root: modb_root.clone(),
            },
        ];

        let include_dirs = vec!["common".to_string(), "events".to_string()];
        let files =
            discover_files_multi_mod(Some(&vanilla), &mods, &include_dirs, ScanBudget::default());

        // Build logical_path → content map
        let by_logical: HashMap<String, String> = files
            .iter()
            .map(|(abs, logical)| {
                let content = fs::read_to_string(abs).unwrap_or_default();
                (logical.clone(), content)
            })
            .collect();

        // Vanilla's common/foo.txt should be suppressed by ModA's replace_path
        assert_eq!(
            by_logical.get("common/foo.txt").map(|s| s.as_str()),
            Some("moda"),
            "ModA's common/foo.txt should win; vanilla suppressed by replace_path"
        );

        // ModB's events/bar.txt should be present
        assert!(
            by_logical.contains_key("events/bar.txt"),
            "ModB events/bar.txt should be present"
        );
    }

    /// A workspace of two mods (a `mod/` folder with two `.mod` descriptors)
    /// must classify as `MultipleMod`, expand to both resolved roots (name-sorted),
    /// and discover the union of their script files with later-mod-wins override:
    /// the alphabetically-greater mod name wins a shared logical path. Non-script
    /// and default-excluded files are dropped, same as the single-root walk.
    #[test]
    fn multi_mod_workspace_expands_and_overrides() {
        use std::collections::HashMap;
        use std::fs;

        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let ws = tmp.path();

        // Two mods, resolved via .mod descriptors in the workspace's mod/ folder.
        fs::create_dir_all(ws.join("mod")).unwrap();
        fs::write(
            ws.join("mod/alpha.mod"),
            "name = \"Alpha Mod\"\npath = \"alpha\"\n",
        )
        .unwrap();
        fs::write(
            ws.join("mod/bravo.mod"),
            "name = \"Bravo Mod\"\npath = \"bravo\"\n",
        )
        .unwrap();

        // Alpha: a shared file, an alpha-only file, plus files that must be filtered.
        fs::create_dir_all(ws.join("alpha/common")).unwrap();
        fs::create_dir_all(ws.join("alpha/localisation")).unwrap();
        fs::write(ws.join("alpha/common/foo.txt"), "shared = alpha").unwrap();
        fs::write(ws.join("alpha/common/only_a.txt"), "only = alpha").unwrap();
        fs::write(ws.join("alpha/common/README.md"), "notes").unwrap();
        fs::write(ws.join("alpha/localisation/x.yml"), "l_english:").unwrap();

        // Bravo: overrides the shared file, adds an event file.
        fs::create_dir_all(ws.join("bravo/common")).unwrap();
        fs::create_dir_all(ws.join("bravo/events")).unwrap();
        fs::write(ws.join("bravo/common/foo.txt"), "shared = bravo").unwrap();
        fs::write(ws.join("bravo/events/e.txt"), "evt = bravo").unwrap();

        assert_eq!(classify_directory(ws), DirectoryType::MultipleMod);

        let mods = expand_multiple_mods(ws);
        assert_eq!(
            mods.iter()
                .map(|m| m.descriptor.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Alpha Mod", "Bravo Mod"],
            "mods resolve name-sorted"
        );

        let mut fm = FileManager::new(FileManagerConfig {
            root: ws.to_path_buf(),
            include_dirs: vec!["common".into(), "events".into()],
            ..Default::default()
        });
        let files = fm
            .discover_and_parse_multi_mod()
            .expect("multi-mod discover");

        let by_logical: HashMap<String, PathBuf> = files
            .iter()
            .map(|f| (f.logical_path.clone(), f.path.clone()))
            .collect();

        // Shared file: Bravo wins (alphabetically-greater name = higher priority).
        let foo = by_logical
            .get("common/foo.txt")
            .expect("common/foo.txt present");
        assert_eq!(
            fs::read_to_string(foo).unwrap(),
            "shared = bravo",
            "Bravo's common/foo.txt overrides Alpha's"
        );
        // Alpha-only and Bravo-only files both survive.
        assert!(by_logical.contains_key("common/only_a.txt"));
        assert!(by_logical.contains_key("events/e.txt"));
        // Non-script and default-excluded files are dropped.
        assert!(
            !by_logical.keys().any(|k| k.ends_with("README.md")),
            "*.md excluded: {by_logical:?}"
        );
        assert!(
            !by_logical.keys().any(|k| k.ends_with(".yml")),
            "loc files are not script-discovered: {by_logical:?}"
        );
    }

    /// A plain single mod directory must classify as `Mod`, NOT `MultipleMod`, so
    /// the driver keeps taking the existing single-root discovery path unchanged.
    #[test]
    fn single_mod_directory_classifies_as_mod() {
        use std::fs;
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path();
        fs::create_dir_all(root.join("common")).unwrap();
        fs::create_dir_all(root.join("events")).unwrap();
        fs::write(root.join("common/foo.txt"), "x = 1").unwrap();

        assert_eq!(classify_directory(root), DirectoryType::Mod);

        // The existing single-root walk still finds the file.
        let fm = FileManager::new(FileManagerConfig {
            root: root.to_path_buf(),
            include_dirs: vec!["common".into()],
            ..Default::default()
        });
        let mut paths = Vec::new();
        fm.collect_paths(root, &mut paths).unwrap();
        assert!(paths.iter().any(|(_, lp)| lp.ends_with("common/foo.txt")));
    }

    #[test]
    fn walk_workspace_files_returns_sorted_order() {
        // The workspace scan must process files in a deterministic, sorted order
        // independent of the filesystem's read_dir order, so editor diagnostics
        // and indexing are reproducible run to run.
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path();
        for name in ["zebra.txt", "alpha.txt", "middle.txt"] {
            std::fs::write(root.join(name), "").unwrap();
        }
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("aaa.txt"), "").unwrap();

        let files = walk_workspace_files(root, &["txt"], &[], &[], ScanBudget::default());
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        let pos = |n: &str| names.iter().position(|x| x == n).expect("file present");
        assert!(pos("alpha.txt") < pos("middle.txt"), "got: {:?}", names);
        assert!(pos("middle.txt") < pos("zebra.txt"), "got: {:?}", names);
    }

    /// #244: the globs the LSP forwards reach the walk, and a `**/`-prefixed one
    /// (every `errors.ignorefiles` entry, once the client has rewritten it) drops
    /// the file it names wherever it sits.
    #[test]
    fn walk_workspace_files_honours_path_globs() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("common/units")).unwrap();
        std::fs::write(root.join("skip.txt"), "").unwrap();
        std::fs::write(root.join("common/units/skip.txt"), "").unwrap();
        std::fs::write(root.join("common/units/keep.txt"), "").unwrap();

        let names = |globs: &[String], dirs: &[String]| -> Vec<String> {
            walk_workspace_files(root, &["txt"], globs, dirs, ScanBudget::default())
                .iter()
                .map(|p| compute_logical_path(p, root))
                .collect()
        };

        let all = names(&[], &[]);
        assert_eq!(all.len(), 3, "nothing ignored yet: {all:?}");

        let filtered = names(&["**/skip.txt".to_string()], &[]);
        assert_eq!(filtered, ["common/units/keep.txt"], "got: {filtered:?}");

        // Anchored to one location, the sibling at the root survives.
        let anchored = names(&["common/**/skip.txt".to_string()], &[]);
        assert!(
            anchored.contains(&"skip.txt".to_string()),
            "got: {anchored:?}"
        );
        assert!(
            !anchored.contains(&"common/units/skip.txt".to_string()),
            "got: {anchored:?}"
        );

        // A directory glob addressing a path prunes that subtree and no other.
        let pruned = names(&[], &["common/units".to_string()]);
        assert_eq!(pruned, ["skip.txt"], "got: {pruned:?}");
    }

    /// The CLI walker reads the same globs the same way (`exclude_patterns` is
    /// where the driver puts `--ignore-file`), so the two discovery paths can't
    /// disagree about what a pattern means.
    #[test]
    fn collect_paths_honours_path_globs() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("common/units")).unwrap();
        std::fs::write(root.join("common/units/skip.txt"), "").unwrap();
        std::fs::write(root.join("common/units/keep.txt"), "").unwrap();

        let fm = FileManager::new(FileManagerConfig {
            root: root.to_path_buf(),
            exclude_patterns: vec!["**/skip.txt".to_string()],
            ..Default::default()
        });
        let mut paths = Vec::new();
        fm.collect_paths(root, &mut paths).unwrap();
        let logical: Vec<&str> = paths.iter().map(|(_, lp)| lp.as_str()).collect();
        assert_eq!(logical, ["common/units/keep.txt"], "got: {logical:?}");
    }

    /// A root that isn't there is an error, not an empty result: a typo'd
    /// `--directory` used to walk nothing and report a clean run.
    #[test]
    fn discover_and_parse_missing_root_is_an_error() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let missing = tmp.path().join("no_such_mod");
        let mut fm = FileManager::new(FileManagerConfig {
            root: missing.clone(),
            ..Default::default()
        });
        let Err(err) = fm.discover_and_parse() else {
            panic!("missing root must error");
        };
        assert!(
            err.to_string().contains(&missing.display().to_string()),
            "error must name the missing root, got: {err}"
        );
    }

    /// An existing-but-empty root still succeeds with no files; "empty" is the
    /// caller's policy call, only "not there" is an error here.
    #[test]
    fn discover_and_parse_empty_root_is_ok() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let mut fm = FileManager::new(FileManagerConfig {
            root: tmp.path().to_path_buf(),
            ..Default::default()
        });
        assert!(fm.discover_and_parse().expect("empty root").is_empty());
    }

    // ── symlink / special-file / budget hardening (#161) ───────────────────────

    /// A symlinked directory or file must not be walked: a dir symlink can point
    /// outside the root or into a cycle, and a file symlink can point at a
    /// special file (e.g. `/dev/zero`) that reports length 0 and reads to EOF.
    #[cfg(unix)]
    #[test]
    fn walk_workspace_files_rejects_symlinks() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path();

        // A real file that must be found, plus a dir symlink and a file symlink
        // that must be ignored.
        std::fs::write(root.join("real.txt"), "x").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("inside.txt"), "x").unwrap();
        symlink(root.join("sub"), root.join("dir_link")).unwrap();
        symlink(root.join("real.txt"), root.join("file_link.txt")).unwrap();
        // A symlink to a special file that reports length 0 and would read to EOF.
        symlink("/dev/zero", root.join("zero.txt")).unwrap();

        let files = walk_workspace_files(root, &["txt"], &[], &[], ScanBudget::default());
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"real.txt".to_string()));
        assert!(names.contains(&"inside.txt".to_string()));
        assert!(
            !names.iter().any(|n| n.contains("dir_link")),
            "dir symlink must not be followed: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("file_link")),
            "file symlink must be rejected: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("zero")),
            "symlink to /dev/zero must be rejected: {names:?}"
        );
    }

    /// The CLI discovery path must apply the same symlink policy as the LSP walk.
    #[cfg(unix)]
    #[test]
    fn collect_paths_rejects_symlinks() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path();
        std::fs::write(root.join("real.txt"), "x").unwrap();
        symlink(root.join("real.txt"), root.join("link.txt")).unwrap();

        let fm = FileManager::new(FileManagerConfig {
            root: root.to_path_buf(),
            include_dirs: vec![".".into()],
            ..Default::default()
        });
        let mut paths = Vec::new();
        fm.collect_paths(root, &mut paths).unwrap();
        let names: Vec<String> = paths.iter().map(|(_, lp)| lp.clone()).collect();
        assert!(names.iter().any(|n| n.ends_with("real.txt")));
        assert!(
            !names.iter().any(|n| n.ends_with("link.txt")),
            "file symlink must be rejected: {names:?}"
        );
    }

    /// The per-scan file-count budget stops a pathological tree from being
    /// walked to exhaustion.
    #[test]
    fn walk_workspace_files_enforces_file_budget() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path();
        for i in 0..10 {
            std::fs::write(root.join(format!("f{i}.txt")), "x").unwrap();
        }
        let budget = ScanBudget {
            max_files: 3,
            max_bytes: 0,
            max_file_size: 0,
        };
        let files = walk_workspace_files(root, &["txt"], &[], &[], budget);
        assert_eq!(files.len(), 3, "walk must stop at the file budget");
    }

    /// The multi-mod walk must reject symlinks too.
    #[cfg(unix)]
    #[test]
    fn multi_mod_walk_rejects_symlinks() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("moda/common")).unwrap();
        std::fs::write(root.join("moda/common/real.txt"), "x").unwrap();
        symlink(
            root.join("moda/common/real.txt"),
            root.join("moda/common/link.txt"),
        )
        .unwrap();

        let mods = vec![ResolvedMod {
            descriptor: ModDescriptor {
                name: "ModA".into(),
                path: Some(root.join("moda").to_str().unwrap().to_string()),
                replace_paths: vec![],
            },
            root: root.join("moda"),
        }];
        let files =
            discover_files_multi_mod(None, &mods, &["common".to_string()], ScanBudget::default());
        let names: Vec<String> = files.iter().map(|(_, lp)| lp.clone()).collect();
        assert!(names.iter().any(|n| n.ends_with("real.txt")));
        assert!(
            !names.iter().any(|n| n.ends_with("link.txt")),
            "multi-mod walk must reject file symlinks: {names:?}"
        );
    }

    /// `read_text_capped` refuses a file over the cap rather than truncating it,
    /// so a special file that reports length 0 can't be read to EOF.
    #[test]
    fn read_text_capped_refuses_over_limit() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let file = tmp.path().join("big.txt");
        std::fs::write(&file, "x".repeat(100)).unwrap();

        let ok = read_text_capped(&file, 1000).expect("under cap reads");
        assert_eq!(ok.0, "x".repeat(100));
        assert_eq!(ok.1, 100);

        let Err(err) = read_text_capped(&file, 50) else {
            panic!("over-cap file must be refused");
        };
        assert!(
            err.to_string().contains("read cap"),
            "error must name the cap, got: {err}"
        );
    }

    /// `discover_and_parse` must skip a file over the per-file cap instead of
    /// reading it to EOF (the CLI's metadata-only check didn't protect special
    /// files that report length 0).
    #[test]
    fn discover_and_parse_skips_over_limit_file() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path();
        std::fs::write(root.join("ok.txt"), "x = 1").unwrap();
        std::fs::write(root.join("big.txt"), "x = 1".repeat(1000)).unwrap();

        let mut fm = FileManager::new(FileManagerConfig {
            root: root.to_path_buf(),
            include_dirs: vec![".".into()],
            max_file_size: 100,
            ..Default::default()
        });
        let files = fm.discover_and_parse().expect("discover");
        let names: Vec<String> = files.iter().map(|f| f.logical_path.clone()).collect();
        assert!(names.iter().any(|n| n.ends_with("ok.txt")));
        assert!(
            !names.iter().any(|n| n.ends_with("big.txt")),
            "over-cap file must be skipped: {names:?}"
        );
    }

    #[test]
    fn is_ignored_logical_path_applies_engine_baseline() {
        // Baseline must win even with no user globs.
        assert!(is_ignored_logical_path("README.txt", &[]));
        assert!(is_ignored_logical_path("Changelog.txt", &[]));
        assert!(is_ignored_logical_path("LICENSE.txt", &[]));
        assert!(is_ignored_logical_path("docs/readme.md", &[]));
        assert!(is_ignored_logical_path("notes/README.md", &[]));
        assert!(!is_ignored_logical_path("common/ideas/foo.txt", &[]));
        assert!(!is_ignored_logical_path("events/kept.txt", &[]));
    }

    #[test]
    fn is_ignored_logical_path_bare_name_matches_any_depth() {
        let extra = vec!["ignored.txt".to_string()];
        assert!(is_ignored_logical_path("ignored.txt", &extra));
        assert!(is_ignored_logical_path("common/ignored.txt", &extra));
        assert!(is_ignored_logical_path("a/b/c/ignored.txt", &extra));
        assert!(!is_ignored_logical_path("kept.txt", &extra));
        assert!(!is_ignored_logical_path("common/kept.txt", &extra));
    }

    #[test]
    fn is_ignored_logical_path_path_globs_match_location() {
        assert!(is_ignored_logical_path(
            "common/units/skip.txt",
            &["**/skip.txt".to_string()]
        ));
        assert!(is_ignored_logical_path(
            "skip.txt",
            &["**/skip.txt".to_string()]
        ));
        assert!(!is_ignored_logical_path(
            "common/units/keep.txt",
            &["**/skip.txt".to_string()]
        ));
        assert!(is_ignored_logical_path(
            "common/units/skip.txt",
            &["common/**/skip.txt".to_string()]
        ));
        assert!(!is_ignored_logical_path(
            "skip.txt",
            &["common/**/skip.txt".to_string()]
        ));
        // Trailing slash means everything below.
        assert!(is_ignored_logical_path(
            "common/units/foo.txt",
            &["common/units/".to_string()]
        ));
        assert!(!is_ignored_logical_path(
            "common/other/foo.txt",
            &["common/units/".to_string()]
        ));
    }

    #[test]
    fn is_ignored_logical_path_empty_is_never_ignored() {
        assert!(!is_ignored_logical_path("", &[]));
        assert!(!is_ignored_logical_path("", &["ignored.txt".to_string()]));
    }

    #[test]
    fn is_ignored_file_wrapper_derives_logical_path() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path();
        // Inside root.
        let inside = root.join("common/ignored.txt");
        std::fs::create_dir_all(inside.parent().unwrap()).unwrap();
        std::fs::write(&inside, "").unwrap();
        assert!(is_ignored_file(root, &inside, &["ignored.txt".to_string()]));
        // Outside root falls back to filename (compute_logical_path) and still
        // matches a bare glob.
        let outside = tmp.path().join("../outside_ignored.txt");
        assert!(is_ignored_file(
            root,
            &outside,
            &["outside_ignored.txt".to_string()]
        ));
        assert!(!is_ignored_file(root, &inside, &[]));
        // Engine baseline via wrapper.
        let readme = root.join("README.txt");
        std::fs::write(&readme, "").unwrap();
        assert!(is_ignored_file(root, &readme, &[]));
    }

    #[test]
    fn is_ignored_logical_path_agrees_with_walk_workspace_files() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path();
        for rel in [
            "README.txt",
            "Changelog.txt",
            "docs/readme.md",
            "common/keep.txt",
            "common/ignored.txt",
            "events/skip.txt",
            "common/units/keep.txt",
        ] {
            if let Some(parent) = std::path::Path::new(rel).parent() {
                std::fs::create_dir_all(root.join(parent)).unwrap();
            }
            std::fs::write(root.join(rel), "").unwrap();
        }
        let extra = vec!["ignored.txt".to_string(), "**/skip.txt".to_string()];
        let walked = walk_workspace_files(root, &["txt", "md"], &extra, &[], ScanBudget::default());
        let walked_set: std::collections::HashSet<String> = walked
            .iter()
            .map(|p| compute_logical_path(p, root))
            .collect();
        for rel in [
            "README.txt",
            "Changelog.txt",
            "docs/readme.md",
            "common/ignored.txt",
            "events/skip.txt",
        ] {
            assert!(
                !walked_set.contains(rel),
                "walk should have excluded {rel}: {walked_set:?}"
            );
            assert!(
                is_ignored_logical_path(rel, &extra),
                "predicate must agree it is ignored: {rel}"
            );
        }
        for rel in ["common/keep.txt", "common/units/keep.txt"] {
            assert!(
                walked_set.contains(rel),
                "walk should have kept {rel}: {walked_set:?}"
            );
            assert!(
                !is_ignored_logical_path(rel, &extra),
                "predicate must agree it is kept: {rel}"
            );
        }
    }

    #[test]
    fn is_ignored_path_applies_dir_globs() {
        // Bare directory name matches at any depth.
        assert!(is_ignored_path("scratch/foo.txt", &[], &["scratch".into()]));
        assert!(is_ignored_path(
            "common/scratch/foo.txt",
            &[],
            &["scratch".into()]
        ));
        assert!(!is_ignored_path(
            "common/keep/foo.txt",
            &[],
            &["scratch".into()]
        ));
        // Path-aware directory glob.
        assert!(is_ignored_path(
            "common/scratch/foo.txt",
            &[],
            &["common/scratch".into()]
        ));
        assert!(!is_ignored_path(
            "events/scratch/foo.txt",
            &[],
            &["common/scratch".into()]
        ));
        assert!(is_ignored_path(
            "a/b/scratch/foo.txt",
            &[],
            &["**/scratch".into()]
        ));
        // File and directory globs stack.
        assert!(is_ignored_path(
            "scratch/README.txt",
            &[],
            &["scratch".into()]
        ));
        assert!(is_ignored_path(
            "common/ignored.txt",
            &["ignored.txt".into()],
            &[]
        ));
        assert!(is_ignored_path(
            "scratch/ignored.txt",
            &["ignored.txt".into()],
            &["scratch".into()]
        ));
    }

    #[test]
    fn is_ignored_path_uses_baseline_file_patterns() {
        assert!(is_ignored_path("README.txt", &[], &[]));
        assert!(is_ignored_path("docs/readme.md", &[], &[]));
        assert!(!is_ignored_path("common/ideas/foo.txt", &[], &[]));
    }

    #[test]
    fn is_ignored_path_handles_windows_separators() {
        assert!(is_ignored_path(
            "common\\scratch/foo.txt",
            &[],
            &["scratch".into()]
        ));
        assert!(is_ignored_path(
            "common/scratch/foo.txt",
            &[],
            &["common\\scratch".into()]
        ));
    }

    #[test]
    fn is_ignored_path_agrees_with_walk_workspace_files_for_dirs() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path();
        for rel in [
            "scratch/foo.txt",
            "common/scratch/foo.txt",
            "common/keep/foo.txt",
            "events/keep.txt",
        ] {
            if let Some(parent) = std::path::Path::new(rel).parent() {
                std::fs::create_dir_all(root.join(parent)).unwrap();
            }
            std::fs::write(root.join(rel), "").unwrap();
        }
        let extra_dirs = vec!["scratch".to_string(), "**/skip".to_string()];
        let walked = walk_workspace_files(root, &["txt"], &[], &extra_dirs, ScanBudget::default());
        let walked_set: std::collections::HashSet<String> = walked
            .iter()
            .map(|p| compute_logical_path(p, root))
            .collect();
        for rel in ["scratch/foo.txt", "common/scratch/foo.txt"] {
            assert!(
                !walked_set.contains(rel),
                "walk should have excluded {rel}: {walked_set:?}"
            );
            assert!(
                is_ignored_path(rel, &[], &extra_dirs),
                "predicate must agree it is ignored: {rel}"
            );
        }
        for rel in ["common/keep/foo.txt", "events/keep.txt"] {
            assert!(
                walked_set.contains(rel),
                "walk should have kept {rel}: {walked_set:?}"
            );
            assert!(
                !is_ignored_path(rel, &[], &extra_dirs),
                "predicate must agree it is kept: {rel}"
            );
        }
    }

    #[test]
    fn is_ignored_logical_path_handles_windows_separators() {
        // Pattern with \ must still match a forward-slashed logical path and vice versa.
        assert!(is_ignored_logical_path(
            "common\\ignored.txt",
            &["ignored.txt".to_string()]
        ));
        assert!(is_ignored_logical_path(
            "common/ignored.txt",
            &["common\\ignored.txt".to_string()]
        ));
        assert!(is_ignored_logical_path(
            "a\\b\\skip.txt",
            &["**/skip.txt".to_string()]
        ));
        assert!(is_ignored_logical_path(
            "a/b/skip.txt",
            &["**\\skip.txt".to_string()]
        ));
    }
}
