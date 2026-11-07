use crate::commands::{Lang, LocEntry, Position};
use crate::loc_string::{LocElement, parse_loc_elements};
use std::sync::Arc;

pub const CK2_COLUMN_LANGS: &[Option<Lang>] = &[
    None,                // col 0: key
    Some(Lang::English), // col 1
    Some(Lang::French),  // col 2
    Some(Lang::German),  // col 3
    None,                // col 4: empty column (CK2/VIC2 schema gap)
    Some(Lang::Spanish), // col 5
];

pub(crate) fn parse_csv_loc_per_lang(
    text: &str,
    name: &str,
    column_langs: Option<&[Option<Lang>]>,
) -> Vec<(String, Lang, LocEntry)> {
    let column_langs = column_langs.unwrap_or(CK2_COLUMN_LANGS);
    let stream_name: Arc<str> = Arc::from(name);
    let mut out = Vec::new();
    let has_bom = text.starts_with('\u{feff}');
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    for (line_num, line) in text.lines().enumerate() {
        let trimmed = line.trim();

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

        let leading_ws = line.chars().count() - line.trim_start().chars().count();

        for (col_idx, maybe_lang) in column_langs.iter().enumerate().skip(1) {
            let Some(lang) = *maybe_lang else { continue };

            let desc = parts.get(col_idx).copied().unwrap_or("").to_string();
            // Collect references without enabling YAML/Jomini command validation.
            let refs = if desc.len() > crate::yaml_parser::MAX_LOC_VALUE_BYTES {
                Vec::new()
            } else {
                parse_loc_elements(&desc)
                    .into_iter()
                    .filter_map(|e| match e {
                        LocElement::Ref(r) => Some(r.to_string()),
                        _ => None,
                    })
                    .collect()
            };

            let position = Position::new(Arc::clone(&stream_name), line_num + 1, 1);
            let prefix_chars: usize = parts[..col_idx.min(parts.len())]
                .iter()
                .map(|p| p.chars().count())
                .sum::<usize>()
                + col_idx;
            let desc_column = leading_ws + prefix_chars + usize::from(has_bom && line_num == 0);

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
                    refs,
                    commands: Vec::new(),
                    jomini_commands: Vec::new(),
                },
            ));
        }
    }

    out
}

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
    fn leading_bom_is_not_part_of_key_or_header() {
        let entries = parse_csv_loc_for_lang(
            "\u{feff}key;Text;;;;\nnext\u{feff};Other;;;;",
            "test.csv",
            Lang::English,
        );
        assert_eq!(entries[0].key, "key");
        assert_eq!(entries[0].desc_column, 5);
        assert_eq!(entries[1].key, "next\u{feff}");
        let header = parse_csv_loc_for_lang(
            "\u{feff}#CODE;ENGLISH\nkey;Text;;;;",
            "test.csv",
            Lang::English,
        );
        assert_eq!(header.len(), 1);
        assert_eq!(header[0].key, "key");
    }

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
    fn dollar_references_are_extracted_but_commands_stay_empty() {
        let text = "# ignored;$comment$;;;;\nkey1;$en$ [GetName] [Scope.Owner.GetName];$fr$;$de$;$gap$;$es$\n";
        let all = parse_csv_loc_per_lang(text, "test.csv", None);
        assert_eq!(all.len(), 4);
        for ((_, _, entry), expected) in all.iter().zip(["en", "fr", "de", "es"]) {
            assert_eq!(entry.refs, [expected], "{}", entry.desc);
            assert!(
                entry.commands.is_empty(),
                "CSV brackets are not commands: {}",
                entry.desc
            );
            assert!(
                entry.jomini_commands.is_empty(),
                "CSV brackets are not commands: {}",
                entry.desc
            );
        }
    }

    #[test]
    fn dollar_reference_extraction_respects_the_value_size_limit() {
        let cap = crate::yaml_parser::MAX_LOC_VALUE_BYTES;
        for (len, expected) in [
            (cap - 1, vec!["ref"]),
            (cap, vec!["ref"]),
            (cap + 1, vec![]),
        ] {
            let desc = format!("$ref${}", "x".repeat(len - "$ref$".len()));
            let text = format!("key;{desc};;;;\n");
            let entries = parse_csv_loc_for_lang(&text, "test.csv", Lang::English);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].refs, expected, "value length: {len}");
            assert_eq!(entries[0].desc, desc);
        }
    }

    #[test]
    fn malformed_dollar_runs_are_not_references() {
        let text = "key1;costs 100$ and $5 [x];F;;S\n";
        let all = parse_csv_loc_per_lang(text, "test.csv", None);
        assert!(all.iter().all(|(_, _, entry)| entry.refs.is_empty()));
    }

    #[test]
    fn test_comment_lines_skipped() {
        let text = "# this is a comment\nkey1;val;;;\n";
        let all = parse_csv_loc_per_lang(text, "test.csv", None);
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
        let text = "key1;E;F;G;;S\n";
        let all = parse_csv_loc_per_lang(text, "test.csv", None);
        let langs: Vec<Lang> = all.iter().map(|(_, l, _)| *l).collect();
        assert!(langs.contains(&Lang::English));
        assert!(langs.contains(&Lang::French));
        assert!(langs.contains(&Lang::German));
        assert!(langs.contains(&Lang::Spanish));
        assert_eq!(langs.len(), 4);
    }
}
