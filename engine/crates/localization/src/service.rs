use crate::commands::{Lang, LocFile};
use crate::csv_parser::parse_csv_loc_per_lang;
use crate::yaml_parser::{LocFileParseError, parse_loc_text};
use cwtools_file_manager::{FileEncoding, ScanBudget, ScanBytes, read_text_capped_with_encoding};
use std::path::{Path, PathBuf};

pub struct LocService {
    files: Vec<LocFile>,
    errors: Vec<(String, String)>,
}

impl LocService {
    pub fn from_files(files: Vec<(String, String)>) -> Self {
        Self::from_files_with_encoding(files.into_iter().map(|(p, t)| (p, t, None)).collect())
    }

    /// As [`from_files`], but each file carries its detected on-disk encoding so
    /// the UTF-8-BOM rule (CW254) can be enforced.
    pub fn from_files_with_encoding(files: Vec<(String, String, Option<FileEncoding>)>) -> Self {
        use rayon::prelude::*;

        let results: Vec<Result<Vec<LocFile>, (String, String)>> = files
            .into_par_iter()
            .map(|(path, text, encoding)| parse_loc_file_entry(path, text, encoding))
            .collect();
        Self::from_results(results)
    }

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

    pub fn from_paths(paths: Vec<PathBuf>, budget: ScanBudget, langs: Option<&[Lang]>) -> Self {
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

    pub fn merge_from(&mut self, mut other: Self) {
        self.files.append(&mut other.files);
        self.errors.append(&mut other.errors);
    }

    pub fn files(&self) -> &[LocFile] {
        &self.files
    }

    pub fn errors(&self) -> &[(String, String)] {
        &self.errors
    }

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

fn parse_loc_file_entry(
    path: String,
    text: String,
    encoding: Option<FileEncoding>,
) -> Result<Vec<LocFile>, (String, String)> {
    parse_loc_files(&path, &text, encoding).map_err(|e| (path, e.to_string()))
}

pub fn parse_loc_files(
    path: &str,
    text: &str,
    encoding: Option<FileEncoding>,
) -> Result<Vec<LocFile>, LocFileParseError> {
    let is_csv = Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("csv"));
    if is_csv {
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
    fn from_paths_merges_multiple_roots() {
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

        let svc = LocService::from_paths(
            vec![
                a.path().join("localisation/a_l_english.yml"),
                b.path().join("localisation/b_l_english.yml"),
            ],
            ScanBudget::default(),
            None,
        );
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
    fn filtered_load_skips_unselected_yaml_languages_before_parsing() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = tmp.path().join("localisation");
        std::fs::create_dir_all(&loc).unwrap();
        std::fs::write(loc.join("en.yml"), "l_english:\n en:0 \"English\"\n").unwrap();
        std::fs::write(loc.join("fr.yml"), "l_french:\n fr:0 \"French\"\n").unwrap();
        std::fs::write(loc.join("names.csv"), "key;english;french\nname;Name;Nom\n").unwrap();

        let service = LocService::from_paths(
            vec![
                loc.join("en.yml"),
                loc.join("fr.yml"),
                loc.join("names.csv"),
            ],
            ScanBudget::default(),
            Some(&[Lang::English]),
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

        let service = LocService::from_paths(
            vec![loc.join("large_french.yml")],
            budget,
            Some(&[Lang::English]),
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

    #[test]
    fn from_paths_skips_over_limit_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("localisation")).unwrap();
        std::fs::write(
            tmp.path().join("localisation").join("good_l_english.yml"),
            r#"l_english:
 key:0 "v"
"#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("localisation").join("huge_l_english.yml"),
            "l_english:\n".to_string() + &"x".repeat(200),
        )
        .unwrap();

        let budget = ScanBudget {
            max_file_size: 50,
            ..ScanBudget::default()
        };
        let svc = LocService::from_paths(
            vec![
                tmp.path().join("localisation/good_l_english.yml"),
                tmp.path().join("localisation/huge_l_english.yml"),
            ],
            budget,
            None,
        );
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
}
