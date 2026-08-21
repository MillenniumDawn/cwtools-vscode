//! Localization service.
//!
//! Aggregates loc files across multiple directories. Entries are owned once in
//! `files`; per-language / per-key views are derived on demand.
//!
//! Mirrors F# `LocalisationManager.fs`.

use crate::commands::{Lang, LocFile};
use crate::csv_parser::parse_csv_loc_per_lang;
use crate::yaml_parser::{LocFileParseError, parse_loc_text};
use cwtools_file_manager::file_manager::{ignore_glob_match, to_slash_path};
use cwtools_file_manager::{
    FileEncoding, ScanBudget, ScanBytes, is_excluded_dir, is_excluded_root_dir, is_loc_ext,
    read_text_capped_with_encoding,
};
use std::path::{Path, PathBuf};

/// A multi-file localization service for a single game.
///
/// Loc entries are owned exactly once, in `files`. Per-language and per-key
/// views are derived on demand (or by [`crate::loc_index::LocIndex`]) rather
/// than stored as a second copy — for large projects (Millennium Dawn ships
/// ~2M loc entries) a second owned copy dominated the heap.
pub struct LocService {
    /// Every successfully parsed loc file, in load order.
    files: Vec<LocFile>,
    /// (path, parse error) for files that failed to parse.
    errors: Vec<(String, String)>,
}

impl LocService {
    /// Create from a list of (file_path, file_text) pairs. Encoding is unknown
    /// (no CW254 check) — use [`LocService::from_folder`] when bytes are on disk.
    pub fn from_files(files: Vec<(String, String)>) -> Self {
        Self::from_files_with_encoding(files.into_iter().map(|(p, t)| (p, t, None)).collect())
    }

    /// As [`from_files`], but each file carries its detected on-disk encoding so
    /// the UTF-8-BOM rule (CW254) can be enforced.
    pub fn from_files_with_encoding(files: Vec<(String, String, Option<FileEncoding>)>) -> Self {
        use rayon::prelude::*;

        // Parsing is independent per file; run it in parallel, preserving input
        // order (`par_iter` over the indexed Vec) so first-seen-wins semantics
        // and diagnostics order are unchanged.
        let results: Vec<Result<Vec<LocFile>, (String, String)>> = files
            .into_par_iter()
            .map(|(path, text, encoding)| parse_loc_file_entry(path, text, encoding))
            .collect();
        Self::from_results(results)
    }

    /// Collect the parallel per-file parse results into the service, preserving
    /// input order for first-seen-wins semantics and diagnostics order.
    fn from_results(results: Vec<Result<Vec<LocFile>, (String, String)>>) -> Self {
        let mut parsed: Vec<LocFile> = Vec::new();
        let mut errors: Vec<(String, String)> = Vec::new();
        for r in results {
            match r {
                Ok(files) => parsed.extend(files),
                Err(e) => errors.push(e),
            }
        }
        Self {
            files: parsed,
            errors,
        }
    }

    /// Load from a directory tree (recursively).
    pub fn from_folder(folder: &Path, budget: ScanBudget) -> Self {
        Self::from_folders_filtered(&[folder], budget, None, &[], &[])
    }

    /// Load from several directory trees (e.g. a mod dir plus the vanilla
    /// install). Later folders' keys join the union; duplicate keys keep the
    /// first-seen entry per language.
    pub fn from_folders(folders: &[&Path], budget: ScanBudget) -> Self {
        Self::from_folders_filtered(folders, budget, None, &[], &[])
    }

    /// Load loc files while pruning user-excluded paths during discovery and
    /// unselected YAML languages before the full parse.
    pub fn from_folders_filtered(
        folders: &[&Path],
        budget: ScanBudget,
        langs: Option<&[Lang]>,
        ignore_files: &[String],
        ignore_dirs: &[String],
    ) -> Self {
        let paths = Self::discover_files_filtered(folders, budget, ignore_files, ignore_dirs);
        Self::from_paths(paths, budget, langs)
    }

