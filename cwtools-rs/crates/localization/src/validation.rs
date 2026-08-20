//! Localisation validation.
//!
//! Validates parsed loc entries:
//! * Undefined loc references
//! * Recursive references
//! * Invalid loc characters
//! * Missing/computed loc commands
//!
//! Mirrors F# `LocalisationString.fs`.

use crate::commands::{LocEntry, LocFile};
use crate::loc_index::LocKeySet;
use cwtools_parser::ast::{SourcePos, SourceRange};
use cwtools_parser::fix::SuggestedFix;
use std::collections::HashSet;
use std::sync::OnceLock;

/// The kind of a loc-entry validation error.
///
/// Carries the structured data needed to build a diagnostic with the correct
/// F# numeric code (see `pipeline::loc_error_code_severity`). The language is supplied by
/// the caller (it comes from the file being validated), not stored here.
#[derive(Debug, Clone, PartialEq)]
pub enum LocErrorKind {
    /// A `$ref$` to a key that doesn't exist anywhere (F# CW225).
    UndefinedLocReference { other_key: String },
    /// A loc string that references itself (F# CW259).
    RecursiveLocRef,
    /// A `REPLACE_ME` / `TODO_CD` placeholder value (F# CW234).
    ReplaceMe,
    /// Value doesn't start and end with double quotes (F# CW268).
    LocMissingQuote,
    /// Value contains characters outside the allowed Unicode ranges (F# CW275).
    LocInvalidChars,
    /// Key contains spaces or characters not valid in loc keys (CW276).
    LocKeyInvalidChars,
}

/// Validation error for a loc entry. `line`/`col` are 1-based source positions.
#[derive(Debug, Clone, PartialEq)]
pub struct LocValidationError {
    pub line: usize,
    pub col: usize,
    pub key: String,
    pub kind: LocErrorKind,
    /// Optional machine-applicable fix, carried through to `LocDiagnostic`. Pure
    /// metadata; the report/hash path never reads it.
    pub fix: Option<SuggestedFix>,
}

/// Build the CW268 "wrap the value in quotes" fix for an entry whose desc has an
/// unbalanced quote. The span covers the trimmed desc; the replacement strips any
/// stray surrounding quotes and re-wraps. Returns `None` (no fix, don't
/// approximate) when the value carries an embedded quote or a trailing comment,
/// where a single wrap would be wrong. Ranges use the parser convention: 1-based
/// line, 0-based char column.
fn cw268_quote_fix(entry: &LocEntry) -> Option<SuggestedFix> {
    let desc = &entry.desc;
    let lead = desc.chars().count() - desc.trim_start().chars().count();
    let trimmed = desc.trim();
    if trimmed.is_empty() {
        return None;
    }
    let inner = trimmed.trim_matches('"');
    // A comment or an interior quote makes the single-wrap ambiguous — skip.
    if inner.contains('"') || trimmed.contains('#') {
        return None;
    }
    let start_col = entry.desc_column + lead;
    let end_col = start_col + trimmed.chars().count();
    let line = entry.position.line as u32;
    let range = SourceRange {
        start: SourcePos {
            line,
            col: start_col as u16,
        },
        end: SourcePos {
            line,
            col: end_col as u16,
        },
    };
    Some(SuggestedFix::replace(
        "Wrap the value in quotes",
        range,
        format!("\"{inner}\""),
    ))
}

/// Validate a loaded loc file against a set of known keys.
///
/// * `file` – the parsed loc file (`yaml_parser::parse_loc_text` result)
/// * `all_keys` – union of keys across ALL languages (to validate `$ref$`)
/// * `extra_valid_refs` – lowercased names a `$ref$` may resolve to besides loc
///   keys: game-definition registries the engine resolves in loc context
///   (modifiers, ideas). A ref matching one is treated as defined.
///
/// Returns list of validation errors.
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
    validate_loc_file_with_hardcoded(file, all_keys, extra_valid_refs, &hardcoded)
}

/// Lowercased [`HARDCODED_LOC`], built once. The project-validation hot path
/// reuses this instead of re-lowercasing + re-collecting the list per file.
pub fn hardcoded_loc_set() -> &'static HashSet<String> {
    static SET: OnceLock<HashSet<String>> = OnceLock::new();
    SET.get_or_init(|| HARDCODED_LOC.iter().map(|s| s.to_lowercase()).collect())
}

