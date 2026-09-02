use cwtools_parser::ast::{Arena, Child, ParseError};
use cwtools_parser::parser::{parse_string, parse_string_without_comments};
use cwtools_string_table::string_table::StringTable;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Windows-1252 → Unicode mapping for the 0x80-0x9F range (the gap not covered
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
        b as char
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEncoding {
    /// Valid UTF-8 starting with the UTF-8 BOM (`EF BB BF`). What Paradox wants
    Utf8Bom,
    /// Valid UTF-8 but with no BOM.
    Utf8NoBom,
    /// Not valid UTF-8 (decoded via Windows-1252 fallback).
    NonUtf8,
}

const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

/// Read a file as text: try UTF-8 first, fall back to Windows-1252.
/// Windows-1252.  Blindly using `read_to_string` fails on any accented byte
pub fn read_text(path: &Path) -> Result<String, FileError> {
    read_text_with_encoding(path).map(|(s, _)| s)
}

/// enforce encoding rules (e.g. localisation must be UTF-8 BOM).
pub fn read_text_with_encoding(path: &Path) -> Result<(String, FileEncoding), FileError> {
    Ok(decode_bytes(std::fs::read(path)?))
}

/// Read a file as text through a hard byte cap. Opens once and reads at most
/// `max_bytes`; a file that reports a larger length, grows under us, or is a
/// so callers can enforce a per-scan total-byte budget.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Script,
    Localisation,
    Resource,
}

pub const SCRIPT_EXTENSIONS: &[&str] = &["txt", "gui", "gfx", "sfx", "asset", "map"];

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

pub const EXCLUDED_ROOT_DIRS: &[&str] = &["resources"];

pub fn is_excluded_dir(name: &str) -> bool {
    EXCLUDED_DIRS.iter().any(|d| name.eq_ignore_ascii_case(d))
}

pub fn is_excluded_root_dir(name: &str) -> bool {
    EXCLUDED_ROOT_DIRS
        .iter()
        .any(|d| name.eq_ignore_ascii_case(d))
}

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
    #[error("directory does not exist: {0}")]
    MissingRoot(PathBuf),
    /// A file exceeded the hard read cap for a scan. Distinct from a parse
    #[error("file exceeds the {limit} byte read cap: {path}")]
    OverLimit { path: PathBuf, limit: u64 },
}

#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub path: PathBuf,
    pub logical_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPath {
    pub path: PathBuf,
    pub root_relative_path: String,
    pub kind: FileKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryFailure {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Default)]
pub struct DiscoveryReport {
    pub files: Vec<DiscoveredPath>,
    pub failures: Vec<DiscoveryFailure>,
}

pub struct ParsedFile {
    pub path: PathBuf,
    pub logical_path: String,
    pub arena: Arena,
    pub root_children: Vec<Child>,
    pub errors: Vec<ParseError>,
}

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

#[derive(Clone)]
pub struct FileManagerConfig {
    pub root: PathBuf,
    pub include_dirs: Vec<String>,
    pub file_patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
    pub exclude_dirs: Vec<String>,
    pub exclude_dir_patterns: Vec<String>,
    pub exclude_root_dirs: Vec<String>,
    pub max_file_size: u64,
    /// Per-scan resource budget (file count and total bytes).
    pub scan_budget: ScanBudget,
}

/// Per-scan resource budget. Guards one discovery/read pass against a
/// pathological tree (a symlink to `/`, a special file that reports length 0,
#[derive(Debug, Clone, Copy)]
pub struct ScanBudget {
    pub max_files: usize,
    pub max_bytes: u64,
    /// Hard per-file read cap (bytes). 0 = no per-file cap.
    pub max_file_size: u64,
}

impl Default for ScanBudget {
    fn default() -> Self {
        Self {
            max_files: 100_000,
            max_bytes: 2 * 1024 * 1024 * 1024, // 2 GiB
            // script cap (2 MB) is separate (`FileManagerConfig::max_file_size`).
            max_file_size: 64 * 1024 * 1024, // 64 MB
        }
    }
}