    /// Discover the on-disk loc file paths `from_folders` would parse,
    /// without reading or parsing them. For callers that only need a cheap
    /// stat-based signature over the loc tree (e.g. the LSP's quiet
    /// background rescan deciding whether to skip a full loc rebuild).
    pub fn discover_files(folders: &[&Path], budget: ScanBudget) -> Vec<PathBuf> {
        Self::discover_files_filtered(folders, budget, &[], &[])
    }

    /// Discover loc files with user globs applied before ignored paths consume
    /// the file budget.
    pub fn discover_files_filtered(
        folders: &[&Path],
        budget: ScanBudget,
        ignore_files: &[String],
        ignore_dirs: &[String],
    ) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for folder in folders {
            paths.extend(walk_folder(folder, budget, ignore_files, ignore_dirs));
        }
        paths
    }

    /// Read and parse a set of loc files in parallel. Reading (disk I/O) happens
    /// inside the parallel map alongside parsing — mirroring the CLI's
    /// `discover_and_parse` — so a large loc tree isn't read sequentially before
    /// the parse fans out.
    fn from_paths(paths: Vec<PathBuf>, budget: ScanBudget, langs: Option<&[Lang]>) -> Self {
        use rayon::prelude::*;
        let bytes = ScanBytes::new();
        let results: Vec<Result<Vec<LocFile>, (String, String)>> = paths
            .into_par_iter()
            .map(|path| {
                let path_str = path.to_string_lossy().to_string();
                match read_text_capped_with_encoding(&path, budget.max_file_size) {
                    Ok((text, enc, n)) => {
                        if !bytes.try_reserve(n, budget.max_bytes) {
                            return Err((path_str, "scan byte budget exceeded".to_string()));
                        }
                        let is_csv = path
                            .extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("csv"));
                        if !is_csv
                            && let Some(langs) = langs
                            && let Some(lang) = loc_header_language(&text, &path_str)
                            && !langs.contains(&lang)
                        {
                            return Ok(Vec::new());
                        }
                        parse_loc_file_entry(path_str, text, Some(enc))
                    }
                    Err(e) => Err((path_str, format!("IO error: {}", e))),
                }
            })
            .collect();
        Self::from_results(results)
    }

    /// Append another service's parsed files and errors in discovery order.
    pub fn merge_from(&mut self, mut other: Self) {
        self.files.append(&mut other.files);
        self.errors.append(&mut other.errors);
    }

    /// All successfully parsed loc files (the single owner of loc entries).
    pub fn files(&self) -> &[LocFile] {
        &self.files
    }

    /// Files that failed to parse, as `(path, error)`.
    pub fn errors(&self) -> &[(String, String)] {
        &self.errors
    }

    /// Languages that actually have loc data loaded.
    pub fn languages(&self) -> Vec<Lang> {
        let mut langs: Vec<Lang> = Vec::new();
        for f in &self.files {
            if let Some(l) = f.lang
                && !langs.contains(&l)
            {
                langs.push(l);
            }
        }
        langs
    }
}

/// Parse one loc file's text. CSV files (CK2/VIC2) are routed through
/// `csv_parser` (one `LocFile` per language present); everything else goes
/// through `parse_loc_text` (YAML).
fn parse_loc_file_entry(
    path: String,
    text: String,
    encoding: Option<FileEncoding>,
) -> Result<Vec<LocFile>, (String, String)> {
    parse_loc_files(&path, &text, encoding).map_err(|e| (path, e.to_string()))
}

/// Parse one loc file's text into its [`LocFile`]s: a `.csv` yields one per
/// language present in it, a `.yml` exactly one.
///
/// Borrows its inputs, so a caller that needs the keys, the diagnostics and the
/// display text of the same buffer can parse it once and share the result
/// instead of handing an owned copy to a fresh [`LocService`] per use (#87).
pub fn parse_loc_files(
    path: &str,
    text: &str,
    encoding: Option<FileEncoding>,
) -> Result<Vec<LocFile>, LocFileParseError> {
    let is_csv = Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("csv"));
    if is_csv {
        // CSV: produce one LocFile per language present in the file.
        let entries_by_lang = parse_csv_loc_per_lang(text, path, None);
        let mut by_lang: std::collections::HashMap<Lang, Vec<crate::commands::LocEntry>> =
            std::collections::HashMap::new();
        for (_key, lang, entry) in entries_by_lang {
            by_lang.entry(lang).or_default().push(entry);
        }
        let loc_files: Vec<LocFile> = by_lang
            .into_iter()
            .map(|(lang, entries)| LocFile {
                path: path.to_string(),
                language_prefix: lang.to_string(),
                lang: Some(lang),
                is_csv: true,
                entries,
                parse_errors: Vec::new(),
                encoding,
            })
            .collect();
        Ok(loc_files)
    } else {
        match parse_loc_text(text, path) {
            Ok(mut file) => {
                file.encoding = encoding;
                Ok(vec![file])
            }
            Err(e) => Err(e), // LocFileParseError
        }
    }
}

