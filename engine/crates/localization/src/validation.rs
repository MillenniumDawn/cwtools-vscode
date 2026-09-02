use crate::commands::{LocEntry, LocFile};
use crate::loc_index::LocKeySet;
use cwtools_parser::ast::{SourcePos, SourceRange};
use cwtools_parser::fix::SuggestedFix;
use std::collections::HashSet;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq)]
pub enum LocErrorKind {
    UndefinedLocReference {
        other_key: String,
    },
    RecursiveLocRef,
    /// A `REPLACE_ME` / `TODO_CD` placeholder value (F# CW234).
    ReplaceMe,
    LocMissingQuote,
    LocInvalidChars,
    LocKeyInvalidChars,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocValidationError {
    pub line: usize,
    pub col: usize,
    pub key: String,
    pub kind: LocErrorKind,
    pub fix: Option<SuggestedFix>,
}

/// unbalanced quote. The span covers the trimmed desc; the replacement strips any
fn cw268_quote_fix(entry: &LocEntry) -> Option<SuggestedFix> {
    let desc = &entry.desc;
    let lead = desc.chars().count() - desc.trim_start().chars().count();
    let trimmed = desc.trim();
    if trimmed.is_empty() {
        return None;
    }
    let inner = trimmed.trim_matches('"');
    if inner.contains('"') || trimmed.contains('#') {
        return None;
    }
    let start_col = entry.desc_column + lead;
    let end_col = start_col + trimmed.chars().count();
    if start_col > u16::MAX as usize || end_col > u16::MAX as usize {
        return None;
    }
    let line = entry.position.line as u32;
    let range = SourceRange {
        start: SourcePos {
            line,
            col: start_col.min(u16::MAX as usize) as u16,
        },
        end: SourcePos {
            line,
            col: end_col.min(u16::MAX as usize) as u16,
        },
    };
    Some(SuggestedFix::replace(
        "Wrap the value in quotes",
        range,
        format!("\"{inner}\""),
    ))
}

pub fn validate_loc_file(
    file: &LocFile,
    all_keys: &LocKeySet,
    extra_valid_refs: &HashSet<String>,
    hardcoded_localisation: &[impl AsRef<str>],
) -> Vec<LocValidationError> {
    let hardcoded: HashSet<String> = hardcoded_localisation
        .iter()
        .map(|s| s.as_ref().to_lowercase())
        .collect();
    validate_loc_file_with_hardcoded(
        file,
        all_keys,
        &HashSet::new(),
        extra_valid_refs,
        &hardcoded,
    )
}

pub fn hardcoded_loc_set() -> &'static HashSet<String> {
    static SET: OnceLock<HashSet<String>> = OnceLock::new();
    SET.get_or_init(|| HARDCODED_LOC.iter().map(|s| s.to_lowercase()).collect())
}

pub(crate) fn validate_loc_file_with_hardcoded(
    file: &LocFile,
    all_keys: &LocKeySet,
    additional_loc_keys: &HashSet<String>,
    extra_valid_refs: &HashSet<String>,
    hardcoded: &HashSet<String>,
) -> Vec<LocValidationError> {
    let mut errors = Vec::new();

    for entry in &file.entries {
        validate_key_chars(entry, &mut errors);

        validate_invalid_chars(entry, &mut errors);

        if !validate_quotes(entry) {
            errors.push(LocValidationError {
                line: entry.position.line,
                col: entry.position.column,
                key: entry.key.clone(),
                kind: LocErrorKind::LocMissingQuote,
                fix: cw268_quote_fix(entry),
            });
        }

        for r in &entry.refs {
            let lowercase = r.to_lowercase();
            if all_keys.contains(lowercase.as_str()) || additional_loc_keys.contains(&lowercase) {
                if lowercase == entry.key.to_lowercase() && !hardcoded.contains(&lowercase) {
                    errors.push(LocValidationError {
                        line: entry.position.line,
                        col: entry.position.column,
                        key: entry.key.clone(),
                        kind: LocErrorKind::RecursiveLocRef,
                        fix: None,
                    });
                }
            } else if extra_valid_refs.contains(&lowercase) {
            } else {
                let has_lower = r.chars().any(|c| c.is_lowercase());
                let first_space = r.find(' ');
                let last_space = r.rfind(' ');

                if has_lower
                    && !hardcoded.contains(&lowercase)
                    && !(first_space.is_some() && last_space.is_some() && first_space != last_space)
                {
                    errors.push(LocValidationError {
                        line: entry.position.line,
                        col: entry.position.column,
                        key: entry.key.clone(),
                        kind: LocErrorKind::UndefinedLocReference {
                            other_key: r.clone(),
                        },
                        fix: None,
                    });
                }
            }
        }

        if is_replace_me(entry) {
            errors.push(LocValidationError {
                line: entry.position.line,
                col: entry.position.column,
                key: entry.key.clone(),
                kind: LocErrorKind::ReplaceMe,
                fix: None,
            });
        }
    }

    errors
}

