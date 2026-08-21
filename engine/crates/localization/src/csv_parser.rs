//! CSV localisation parser (CK2/VIC2-style).
//!
//! CK2 and VIC2 use `;`-delimited rows with multiple languages per row:
//!   CODE;English;French;German;;Spanish;...
//!
//! `#` rows are comments except `#CODE` header lines (which are skipped).
//! Columns map to languages in the order defined by the game's schema.
//!
//! This module produces per-language `LocEntry` values instead of
//! concatenating all columns, matching F# `CK2Localisation.fs` and
//! `VIC2Localisation.fs`.

use crate::commands::{Lang, LocEntry, Position};
use std::sync::Arc;

/// Column order for CK2 / VIC2 CSV localisation files.
///
/// Index 0 is the key; indices 1–4 are languages in this order.
/// Matches F# `CSVLocRow`:  Code, English, French, German, Spanish.
pub const CK2_COLUMN_LANGS: &[Option<Lang>] = &[
    None,                // col 0: key
    Some(Lang::English), // col 1
    Some(Lang::French),  // col 2
    Some(Lang::German),  // col 3
    None,                // col 4: empty column (CK2/VIC2 schema gap)
    Some(Lang::Spanish), // col 5
];

/// Parse a CSV localisation file and return per-language entries.
///
/// * `text`         — raw file text
/// * `name`         — file name for `Position` records
/// * `column_langs` — slice mapping column index → `Option<Lang>`.
///   `None` entries are skip columns.  Defaults to `CK2_COLUMN_LANGS`
///   when `None` is passed.
///
/// `#` lines are treated as comments and skipped, **except** `#CODE`
/// header lines which are also skipped (not parsed as data).
///
/// Returns `(key, lang, LocEntry)` triples so the caller can bucket them.
pub(crate) fn parse_csv_loc_per_lang(
    text: &str,
    name: &str,
    column_langs: Option<&[Option<Lang>]>,
) -> Vec<(String, Lang, LocEntry)> {
    let column_langs = column_langs.unwrap_or(CK2_COLUMN_LANGS);
    // One Arc allocation shared by every Position in this file.
    let stream_name: Arc<str> = Arc::from(name);
    let mut out = Vec::new();

    for (line_num, line) in text.lines().enumerate() {
        let trimmed = line.trim();

        // Skip blank lines and comment lines (including #CODE header)
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = trimmed.split(';').collect();
        if parts.is_empty() {
            continue;
        }

        let key = parts[0].trim().to_string();
        if key.is_empty() {
            continue;
        }

        // 0-based char column of `trimmed` within the original line.
        let leading_ws = line.chars().count() - line.trim_start().chars().count();

        // Emit one LocEntry per language column that has a value
        for (col_idx, maybe_lang) in column_langs.iter().enumerate().skip(1) {
            let Some(lang) = *maybe_lang else { continue };

            let desc = parts.get(col_idx).copied().unwrap_or("").to_string();

            let position = Position::new(Arc::clone(&stream_name), line_num + 1, 1);
            // Char column where this cell begins: preceding cells plus their `;`
            // separators, offset by any leading whitespace on the line.
            let prefix_chars: usize = parts[..col_idx.min(parts.len())]
                .iter()
                .map(|p| p.chars().count())
                .sum::<usize>()
                + col_idx;
            let desc_column = leading_ws + prefix_chars;

            out.push((
                key.clone(),
                lang,
                LocEntry {
                    key: key.clone(),
                    value: None,
                    desc,
                    position,
                    desc_column,
                    error_range: None,
                    refs: Vec::new(),
                    commands: Vec::new(),
                    jomini_commands: Vec::new(),
                },
            ));
        }
    }

    out
}

/// Convenience wrapper: parse CSV and return a flat `Vec<LocEntry>` for a
/// single language. Uses `CK2_COLUMN_LANGS` column mapping.
#[cfg(test)]
fn parse_csv_loc_for_lang(text: &str, name: &str, lang: Lang) -> Vec<LocEntry> {
    parse_csv_loc_per_lang(text, name, None)
        .into_iter()
        .filter_map(|(_, l, e)| if l == lang { Some(e) } else { None })
        .collect()
}

/* ======================================================================== */
/* Tests                                                                   */
/* ======================================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "key1;English text;French text;German text;;Spanish text\n\
                          key2;More English;More French;;;\n\
                          # comment\n\
                          key3;Last entry;;;;";

    #[test]
    fn test_per_lang_english() {
        let entries = parse_csv_loc_for_lang(SAMPLE, "test.csv", Lang::English);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].key, "key1");
        assert_eq!(entries[0].desc, "English text");
        assert_eq!(entries[1].key, "key2");
        assert_eq!(entries[1].desc, "More English");
        assert_eq!(entries[2].key, "key3");
        assert_eq!(entries[2].desc, "Last entry");
    }

    #[test]
    fn test_per_lang_french() {
        let entries = parse_csv_loc_for_lang(SAMPLE, "test.csv", Lang::French);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].desc, "French text");
        assert_eq!(entries[1].desc, "More French");
        assert_eq!(entries[2].desc, "");
    }

    #[test]
    fn test_per_lang_spanish() {
        let entries = parse_csv_loc_for_lang(SAMPLE, "test.csv", Lang::Spanish);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].desc, "Spanish text");
    }

    #[test]
    fn test_comment_lines_skipped() {
        let text = "# this is a comment\nkey1;val;;;\n";
        let all = parse_csv_loc_per_lang(text, "test.csv", None);
        // only key1 — the comment line should not appear
        assert!(all.iter().all(|(k, _, _)| k == "key1"));
    }

    #[test]
    fn test_hash_code_header_skipped() {
        let text = "#CODE;ENGLISH;FRENCH;GERMAN;;SPANISH\nkey1;val;;;\n";
        let entries = parse_csv_loc_for_lang(text, "test.csv", Lang::English);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "key1");
    }

    #[test]
    fn test_empty_lines_skipped() {
        let entries = parse_csv_loc_for_lang("\n\n#comment\n\n", "test.csv", Lang::English);
        assert!(entries.is_empty());
    }

    #[test]
    fn test_per_lang_all_languages() {
        // One row should produce one entry per non-None column
        let text = "key1;E;F;G;;S\n";
        let all = parse_csv_loc_per_lang(text, "test.csv", None);
        // CK2_COLUMN_LANGS: col1=English, col2=French, col3=German, col5=Spanish
        let langs: Vec<Lang> = all.iter().map(|(_, l, _)| *l).collect();
        assert!(langs.contains(&Lang::English));
        assert!(langs.contains(&Lang::French));
        assert!(langs.contains(&Lang::German));
        assert!(langs.contains(&Lang::Spanish));
        // No duplicate for the skip columns
        assert_eq!(langs.len(), 4);
    }
}