/// True for a directory name the game treats as a localisation root.
fn is_loc_dir_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "localisation" || lower == "localisation_synced" || lower == "localization"
}

/// Tooling / VCS / build directories that never hold game loc. Skipped during the
/// walk so a mirror of the mod tree (e.g. a `.claude/worktrees/<wt>/localisation`,
/// a `.git` checkout, or `node_modules`) isn't loaded and double-counted. Shares
/// `FileManager`'s exclusion set so the two walkers stay consistent; any
/// dot-directory is additionally skipped, and the root-anchored `resources/`
/// exclusion applies only to a direct child of the walk root.
fn is_excluded_loc_dir(name: &str, at_root: bool) -> bool {
    name.starts_with('.') || is_excluded_dir(name) || (at_root && is_excluded_root_dir(name))
}

fn walk_folder(
    folder: &Path,
    budget: ScanBudget,
    ignore_files: &[String],
    ignore_dirs: &[String],
) -> Vec<PathBuf> {
    // Only files under a `localisation` (or `localization`) directory are loc —
    // that's what the game and F# load. Scanning every `.yml` in the tree pulls
    // in CI workflows, editor caches, and staging copies as bogus loc files
    // (false CW254/CW268) and wastes memory on data the game never reads.
    let mut remaining = budget.max_files;
    walk_folder_inner(
        folder,
        folder,
        false,
        true,
        &mut remaining,
        ignore_files,
        ignore_dirs,
    )
}

fn walk_folder_inner(
    root: &Path,
    folder: &Path,
    in_loc: bool,
    at_root: bool,
    remaining_files: &mut usize,
    ignore_files: &[String],
    ignore_dirs: &[String],
) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(rd) = std::fs::read_dir(folder) else {
        return files;
    };
    let mut entries: Vec<(std::ffi::OsString, PathBuf, std::fs::FileType)> = Vec::new();
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        entries.push((entry.file_name(), entry.path(), ft));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    for (name, path, ft) in entries {
        if *remaining_files == 0 {
            break;
        }
        // Reject symlinks and non-regular files outright (see
        // `file_manager::walk_dir_generic`): a symlink can point outside the
        // root or into a cycle, and a special file can report length 0.
        if ft.is_symlink() || !(ft.is_dir() || ft.is_file()) {
            continue;
        }
        let name = name.to_str().unwrap_or("");
        let relative = path.strip_prefix(root).unwrap_or(&path);
        let relative = to_slash_path(relative);
        if ft.is_dir() {
            if is_excluded_loc_dir(name, at_root)
                || ignore_dirs
                    .iter()
                    .any(|pattern| ignore_glob_match(pattern, name, &relative))
            {
                continue;
            }
            let child_in_loc = in_loc || is_loc_dir_name(name);
            files.extend(walk_folder_inner(
                root,
                &path,
                child_in_loc,
                false,
                remaining_files,
                ignore_files,
                ignore_dirs,
            ));
        } else if in_loc
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(is_loc_ext)
            && !ignore_files
                .iter()
                .any(|pattern| ignore_glob_match(pattern, name, &relative))
        {
            *remaining_files -= 1;
            files.push(path);
        }
    }

    files
}