/// As [`validate_loc_file`], but takes the already-lowercased hardcoded set so
/// the caller can build it once outside a per-file loop.
pub(crate) fn validate_loc_file_with_hardcoded(
    file: &LocFile,
    all_keys: &LocKeySet,
    extra_valid_refs: &HashSet<String>,
    hardcoded: &HashSet<String>,
) -> Vec<LocValidationError> {
    let mut errors = Vec::new();

    for entry in &file.entries {
        // ---- Invalid key characters ----
        validate_key_chars(entry, &mut errors);

        // ---- Invalid characters ----
        validate_invalid_chars(entry, &mut errors);

        // ---- Quote balancing ----
        if !validate_quotes(entry) {
            errors.push(LocValidationError {
                line: entry.position.line,
                col: entry.position.column,
                key: entry.key.clone(),
                kind: LocErrorKind::LocMissingQuote,
                fix: cw268_quote_fix(entry),
            });
        }

        // ---- Undefined references ----
        for r in &entry.refs {
            let lowercase = r.to_lowercase();
            if all_keys.contains(lowercase.as_str()) {
                // Defined – check for recursion (case-insensitive, matching F# checkRef)
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
                // Resolves to a modifier/idea (HOI4 `$modifier$` / `$idea$` embed),
                // not a loc key. Defined for the engine — no CW225, and no
                // recursion check (it isn't a loc self-reference).
            } else {
                // Not defined – check F# rule: if the ref contains lowercase
                // letters but is not all-lowercase, it's "maybe a compound",
                // which F# accepts (e.g. "FROM.FROM")
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

        // ---- REPLACE_ME / TODO_CD check ----
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

/// Validate invalid characters.
///
/// If `entry.error_range` is set (populated by the parser when a char outside
/// the `isLocValueChar` ranges was found), push a `CW-LocInvalidChars` error.
/// Mirrors F# `validateInvalidChars` (LocalisationString.fs:124-127).
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

/// Check whether a loc key contains only characters valid in a loc key.
///
/// Valid: ASCII alphanumeric, `_`, `.`, `-`. A space or anything outside this
/// set triggers CW276.
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
///
/// Returns `true` if OK, `false` if unbalanced.
/// On failure, sets `entry.error_range`.
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

    // A single `"` satisfies both starts_with and ends_with — it's only an
    // opening quote with the truncation consuming the whole string. Still
    // unbalanced.
    if starts && ends && effective.len() == 1 {
        return false;
    }

    // Balanced when both ends quote or neither does; mismatch -> CW268. (No
    // mutation here: the caller already ran the invalid-char check that reads
    // `error_range`, so a write would be dead anyway.)
    starts == ends
}

/// Check for `REPLACE_ME` / `TODO_CD` placeholder values, quoted or not.
pub fn is_replace_me(entry: &LocEntry) -> bool {
    let inner = entry.desc.trim().trim_matches('"');
    inner == "REPLACE_ME" || inner == "TODO_CD"
}

/// Loc references that are hardcoded engine concepts (scopes, common getters)
/// and so are never "undefined" even when absent from the key set.
///
/// Mirrors the F# `hardcodedLocalisation` list.
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
        // U+FFFE is outside all allowed ranges and should trigger CW-LocInvalidChars
        let bad_char = '\u{FFFE}';
        let text = format!("l_english:\n key1: \"Hello {}world\"\n", bad_char);
        let file = parse_loc_text(&text, "test.yml").unwrap();

        // error_range should be set by the parser
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
        // Normal ASCII and Latin Extended chars should not trigger the check
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

    // ---- unterminated string (CW268) bug fix --------------------------------

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

        // Revalidation of the fixed text no longer emits CW268.
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
    fn test_unterminated_string_emits_cw268() {
        // A value with only an opening quote and no closing quote must flag CW268.
        // The previous implementation truncated effective to a single `"` which
        // appeared balanced on both ends, so no error was emitted.
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

    // ---- key character validation (CW276) -----------------------------------

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

    // ---- case-insensitive recursive ref check (fix 7) ---------------------

    #[test]
    fn test_recursive_ref_case_insensitive() {
        // key "KEY1" references "$key1$" — different case, should still be recursive
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