/// Atomic running total of bytes read in one scan, shared across the parallel
/// read fan-out so the whole scan stops adding files once the total-byte budget
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

    fn collect_paths(&self, dir: &Path, out: &mut Vec<(PathBuf, String)>) -> Result<(), FileError> {
        let root_prefix = normalize_root_prefix(&self.config.root);
        let is_root_level = dir == self.config.root.as_path();
        let cfg = &self.config;
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

    pub fn discover_files_multi_mod(&self) -> Vec<DiscoveredFile> {
        let mods = expand_multiple_mods(&self.config.root);
        discover_files_multi_mod(
            None,
            &mods,
            &self.config.include_dirs,
            &self.config.exclude_dir_patterns,
            self.config.scan_budget,
        )
        .into_iter()
        .filter(|(path, logical_path)| accept_script_file(&self.config, path, logical_path))
        .map(|(path, logical_path)| DiscoveredFile { path, logical_path })
        .collect()
    }

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

pub fn discover_paths(
    roots: &[&Path],
    config: &FileManagerConfig,
    kinds: &[FileKind],
) -> Result<DiscoveryReport, FileError> {
    let mut report = DiscoveryReport::default();

    for root in roots {
        if !root.is_dir() {
            return Err(FileError::MissingRoot((*root).to_path_buf()));
        }
        let root_prefix = normalize_root_prefix(root);
        let mut accept = |path: &Path| {
            let root_relative_path = compute_logical_path_with_root(path, &root_prefix);
            accept_discovered_path(config, kinds, path, root_relative_path)
        };
        let mut on_err = |path: &Path, error: std::io::Error| {
            report.failures.push(DiscoveryFailure {
                path: path.to_path_buf(),
                error: error.to_string(),
            });
        };
        let mut state = WalkState {
            out: Vec::new(),
            remaining_files: config.scan_budget.max_files,
        };
        walk_dir_generic(
            root,
            WalkRoot {
                prefix: &root_prefix,
                is_root_level: true,
            },
            config,
            &[],
            &mut accept,
            &mut on_err,
            &mut state,
        )
        .map_err(FileError::from)?;
        report.files.extend(state.out);
    }
    Ok(report)
}

pub fn discover_paths_multi_mod(config: &FileManagerConfig, kinds: &[FileKind]) -> DiscoveryReport {
    let include_dirs = [".".to_string()];
    let mods = expand_multiple_mods(&config.root);
    let files = discover_files_multi_mod(
        None,
        &mods,
        &include_dirs,
        &config.exclude_dir_patterns,
        config.scan_budget,
    );
    DiscoveryReport {
        files: files
            .into_iter()
            .filter_map(|(path, root_relative_path)| {
                accept_discovered_path(config, kinds, &path, root_relative_path)
            })
            .collect(),
        failures: Vec::new(),
    }
}

fn accept_discovered_path(
    config: &FileManagerConfig,
    kinds: &[FileKind],
    path: &Path,
    root_relative_path: String,
) -> Option<DiscoveredPath> {
    let kind = classify_extension(path);
    if !kinds.contains(&kind) {
        return None;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if config
        .exclude_patterns
        .iter()
        .any(|pattern| ignore_glob_match(pattern, file_name, &root_relative_path))
    {
        return None;
    }
    if kind == FileKind::Script
        && (!is_included_script_path(&root_relative_path, &config.include_dirs)
            || !accept_script_file(config, path, &root_relative_path))
    {
        return None;
    }
    if kind == FileKind::Localisation
        && (!is_localisation_path(&root_relative_path) || has_hidden_component(&root_relative_path))
    {
        return None;
    }
    Some(DiscoveredPath {
        path: path.to_path_buf(),
        root_relative_path,
        kind,
    })
}

fn is_included_script_path(path: &str, include_dirs: &[String]) -> bool {
    include_dirs.iter().any(|dir| {
        if dir == "." {
            return true;
        }
        let dir = dir.replace('\\', "/").trim_matches('/').to_string();
        path.strip_prefix(&dir)
            .is_some_and(|rest| rest.starts_with('/'))
    })
}

fn is_localisation_path(path: &str) -> bool {
    path.split('/').any(|component| {
        component.eq_ignore_ascii_case("localisation")
            || component.eq_ignore_ascii_case("localisation_synced")
            || component.eq_ignore_ascii_case("localization")
    })
}

fn has_hidden_component(path: &str) -> bool {
    path.split('/').any(|component| component.starts_with('.'))
}

pub(crate) fn compute_logical_path(path: &Path, root: &Path) -> String {
    compute_logical_path_with_root(path, &normalize_root_prefix(root))
}

fn normalize_root_prefix(root: &Path) -> String {
    let s = normalize_slashes(root.to_string_lossy());
    if s.ends_with('/') {
        s.into_owned()
    } else {
        format!("{}/", s)
    }
}

fn compute_logical_path_with_root(path: &Path, root_prefix: &str) -> String {
    let path_str = normalize_slashes(path.to_string_lossy());

    if let Some(rel) = path_str.strip_prefix(root_prefix) {
        rel.to_string()
    } else {
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string()
    }
}

/// Windows hands back `\` separators, so a match target passes through here
pub fn to_slash_path(path: &Path) -> String {
    normalize_slashes(path.to_string_lossy()).into_owned()
}

/// the string contains none (the common case on Unix).
fn normalize_slashes(s: std::borrow::Cow<'_, str>) -> std::borrow::Cow<'_, str> {
    if s.contains('\\') {
        std::borrow::Cow::Owned(s.replace('\\', "/"))
    } else {
        s
    }
}

/// Mirrors F# FileManager.fs:91-125: extracts `name`, `path`, and
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

/// (`replace_path = "common/ideas" # keep` -> `common/ideas`). An unquoted value
/// runs up to an inline `#` comment. The old `trim_matches('"')` left the closing
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

#[derive(Debug, Clone)]
pub struct ResolvedMod {
    pub descriptor: ModDescriptor,
    pub root: PathBuf,
}

/// Mirrors F# FileManager.fs:64-90: reads every `*.mod` file inside the
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

    out.sort_by(|a, b| a.descriptor.name.cmp(&b.descriptor.name));
    out
}

/// Mirrors F# FileManager.fs:91-147:
pub fn discover_files_multi_mod(
    vanilla_root: Option<&Path>,
    mods: &[ResolvedMod],
    include_dirs: &[String],
    exclude_dir_patterns: &[String],
    budget: ScanBudget,
) -> Vec<(PathBuf, String)> {
    use std::collections::HashMap;

    let mut best: HashMap<String, (PathBuf, usize)> = HashMap::new();
    let mut remaining = budget.max_files;

    let mut sources: Vec<(usize, &Path, &[String])> = Vec::new();

    if let Some(v) = vanilla_root {
        sources.push((0, v, include_dirs));
    }
    for (i, m) in mods.iter().enumerate() {
        sources.push((i + 1, &m.root, include_dirs));
    }

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
            collect_files_recursive(
                &dir,
                &root_prefix,
                *priority,
                exclude_dir_patterns,
                &mut best,
                &mut remaining,
            );
        }
    }

    let logical_lower: HashMap<String, String> = best
        .keys()
        .map(|k| (k.clone(), k.to_ascii_lowercase()))
        .collect();
    for (i, m) in mods.iter().enumerate().rev() {
        let mod_priority = i + 1;
        for rp in &m.descriptor.replace_paths {
            // Normalize: backslash → slash (Windows-authored .mod files), trim
            let prefix_lower = rp.replace('\\', "/").trim_matches('/').to_ascii_lowercase();
            let prefix_lower_slash = format!("{}/", prefix_lower);
            best.retain(|logical, (_path, file_prio)| {
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
    exclude_dir_patterns: &[String],
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
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let dir_relative = if has_path_pattern(exclude_dir_patterns) {
                compute_logical_path_with_root(&path, root_prefix)
            } else {
                String::new()
            };
            let skip = is_excluded_dir(name)
                || exclude_dir_patterns
                    .iter()
                    .any(|pat| ignore_glob_match(pat, name, &dir_relative));
            if skip {
                continue;
            }
            collect_files_recursive(
                &path,
                root_prefix,
                priority,
                exclude_dir_patterns,
                out,
                remaining_files,
            );
        } else {
            *remaining_files -= 1;
            let logical = compute_logical_path_with_root(&path, root_prefix);
            let entry = out.entry(logical).or_insert((path.clone(), priority));
            if priority > entry.1 {
                *entry = (path, priority);
            }
        }
    }
}

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
    let needs_relative =
        has_path_pattern(&cfg.exclude_patterns) || has_path_pattern(extra_file_globs);
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

/// `state.out`. Symlinks and non-regular files (fifos, sockets, devices) are
/// rejected outright: a symlink can point outside the root or into a cycle,
/// and the remaining per-scan file budget.
struct WalkState<T> {
    out: Vec<T>,
    remaining_files: usize,
}

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
    let dir_paths_needed =
        has_path_pattern(&cfg.exclude_dir_patterns) || has_path_pattern(extra_dir_globs);
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
pub fn classify_directory(dir: &Path) -> DirectoryType {
    let looks_like_game_folder = |d: &Path| -> bool {
        for sub in &["common", "events", "interface", "gfx", "localisation"] {
            if d.join(sub).is_dir() {
                return true;
            }
        }
        false
    };

    let game_sub = dir.join("game");
    if game_sub.is_dir() && looks_like_game_folder(&game_sub) {
        return DirectoryType::Vanilla;
    }

    if looks_like_game_folder(dir) {
        return DirectoryType::Mod;
    }

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

pub fn glob_match(pattern: &str, text: &str) -> bool {
    if !pattern.contains(['*', '?']) {
        return pattern == text;
    }
    if let Some(suffix) = pattern.strip_prefix('*')
        && !suffix.contains(['*', '?'])
    {
        return text.ends_with(suffix);
    }
    if let Some(prefix) = pattern.strip_suffix('*')
        && !prefix.contains(['*', '?'])
    {
        return text.starts_with(prefix);
    }
    glob_match_general(pattern, text)
}

/// which the callers' pattern-length caps keep bounded (#169).
fn glob_match_general(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_greedy(&p, &t)
}

/// Windows users write `\`, so both separators count.
fn is_path_pattern(pattern: &str) -> bool {
    pattern.contains(['/', '\\'])
}

fn has_path_pattern(patterns: &[String]) -> bool {
    patterns.iter().any(|p| is_path_pattern(p))
}

/// file it names instead of nothing at all (#244).
pub fn ignore_glob_match(pattern: &str, name: &str, relative: &str) -> bool {
    if is_path_pattern(pattern) {
        path_glob_match(pattern, relative)
    } else {
        glob_match(pattern, name)
    }
}

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

pub fn is_ignored_file(
    root: &std::path::Path,
    path: &std::path::Path,
    extra_file_globs: &[String],
) -> bool {
    let logical = compute_logical_path(path, root);
    is_ignored_logical_path(&logical, extra_file_globs)
}

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
            star = Some(j);
            j += 1;
            mark = i;
        } else if j < m && (p[j] == '?' || p[j] == t[i]) {
            i += 1;
            j += 1;
        } else if let Some(s) = star {
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
        assert!(glob_match("*foo*", "barfoobar"));
        assert!(glob_match("*foo*", "foo"));
        assert!(glob_match("*foo*", "xfoox"));
        assert!(!glob_match("*foo*", "bar"));
        assert!(glob_match("fo?*", "foobar"));
        assert!(!glob_match("fo?*", "fo")); // needs at least one char after "fo"
    }

    #[test]
    fn path_globs_match_a_location_not_just_a_name() {
        // #244: the VS Code client rewrites every `errors.ignorefiles` entry to
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
        assert!(ignore_glob_match(
            "**/99_README**.txt",
            "99_README_units.txt",
            "common/99_README_units.txt"
        ));
        assert!(ignore_glob_match("*.md", "notes.md", "docs/notes.md"));
        assert!(ignore_glob_match("*.md", "notes.md", ""));
        assert!(!ignore_glob_match("*.md", "notes.txt", "docs/notes.txt"));
    }

    #[test]
    fn path_globs_anchor_at_the_root_and_respect_segments() {
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
        assert!(ignore_glob_match(
            "/common/foo.txt",
            "foo.txt",
            "common/foo.txt"
        ));
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
        let pat = "?".repeat(1024 * 1024);
        let text = "a".repeat(255);
        assert!(!glob_match(&pat, &text));
        // Star-segment worst case at the config length cap. The '?' keeps the
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

    /// discovery, so the two paths agree on directory ignore semantics (#412).
    #[test]
    fn exclude_dir_patterns_skips_matching_dirs_multi_mod() {
        use std::fs;
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let ws = tmp.path();

        fs::create_dir_all(ws.join("mod")).unwrap();
        fs::write(
            ws.join("mod/alpha.mod"),
            "name = \"Alpha Mod\"\npath = \"alpha\"\n",
        )
        .unwrap();
        fs::create_dir_all(ws.join("alpha/common/temp")).unwrap();
        fs::create_dir_all(ws.join("alpha/common/template")).unwrap();
        fs::write(ws.join("alpha/common/keep.txt"), "x = 1").unwrap();
        fs::write(ws.join("alpha/common/temp/skip.txt"), "x = 1").unwrap();
        fs::write(ws.join("alpha/common/template/keep2.txt"), "x = 1").unwrap();

        let fm = FileManager::new(FileManagerConfig {
            root: ws.to_path_buf(),
            include_dirs: vec!["common".into()],
            exclude_dir_patterns: vec!["temp".into()],
            ..Default::default()
        });
        let names: Vec<String> = fm
            .discover_files_multi_mod()
            .into_iter()
            .map(|f| f.logical_path)
            .collect();

        assert!(
            names.iter().any(|n| n.ends_with("common/keep.txt")),
            "keep must survive: {names:?}"
        );
        assert!(
            names
                .iter()
                .any(|n| n.ends_with("common/template/keep2.txt")),
            "template/ must NOT match the exact 'temp' pattern: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.ends_with("common/temp/skip.txt")),
            "temp/ must be skipped by exclude_dir_patterns in multi-mod: {names:?}"
        );
    }

    #[test]
    fn root_resources_skipped_but_common_resources_indexed() {
        use std::fs;
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path();
        for rel in ["common/resources/00_resources.txt", "resources/scratch.txt"] {
            fs::create_dir_all(root.join(Path::new(rel).parent().unwrap())).unwrap();
            fs::write(root.join(rel), "").unwrap();
        }

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

    #[test]
    fn cp1252_e_acute_0xe9() {
        assert_eq!(cp1252_byte(0xE9), 'é');
    }

    #[test]
    fn cp1252_euro_sign_0x80() {
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

        let bytes: &[u8] = b"caf\xE9\n";
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        tmp.write_all(bytes).expect("write");

        let text = read_text(tmp.path()).expect("read_text");
        assert_eq!(text, "caf\u{E9}\n", "0xE9 should decode as é (U+00E9)");
    }

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

    #[test]
    fn multi_mod_replace_path_suppresses_vanilla() {
        use std::collections::HashMap;
        use std::fs;

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
        let files = discover_files_multi_mod(
            Some(&vanilla),
            &mods,
            &include_dirs,
            &[],
            ScanBudget::default(),
        );

        let by_logical: HashMap<String, String> = files
            .iter()
            .map(|(abs, logical)| {
                let content = fs::read_to_string(abs).unwrap_or_default();
                (logical.clone(), content)
            })
            .collect();

        assert_eq!(
            by_logical.get("common/foo.txt").map(|s| s.as_str()),
            Some("moda"),
            "ModA's common/foo.txt should win; vanilla suppressed by replace_path"
        );

        assert!(
            by_logical.contains_key("events/bar.txt"),
            "ModB events/bar.txt should be present"
        );
    }

    #[test]
    fn multi_mod_workspace_expands_and_overrides() {
        use std::collections::HashMap;
        use std::fs;

        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let ws = tmp.path();

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

        fs::create_dir_all(ws.join("alpha/common")).unwrap();
        fs::create_dir_all(ws.join("alpha/localisation")).unwrap();
        fs::write(ws.join("alpha/common/foo.txt"), "shared = alpha").unwrap();
        fs::write(ws.join("alpha/common/only_a.txt"), "only = alpha").unwrap();
        fs::write(ws.join("alpha/common/README.md"), "notes").unwrap();
        fs::write(ws.join("alpha/localisation/x.yml"), "l_english:").unwrap();

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

        let foo = by_logical
            .get("common/foo.txt")
            .expect("common/foo.txt present");
        assert_eq!(
            fs::read_to_string(foo).unwrap(),
            "shared = bravo",
            "Bravo's common/foo.txt overrides Alpha's"
        );
        assert!(by_logical.contains_key("common/only_a.txt"));
        assert!(by_logical.contains_key("events/e.txt"));
        assert!(
            !by_logical.keys().any(|k| k.ends_with("README.md")),
            "*.md excluded: {by_logical:?}"
        );
        assert!(
            !by_logical.keys().any(|k| k.ends_with(".yml")),
            "loc files are not script-discovered: {by_logical:?}"
        );
    }

    #[test]
    fn single_mod_directory_classifies_as_mod() {
        use std::fs;
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path();
        fs::create_dir_all(root.join("common")).unwrap();
        fs::create_dir_all(root.join("events")).unwrap();
        fs::write(root.join("common/foo.txt"), "x = 1").unwrap();

        assert_eq!(classify_directory(root), DirectoryType::Mod);

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

        let anchored = names(&["common/**/skip.txt".to_string()], &[]);
        assert!(
            anchored.contains(&"skip.txt".to_string()),
            "got: {anchored:?}"
        );
        assert!(
            !anchored.contains(&"common/units/skip.txt".to_string()),
            "got: {anchored:?}"
        );

        let pruned = names(&[], &["common/units".to_string()]);
        assert_eq!(pruned, ["skip.txt"], "got: {pruned:?}");
    }

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
    #[cfg(unix)]
    #[test]
    fn walk_workspace_files_rejects_symlinks() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path();

        // A real file that must be found, plus a dir symlink and a file symlink
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
        let files = discover_files_multi_mod(
            None,
            &mods,
            &["common".to_string()],
            &[],
            ScanBudget::default(),
        );
        let names: Vec<String> = files.iter().map(|(_, lp)| lp.clone()).collect();
        assert!(names.iter().any(|n| n.ends_with("real.txt")));
        assert!(
            !names.iter().any(|n| n.ends_with("link.txt")),
            "multi-mod walk must reject file symlinks: {names:?}"
        );
    }

    /// `read_text_capped` refuses a file over the cap rather than truncating it,
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
        let inside = root.join("common/ignored.txt");
        std::fs::create_dir_all(inside.parent().unwrap()).unwrap();
        std::fs::write(&inside, "").unwrap();
        assert!(is_ignored_file(root, &inside, &["ignored.txt".to_string()]));
        let outside = tmp.path().join("../outside_ignored.txt");
        assert!(is_ignored_file(
            root,
            &outside,
            &["outside_ignored.txt".to_string()]
        ));
        assert!(!is_ignored_file(root, &inside, &[]));
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

    #[test]
    fn discover_paths_applies_kind_and_ignore_policy_once() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for path in [
            "common/keep.txt",
            "localisation/keep_l_english.yml",
            "localisation/ignored_l_english.yml",
            ".cache/localisation/hidden_l_english.yml",
            "gfx/icon.dds",
        ] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let mut config = FileManagerConfig {
            root: root.to_path_buf(),
            ..Default::default()
        };
        config
            .exclude_patterns
            .push("localisation/ignored_l_english.yml".to_string());

        let report = discover_paths(
            &[root],
            &config,
            &[FileKind::Script, FileKind::Localisation, FileKind::Resource],
        )
        .unwrap();
        let files: Vec<(FileKind, String)> = report
            .files
            .iter()
            .map(|file| (file.kind, file.root_relative_path.clone()))
            .collect();

        assert_eq!(
            files,
            vec![
                (FileKind::Script, "common/keep.txt".to_string()),
                (FileKind::Resource, "gfx/icon.dds".to_string()),
                (
                    FileKind::Localisation,
                    "localisation/keep_l_english.yml".to_string()
                ),
            ]
        );
        assert!(report.failures.is_empty());
    }

    #[test]
    fn discover_paths_enforces_the_budget_per_root() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for root in [first.path(), second.path()] {
            let path = root.join("localisation/test_l_english.yml");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let config = FileManagerConfig {
            root: first.path().to_path_buf(),
            scan_budget: ScanBudget {
                max_files: 1,
                ..Default::default()
            },
            ..Default::default()
        };

        let report = discover_paths(
            &[first.path(), second.path()],
            &config,
            &[FileKind::Localisation],
        )
        .unwrap();

        assert_eq!(report.files.len(), 2);
        assert!(report.files[0].path.starts_with(first.path()));
        assert!(report.files[1].path.starts_with(second.path()));
    }

    #[test]
    fn discover_paths_multi_mod_layers_localisation_and_resources() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for (name, path, replace_path) in
            [("a", "mods/a", None), ("b", "mods/b", Some("localisation"))]
        {
            let descriptor = root.join("mod").join(format!("{name}.mod"));
            std::fs::create_dir_all(descriptor.parent().unwrap()).unwrap();
            let replace_path = replace_path
                .map(|path| format!("replace_path = \"{path}\"\n"))
                .unwrap_or_default();
            std::fs::write(
                descriptor,
                format!("name = \"{name}\"\npath = \"{path}\"\n{replace_path}"),
            )
            .unwrap();
        }
        for path in [
            "mods/a/localisation/shared_l_english.yml",
            "mods/a/gfx/shared.dds",
            "mods/b/localisation/shared_l_english.yml",
            "mods/b/gfx/shared.dds",
        ] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let config = FileManagerConfig {
            root: root.to_path_buf(),
            ..Default::default()
        };

        let report =
            discover_paths_multi_mod(&config, &[FileKind::Localisation, FileKind::Resource]);

        assert_eq!(report.files.len(), 2);
        assert!(
            report
                .files
                .iter()
                .all(|file| file.path.starts_with(root.join("mods/b")))
        );
        assert_eq!(
            report
                .files
                .iter()
                .map(|file| file.root_relative_path.as_str())
                .collect::<Vec<_>>(),
            ["gfx/shared.dds", "localisation/shared_l_english.yml"]
        );
    }

    #[test]
    fn discover_paths_missing_root_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("missing");
        let config = FileManagerConfig {
            root: missing.clone(),
            ..Default::default()
        };

        let error = discover_paths(&[&missing], &config, &[FileKind::Localisation]).unwrap_err();

        assert!(matches!(error, FileError::MissingRoot(path) if path == missing));
    }
}