pub fn validate_invalid_chars(entry: &LocEntry, errors: &mut Vec<LocValidationError>) {
    if let Some(range) = &entry.error_range {
        errors.push(LocValidationError {
            line: range.line,
            col: range.column,
            key: entry.key.clone(),
            kind: LocErrorKind::LocInvalidChars,
            fix: None,
        });
    }
}

pub fn validate_key_chars(entry: &LocEntry, errors: &mut Vec<LocValidationError>) {
    if entry.key.chars().any(|c| !is_valid_loc_key_char(c)) {
        errors.push(LocValidationError {
            line: entry.position.line,
            col: entry.position.column,
            key: entry.key.clone(),
            kind: LocErrorKind::LocKeyInvalidChars,
            fix: None,
        });
    }
}

fn is_valid_loc_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')
}

/// Quote validation (mirrors F# `validateQuotes`).
pub(crate) fn validate_quotes(entry: &LocEntry) -> bool {
    let trimmed = entry.desc.trim();

    let last_quote = trimmed.rfind('"');

    let first_hash_after_quote = last_quote
        .and_then(|q| trimmed[q..].find('#').map(|h| q + h))
        .or_else(|| trimmed.find('#'));

    let mut effective = match (first_hash_after_quote, last_quote) {
        (Some(h), Some(q)) if h > q => &trimmed[..h],
        _ => trimmed,
    };

    let ends_quote = effective.rfind('"');
    if let Some(q) = ends_quote {
        effective = effective[..=q].trim_end();
    }

    let starts = effective.starts_with('"');
    let ends = effective.ends_with('"');

    if starts && ends && effective.len() == 1 {
        return false;
    }

    starts == ends
}

/// Check for `REPLACE_ME` / `TODO_CD` placeholder values, quoted or not.
pub fn is_replace_me(entry: &LocEntry) -> bool {
    let inner = entry.desc.trim().trim_matches('"');
    inner == "REPLACE_ME" || inner == "TODO_CD"
}