fn loc_header_language(text: &str, path: &str) -> Option<Lang> {
    let text = text.trim_start_matches('\u{feff}');
    let mut end = 0;
    for line in text.split_inclusive('\n') {
        end += line.len();
        let trimmed = line.trim_start();
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            break;
        }
    }
    parse_loc_text(&text[..end], path).ok().and_then(|f| f.lang)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excluded_loc_dirs_skip_tooling_not_content() {
        for skip in [
            ".claude",
            ".git",
            ".vscode",
            ".idea",
            "node_modules",
            "target",
            "out",
            "dist",
            "bin",
            "obj",
        ] {
            assert!(is_excluded_loc_dir(skip, false), "{skip} should be skipped");
        }
        for keep in ["localisation", "localization", "common", "english"] {
            assert!(!is_excluded_loc_dir(keep, false), "{keep} should be walked");
        }
        // `resources` is excluded only at the walk root.
        assert!(is_excluded_loc_dir("resources", true));
        assert!(!is_excluded_loc_dir("resources", false));
    }

    #[test]
    fn from_files_parses_yaml_and_records_language() {
        let svc = LocService::from_files(vec![(
            "mod/localisation/english/test_l_english.yml".to_string(),
            r#"l_english:
 my_key:0 "value"
"#
            .to_string(),
        )]);
        assert!(svc.errors().is_empty(), "{:?}", svc.errors());
        let files = svc.files();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].lang, Some(Lang::English));
        assert_eq!(files[0].entries.len(), 1);
        assert_eq!(files[0].entries[0].key, "my_key");
        assert!(files[0].entries[0].desc.contains("value"));
    }

    #[test]
    fn from_files_merges_keys_from_multiple_files_same_language() {
        let svc = LocService::from_files(vec![
            (
                "a_l_english.yml".to_string(),
                r#"l_english:
 first:0 "A"
"#
                .to_string(),
            ),
            (
                "b_l_english.yml".to_string(),
                r#"l_english:
 second:0 "B"
 first:0 "A2"
"#
                .to_string(),
            ),
        ]);
        assert!(svc.errors().is_empty(), "{:?}", svc.errors());
        let english: Vec<&LocFile> = svc
            .files()
            .iter()
            .filter(|f| f.lang == Some(Lang::English))
            .collect();
        assert_eq!(english.len(), 2);
        let all_keys: Vec<&str> = english
            .iter()
            .flat_map(|f| f.entries.iter().map(|e| e.key.as_str()))
            .collect();
        assert!(all_keys.contains(&"first"));
        assert!(all_keys.contains(&"second"));
        assert_eq!(english[0].entries[0].desc, "\"A\"");
    }

    #[test]
    fn from_files_preserves_file_order() {
        let svc = LocService::from_files(vec![
            (
                "z_l_english.yml".to_string(),
                r#"l_english:
 z:0 "Z"
"#
                .to_string(),
            ),
            (
                "a_l_english.yml".to_string(),
                r#"l_english:
 a:0 "A"
"#
                .to_string(),
            ),
        ]);
        assert_eq!(svc.files()[0].path, "z_l_english.yml");
        assert_eq!(svc.files()[1].path, "a_l_english.yml");
    }

    #[test]
    fn from_files_reports_parse_errors_without_panicking() {
        let svc = LocService::from_files(vec![(
            "broken_l_english.yml".to_string(),
            "this is not a valid loc file\n".to_string(),
        )]);
        assert!(
            !svc.errors().is_empty(),
            "parse errors should be collected, not panic"
        );
        assert_eq!(svc.files().len(), 0);
    }

    #[test]
    fn from_files_with_encoding_records_bom_status() {
        let svc = LocService::from_files_with_encoding(vec![(
            "bom_l_english.yml".to_string(),
            r#"l_english:
 key:0 "v"
"#
            .to_string(),
            Some(cwtools_file_manager::FileEncoding::Utf8Bom),
        )]);
        assert_eq!(svc.files().len(), 1);
        assert_eq!(
            svc.files()[0].encoding,
            Some(cwtools_file_manager::FileEncoding::Utf8Bom)
        );
    }

    #[test]
    fn from_folder_skips_non_localisation_directories() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("not_loc")).unwrap();
        std::fs::create_dir_all(tmp.path().join("localisation")).unwrap();
        std::fs::write(
            tmp.path().join("not_loc").join("bad_l_english.yml"),
            r#"l_english:
 key:0 "v"
"#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("localisation").join("good_l_english.yml"),
            r#"l_english:
 key:0 "v"
"#,
        )
        .unwrap();

        let svc = LocService::from_folder(tmp.path(), ScanBudget::default());
        assert_eq!(svc.files().len(), 1);
        // Windows walks yield `\`-separated paths; compare normalised.
        assert!(
            svc.files()[0]
                .path
                .replace('\\', "/")
                .ends_with("localisation/good_l_english.yml")
        );
    }

    // Skipping this directory left every key defined there out of the index, so
    // script referencing them read as missing.
    #[test]
    fn from_folder_loads_the_synced_localisation_directory() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("localisation_synced")).unwrap();
        std::fs::write(
            tmp.path()
                .join("localisation_synced")
                .join("good_l_english.yml"),
            r#"l_english:
 key:0 "v"
"#,
        )
        .unwrap();

        let svc = LocService::from_folder(tmp.path(), ScanBudget::default());
        assert_eq!(svc.files().len(), 1);
        assert!(
            svc.files()[0]
                .path
                .replace('\\', "/")
                .ends_with("localisation_synced/good_l_english.yml")
        );
    }

    #[test]
    fn from_folder_skips_excluded_dot_directories() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude").join("localisation")).unwrap();
        std::fs::write(
            tmp.path()
                .join(".claude")
                .join("localisation")
                .join("dup_l_english.yml"),
            r#"l_english:
 key:0 "v"
"#,
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("localisation")).unwrap();
        std::fs::write(
            tmp.path().join("localisation").join("good_l_english.yml"),
            r#"l_english:
 key:0 "v"
"#,
        )
        .unwrap();

        let svc = LocService::from_folder(tmp.path(), ScanBudget::default());
        assert_eq!(svc.files().len(), 1);
        assert!(
            svc.files()[0]
                .path
                .replace('\\', "/")
                .ends_with("localisation/good_l_english.yml")
        );
    }

    #[test]
    fn from_files_routes_csv_to_csv_parser() {
        let csv = "#CODE;English;French;German;;Spanish\nKEY_A;Hello;Bonjour;Hallo;;Hola\n";
        let svc = LocService::from_files(vec![(
            "mod/localisation/localisation.csv".to_string(),
            csv.to_string(),
        )]);
        assert!(svc.errors().is_empty(), "{:?}", svc.errors());
        let langs = svc.languages();
        assert!(langs.contains(&Lang::English), "got: {:?}", langs);
        assert!(langs.contains(&Lang::French), "got: {:?}", langs);
        assert!(
            svc.files().iter().any(|f| {
                f.path.ends_with("localisation.csv")
                    && f.lang == Some(Lang::English)
                    && f.entries
                        .iter()
                        .any(|e| e.key == "KEY_A" && e.desc == "Hello")
            }),
            "CSV should produce English LocFile with KEY_A: {:?}",
            svc.files()
        );
    }

    #[test]
    fn from_folders_merges_multiple_roots() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(a.path().join("localisation")).unwrap();
        std::fs::create_dir_all(b.path().join("localisation")).unwrap();
        std::fs::write(
            a.path().join("localisation").join("a_l_english.yml"),
            r#"l_english:
 a:0 "A"
"#,
        )
        .unwrap();
        std::fs::write(
            b.path().join("localisation").join("b_l_english.yml"),
            r#"l_english:
 b:0 "B"
"#,
        )
        .unwrap();

        let svc = LocService::from_folders(&[a.path(), b.path()], ScanBudget::default());
        let paths: Vec<&str> = svc.files().iter().map(|f| f.path.as_str()).collect();
        assert!(
            paths.iter().any(|p| p.contains("a_l_english.yml")),
            "folder a missing: {:?}",
            paths
        );
        assert!(
            paths.iter().any(|p| p.contains("b_l_english.yml")),
            "folder b missing: {:?}",
            paths
        );
    }

    #[test]
    fn languages_returns_unique_langs() {
        let svc = LocService::from_files(vec![
            (
                "a_l_english.yml".to_string(),
                r#"l_english:
 a:0 "A"
"#
                .to_string(),
            ),
            (
                "b_l_english.yml".to_string(),
                r#"l_english:
 b:0 "B"
"#
                .to_string(),
            ),
            (
                "c_l_french.yml".to_string(),
                r#"l_french:
 c:0 "C"
"#
                .to_string(),
            ),
        ]);
        let mut langs = svc.languages();
        langs.sort_by_key(|l| format!("{l}"));
        assert_eq!(langs, vec![Lang::English, Lang::French]);
    }

    #[test]
    fn filtered_discovery_prunes_before_the_file_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let ignored = tmp.path().join("ignored/localisation");
        let kept = tmp.path().join("kept/localisation");
        std::fs::create_dir_all(&ignored).unwrap();
        for name in ["a.yml", "b.yml", "c.yml"] {
            std::fs::write(ignored.join(name), "l_english:\n key:0 \"ignored\"\n").unwrap();
        }

        let mut remaining = 1;
        let ignore_dirs = ["ignored".to_string()];
        let ignored_files = walk_folder_inner(
            tmp.path(),
            tmp.path(),
            false,
            true,
            &mut remaining,
            &[],
            &ignore_dirs,
        );
        assert!(ignored_files.is_empty());
        assert_eq!(remaining, 1);

        std::fs::create_dir_all(&kept).unwrap();
        std::fs::write(kept.join("keep.yml"), "l_english:\n key:0 \"kept\"\n").unwrap();
        let kept_files = walk_folder_inner(
            tmp.path(),
            &tmp.path().join("kept"),
            false,
            false,
            &mut remaining,
            &[],
            &ignore_dirs,
        );
        assert_eq!(kept_files, vec![kept.join("keep.yml")]);
        assert_eq!(remaining, 0);
    }

    #[test]
    fn filtered_discovery_matches_root_relative_file_and_directory_globs() {
        let tmp = tempfile::tempdir().unwrap();
        for relative in [
            "localisation/keep.yml",
            "localisation/skip.yml",
            "wip/localisation/staged.yml",
        ] {
            let path = tmp.path().join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "l_english:\n key:0 \"v\"\n").unwrap();
        }

        let files = LocService::discover_files_filtered(
            &[tmp.path()],
            ScanBudget::default(),
            &["localisation/skip.yml".to_string()],
            &["wip/localisation".to_string()],
        );
        assert_eq!(files, vec![tmp.path().join("localisation/keep.yml")]);
    }

    #[test]
    fn filtered_load_skips_unselected_yaml_languages_before_parsing() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = tmp.path().join("localisation");
        std::fs::create_dir_all(&loc).unwrap();
        std::fs::write(loc.join("en.yml"), "l_english:\n en:0 \"English\"\n").unwrap();
        std::fs::write(loc.join("fr.yml"), "l_french:\n fr:0 \"French\"\n").unwrap();
        std::fs::write(loc.join("names.csv"), "key;english;french\nname;Name;Nom\n").unwrap();

        let service = LocService::from_folders_filtered(
            &[tmp.path()],
            ScanBudget::default(),
            Some(&[Lang::English]),
            &[],
            &[],
        );
        assert!(
            service
                .files()
                .iter()
                .any(|file| file.path.ends_with("en.yml"))
        );
        assert!(
            !service
                .files()
                .iter()
                .any(|file| file.path.ends_with("fr.yml"))
        );
        assert!(service.files().iter().any(|file| file.is_csv));
    }

    #[test]
    fn filtered_load_preserves_capped_read_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = tmp.path().join("localisation");
        std::fs::create_dir_all(&loc).unwrap();
        std::fs::write(
            loc.join("large_french.yml"),
            "l_french:\n".to_string() + &"x".repeat(200),
        )
        .unwrap();
        let budget = ScanBudget {
            max_file_size: 50,
            ..ScanBudget::default()
        };

        let service = LocService::from_folders_filtered(
            &[tmp.path()],
            budget,
            Some(&[Lang::English]),
            &[],
            &[],
        );
        assert_eq!(service.errors().len(), 1);
        assert!(
            service.errors()[0]
                .1
                .contains("file exceeds the 50 byte read cap")
        );
    }

    #[test]
    fn header_language_filter_matches_the_full_parser() {
        let cases = [
            "l_english:\n key:0 \"v\"\n",
            "\u{feff}l_french:\n key:0 \"v\"\n",
            " # comment\n\nl_russian:\n key:0 \"v\"\n",
            "\n\u{feff}l_french:\n key:0 \"v\"\n",
            "l_klingon:\n key:0 \"v\"\n",
            "no header\n key:0 \"v\"\n",
            "",
        ];
        for text in cases {
            let full = parse_loc_text(text, "f.yml")
                .ok()
                .and_then(|file| file.lang);
            assert_eq!(loc_header_language(text, "f.yml"), full);
        }
    }

    // ── symlink / over-limit hardening (#161) ──────────────────────────────────

    /// The loc walker must reject symlinks: a dir symlink can point outside the
    /// root or into a cycle, and a file symlink can point at a special file.
    #[cfg(unix)]
    #[test]
    fn from_folder_rejects_symlinks() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("localisation")).unwrap();
        std::fs::write(
            tmp.path().join("localisation").join("good_l_english.yml"),
            r#"l_english:
 key:0 "v"
"#,
        )
        .unwrap();
        symlink(
            tmp.path().join("localisation").join("good_l_english.yml"),
            tmp.path().join("localisation").join("link_l_english.yml"),
        )
        .unwrap();

        let svc = LocService::from_folder(tmp.path(), ScanBudget::default());
        assert_eq!(svc.files().len(), 1, "symlinked loc file must be rejected");
        assert!(
            svc.files()[0]
                .path
                .replace('\\', "/")
                .ends_with("good_l_english.yml")
        );
    }

    /// A loc file over the per-file read cap must be skipped, not read to EOF.
    #[test]
    fn from_folder_skips_over_limit_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("localisation")).unwrap();
        std::fs::write(
            tmp.path().join("localisation").join("good_l_english.yml"),
            r#"l_english:
 key:0 "v"
"#,
        )
        .unwrap();
        // A file over the per-file cap must be skipped, not read to EOF.
        std::fs::write(
            tmp.path().join("localisation").join("huge_l_english.yml"),
            "l_english:\n".to_string() + &"x".repeat(200),
        )
        .unwrap();

        let budget = ScanBudget {
            max_file_size: 50,
            ..ScanBudget::default()
        };
        let svc = LocService::from_folder(tmp.path(), budget);
        assert!(
            svc.files()
                .iter()
                .any(|f| f.path.ends_with("good_l_english.yml")),
            "good file must load"
        );
        assert!(
            !svc.files()
                .iter()
                .any(|f| f.path.ends_with("huge_l_english.yml")),
            "over-cap file must not load"
        );
    }

    #[test]
    fn discover_files_returns_sorted_order() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = tmp.path().join("localisation");
        std::fs::create_dir_all(loc.join("zebra")).unwrap();
        std::fs::create_dir_all(loc.join("alpha")).unwrap();
        std::fs::write(loc.join("middle.yml"), "l_english:\n k:0 \"v\"\n").unwrap();
        std::fs::write(loc.join("zebra").join("z.yml"), "l_english:\n k:0 \"v\"\n").unwrap();
        std::fs::write(loc.join("alpha").join("a.yml"), "l_english:\n k:0 \"v\"\n").unwrap();

        let files = LocService::discover_files(&[tmp.path()], ScanBudget::default());
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.yml", "middle.yml", "z.yml"]);
    }

    #[test]
    fn discover_files_budget_follows_sorted_order() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = tmp.path().join("localisation");
        std::fs::create_dir_all(&loc).unwrap();
        for name in ["zebra.yml", "alpha.yml", "middle.yml"] {
            std::fs::write(loc.join(name), "l_english:\n k:0 \"v\"\n").unwrap();
        }
        let files = LocService::discover_files(
            &[tmp.path()],
            ScanBudget {
                max_files: 1,
                ..ScanBudget::default()
            },
        );
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_name().unwrap().to_string_lossy(), "alpha.yml");
    }
}