pub const HARDCODED_LOC: &[&str] = &[
    "Player",
    "Root",
    "From",
    "Prev",
    "Capital",
    "Random",
    "This",
    "Country",
    "Ruler",
    "GetName",
    "GetName2",
    "GetSpeciesName",
    "GetSpeciesNamePlural",
    "GetSpeciesAdj",
    "GetTitle",
    "Owner",
    "Controller",
    "GetGovernmentName",
    "GetClassName",
    "GetAdj",
    "GetIcon",
    "GetRegnalName",
    "Date",
    "GetDate",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yaml_parser::parse_loc_text;

    fn key_set(keys: impl IntoIterator<Item = &'static str>) -> LocKeySet {
        keys.into_iter().map(Into::into).collect()
    }

    #[test]
    fn test_validate_undefined_ref() {
        let text = "l_english:\n key1: \"Hello $undefined_key$\"\n";
        let file = parse_loc_text(text, "test.yml").unwrap();
        let keys = LocKeySet::default();
        let errors = validate_loc_file(&file, &keys, &HashSet::new(), &Vec::<String>::new());

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].key, "key1");
        assert_eq!(
            errors[0].kind,
            LocErrorKind::UndefinedLocReference {
                other_key: "undefined_key".to_string()
            }
        );
    }

    #[test]
    fn test_validate_recursive_ref() {
        let text = "l_english:\n key1: \"Hello $key1$\"\n";
        let file = parse_loc_text(text, "test.yml").unwrap();
        let keys = key_set(["key1"]);
        let errors = validate_loc_file(&file, &keys, &HashSet::new(), &Vec::<String>::new());

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, LocErrorKind::RecursiveLocRef);
    }

    #[test]
    fn additional_loc_key_keeps_recursive_ref_semantics() {
        let text = "l_english:\n key1: \"Hello $key1$\"\n";
        let file = parse_loc_text(text, "test.yml").unwrap();
        let errors = validate_loc_file_with_hardcoded(
            &file,
            &LocKeySet::default(),
            &HashSet::from(["key1".to_string()]),
            &HashSet::new(),
            &HashSet::new(),
        );

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, LocErrorKind::RecursiveLocRef);
    }

    #[test]
    fn test_validate_valid_ref() {
        let text = "l_english:\n key1: \"Hello $key2$\"\n";
        let file = parse_loc_text(text, "test.yml").unwrap();
        let keys = key_set(["key2"]);
        let errors = validate_loc_file(&file, &keys, &HashSet::new(), &Vec::<String>::new());

        assert!(errors.is_empty(), "valid ref should not error");
    }

    #[test]
    fn test_validate_replace_me() {
        let text = "l_english:\n key1: \"REPLACE_ME\"\n";
        let file = parse_loc_text(text, "test.yml").unwrap();
        let keys = LocKeySet::default();

        let errors = validate_loc_file(&file, &keys, &HashSet::new(), &Vec::<String>::new());

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, LocErrorKind::ReplaceMe);
    }

    #[test]
    fn test_hardcoded_refs_ignored() {
        let text = "l_english:\n key1: \"Hello $Player$\"\n";
        let file = parse_loc_text(text, "test.yml").unwrap();
        let keys = LocKeySet::default();
        let errors = validate_loc_file(&file, &keys, &HashSet::new(), &["Player"]);

        assert!(errors.is_empty(), "hardcoded ref should not error");
    }

    #[test]
    fn test_invalid_char_detected() {
        let bad_char = '\u{FFFE}';
        let text = format!("l_english:\n key1: \"Hello {}world\"\n", bad_char);
        let file = parse_loc_text(&text, "test.yml").unwrap();

        assert!(
            file.entries[0].error_range.is_some(),
            "parser should have set error_range for out-of-range char"
        );

        let keys = LocKeySet::default();
        let errors = validate_loc_file(&file, &keys, &HashSet::new(), &Vec::<String>::new());

        let inv_char_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.kind == LocErrorKind::LocInvalidChars)
            .collect();
        assert!(
            !inv_char_errors.is_empty(),
            "expected LocInvalidChars error, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_valid_chars_no_error() {
        let text = "l_english:\n key1: \"Hello world — café\"\n";
        let file = parse_loc_text(text, "test.yml").unwrap();

        assert!(
            file.entries[0].error_range.is_none(),
            "valid chars should not set error_range"
        );

        let keys = LocKeySet::default();
        let errors = validate_loc_file(&file, &keys, &HashSet::new(), &Vec::<String>::new());
        let inv_char_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.kind == LocErrorKind::LocInvalidChars)
            .collect();
        assert!(
            inv_char_errors.is_empty(),
            "valid chars should not produce LocInvalidChars"
        );
    }

    #[test]
    fn cw268_fix_wraps_value_in_quotes() {
        use cwtools_parser::fix::apply_edits;
        let text = "l_english:\n key: \"unclosed\n";
        let file = parse_loc_text(text, "test.yml").unwrap();
        let keys = LocKeySet::default();
        let errors = validate_loc_file(&file, &keys, &HashSet::new(), &Vec::<String>::new());

        let err = errors
            .iter()
            .find(|e| e.kind == LocErrorKind::LocMissingQuote)
            .expect("CW268 emitted");
        let fix = err.fix.as_ref().expect("CW268 carries a fix");
        let fixed = apply_edits(text, &fix.edits);
        assert_eq!(fixed, "l_english:\n key: \"unclosed\"\n");

        let file2 = parse_loc_text(&fixed, "test.yml").unwrap();
        let errors2 = validate_loc_file(&file2, &keys, &HashSet::new(), &Vec::<String>::new());
        assert!(
            !errors2
                .iter()
                .any(|e| e.kind == LocErrorKind::LocMissingQuote),
            "CW268 must be gone after applying the fix"
        );
    }

    #[test]
    fn cw268_overlong_value_has_no_unsafe_fix() {
        let value = "x".repeat(u16::MAX as usize + 1);
        let text = format!("l_english:\n key: \"{value}\n");
        let file = parse_loc_text(&text, "test.yml").unwrap();
        let errors = validate_loc_file(
            &file,
            &LocKeySet::default(),
            &HashSet::new(),
            &Vec::<String>::new(),
        );

        let error = errors
            .iter()
            .find(|e| e.kind == LocErrorKind::LocMissingQuote)
            .expect("CW268 emitted");
        assert!(
            error.fix.is_none(),
            "CW268 must not carry a fix for an unrepresentable range"
        );
    }

    #[test]
    fn test_unterminated_string_emits_cw268() {
        let text = "l_english:\n missing_quote:0 \"unclosed\n";
        let file = parse_loc_text(text, "test.yml").unwrap();
        let keys = LocKeySet::default();
        let errors = validate_loc_file(&file, &keys, &HashSet::new(), &Vec::<String>::new());

        let cw268: Vec<_> = errors
            .iter()
            .filter(|e| e.kind == LocErrorKind::LocMissingQuote)
            .collect();
        assert!(
            !cw268.is_empty(),
            "opening quote with no closing quote should emit LocMissingQuote: {:?}",
            errors
        );
    }

    #[test]
    fn test_key_with_space_emits_cw276() {
        let text = "l_english:\n \"bad key\": \"value\"\n";
        let file = parse_loc_text(text, "test.yml").unwrap();
        let keys = LocKeySet::default();
        let errors = validate_loc_file(&file, &keys, &HashSet::new(), &Vec::<String>::new());

        let cw276: Vec<_> = errors
            .iter()
            .filter(|e| e.kind == LocErrorKind::LocKeyInvalidChars)
            .collect();
        assert!(
            !cw276.is_empty(),
            "key with space should emit LocKeyInvalidChars: {:?}",
            errors
        );
    }

    #[test]
    fn test_valid_key_no_cw276() {
        let text = "l_english:\n valid_key.sub-key: \"value\"\n";
        let file = parse_loc_text(text, "test.yml").unwrap();
        let keys = LocKeySet::default();
        let errors = validate_loc_file(&file, &keys, &HashSet::new(), &Vec::<String>::new());

        assert!(
            errors
                .iter()
                .all(|e| e.kind != LocErrorKind::LocKeyInvalidChars),
            "valid key chars should not emit LocKeyInvalidChars: {:?}",
            errors
        );
    }

    #[test]
    fn test_recursive_ref_case_insensitive() {
        let text = "l_english:\n KEY1: \"Hello $key1$\"\n";
        let file = parse_loc_text(text, "test.yml").unwrap();
        let keys = key_set(["key1"]); // stored lowercased in union
        let errors = validate_loc_file(&file, &keys, &HashSet::new(), &Vec::<String>::new());

        let recursive: Vec<_> = errors
            .iter()
            .filter(|e| e.kind == LocErrorKind::RecursiveLocRef)
            .collect();
        assert!(
            !recursive.is_empty(),
            "case-insensitive self-ref should trigger RecursiveLocRef: {:?}",
            errors
        );
    }
}
