use crate::commands::{Lang, LocEntry, LocFile, LocParseError, Position, key_to_language};
use crate::loc_string::parse_loc_elements;
use std::sync::Arc;

pub const MAX_LOC_VALUE_BYTES: usize = 64 * 1024;

// ---- UTF-8 BOM check -------------------------------------------------------

const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

/// Diagnostic produced when a `.yml` loc file is missing the UTF-8 BOM.
#[derive(Debug, Clone, PartialEq)]
pub struct MissingBomDiagnostic {
    pub file: String,
}

/// Check whether `bytes` starts with the UTF-8 BOM (0xEF 0xBB 0xBF).
/// Returns `Ok(())` when the BOM is present, `Err(MissingBomDiagnostic)`
pub fn check_utf8_bom(bytes: &[u8], file: &str) -> Result<(), MissingBomDiagnostic> {
    if bytes.len() >= 3 && bytes[..3] == UTF8_BOM {
        Ok(())
    } else {
        Err(MissingBomDiagnostic {
            file: file.to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LangHeaderDiagnostic {
    MissingLocFileLangHeader {
        file: String,
    },
    MissingLocFileLang {
        file: String,
    },
    LocFileLangMismatch {
        file: String,
        filename_lang: Lang,
        header_lang: Lang,
    },
}

pub fn lang_from_filename(stem: &str) -> Option<Lang> {
    let lower = stem.to_ascii_lowercase();
    if lower.contains("l_english") {
        Some(Lang::English)
    } else if lower.contains("l_french") {
        Some(Lang::French)
    } else if lower.contains("l_german") {
        Some(Lang::German)
    } else if lower.contains("l_spanish") {
        Some(Lang::Spanish)
    } else if lower.contains("l_russian") {
        Some(Lang::Russian)
    } else if lower.contains("l_polish") {
        Some(Lang::Polish)
    } else if lower.contains("l_braz_por") {
        Some(Lang::BrazPor)
    } else if lower.contains("l_simp_chinese") {
        Some(Lang::SimpChinese)
    } else if lower.contains("l_japanese") {
        Some(Lang::Japanese)
    } else if lower.contains("l_korean") {
        Some(Lang::Korean)
    } else if lower.contains("l_turkish") {
        Some(Lang::Turkish)
    } else if lower.contains("l_default") {
        Some(Lang::Default)
    } else {
        None
    }
}

pub fn check_loc_file_lang(file: &str, header_key: &str) -> Option<LangHeaderDiagnostic> {
    let stem = std::path::Path::new(file)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(file);

    if stem.eq_ignore_ascii_case("languages") {
        return None;
    }

    let header_lang = key_to_language(header_key);

    // l_default in the header is always OK (mirrors F# `STLLang.Default` branch)
    if header_key.eq_ignore_ascii_case("l_default") {
        return None;
    }

    let header_lang = match header_lang {
        Some(l) => l,
        None => {
            return Some(LangHeaderDiagnostic::MissingLocFileLangHeader {
                file: file.to_string(),
            });
        }
    };

    let filename_lang = match lang_from_filename(stem) {
        Some(l) => l,
        None => {
            return Some(LangHeaderDiagnostic::MissingLocFileLang {
                file: file.to_string(),
            });
        }
    };

    if filename_lang != header_lang {
        return Some(LangHeaderDiagnostic::LocFileLangMismatch {
            file: file.to_string(),
            filename_lang,
            header_lang,
        });
    }

    None
}

/// Per-entry lenient failures are `LocParseError` on the file instead.
#[derive(Debug, Clone, PartialEq)]
pub enum LocFileParseError {
    EmptyFile,
    MissingColon { header: String },
}

impl std::fmt::Display for LocFileParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyFile => f.write_str("empty file after stripping comments"),
            Self::MissingColon { header } => {
                write!(f, "missing ':' in language header: {header:?}")
            }
        }
    }
}

impl std::error::Error for LocFileParseError {}

pub fn parse_loc_text(text: &str, name: &str) -> Result<LocFile, LocFileParseError> {
    let stream_name: Arc<str> = Arc::from(name);
    // Strip leading UTF-8 BOM(s). Loc files are required to be UTF-8-with-BOM
    // (see CW254) and the disk reader keeps the BOM in the string, so without
    // Some real files in the wild carry a doubled BOM (`\u{FEFF}\u{FEFF}`);
    // F# tolerates it via a substring match, so strip every leading BOM.
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.lines().enumerate();

    let header_line = loop {
        match lines.next() {
            Some((_, line)) => {
                let trimmed = line.trim_start();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                break line;
            }
            None => return Err(LocFileParseError::EmptyFile),
        }
    };

    let header = header_line;
    let colon = header
        .find(':')
        .ok_or_else(|| LocFileParseError::MissingColon {
            header: header.to_string(),
        })?;
    let language_key = header[..colon].trim();
    let lang = key_to_language(language_key);

    let mut entries = Vec::new();
    let mut parse_errors: Vec<LocParseError> = Vec::new();

    for (i, line) in lines {
        match parse_entry(line, i, &stream_name) {
            Ok(Some(entry)) => entries.push(entry),
            Ok(None) => continue, // blank / comment line
            Err(pe) => parse_errors.push(pe),
        }
    }

    Ok(LocFile {
        path: name.to_string(),
        language_prefix: language_key.to_string(),
        lang,
        is_csv: false,
        entries,
        parse_errors,
        encoding: None,
    })
}

///   records the CW001 parse error and continues recovering (lenient parser).
fn parse_entry(
    line: &str,
    line_idx: usize,
    stream_name: &Arc<str>,
) -> Result<Option<LocEntry>, LocParseError> {
    let trimmed = line.trim_start();

    if trimmed.is_empty() || trimmed.starts_with('#') {
        return Ok(None);
    }

    let Some(colon_pos) = trimmed.find(':') else {
        // continue recovering (lenient parser; mirrors F# `Failure` path).
        return Err(LocParseError {
            line: line_idx + 1,
            message: format!("unexpected content (no ':' separator): {:?}", trimmed),
        });
    };
    let key = trimmed[..colon_pos].trim_end();

    let mut remainder = &trimmed[colon_pos + 1..];

    let version = if !remainder.is_empty() && remainder.starts_with(|c: char| c.is_ascii_digit()) {
        let digit_len = remainder.bytes().take_while(|b| b.is_ascii_digit()).count();
        let v = remainder[..digit_len].parse::<u32>().ok();
        remainder = &remainder[digit_len..];
        v
    } else {
        None
    };

    // strip one leading space (the convention after `:`)
    let desc = remainder.strip_prefix(' ').unwrap_or(remainder);

    let position = Position::new(Arc::clone(stream_name), line_idx + 1, 1); // 1-based line numbers

    let leading_ws = line.len() - trimmed.len();
    let desc_col_offset = leading_ws + (trimmed.len() - desc.len());

    let error_range = find_invalid_loc_char(desc).map(|byte_off| {
        let col = desc_col_offset + desc[..byte_off].chars().count() + 1;
        Position::new(Arc::clone(stream_name), line_idx + 1, col)
    });

    let mut refs: Vec<String> = Vec::new();
    let mut commands: Vec<String> = Vec::new();
    let mut jomini_commands: Vec<Vec<crate::loc_string::JominiCommand>> = Vec::new();
    if desc.len() > MAX_LOC_VALUE_BYTES {
        tracing::warn!(
            key,
            bytes = desc.len(),
            cap = MAX_LOC_VALUE_BYTES,
            "localisation value past the length cap; its refs and commands are not read"
        );
    } else {
        for e in &parse_loc_elements(desc) {
            match e {
                crate::loc_string::LocElement::Ref(s) => refs.push(s.to_string()),
                crate::loc_string::LocElement::Command(s) => commands.push(s.to_string()),
                crate::loc_string::LocElement::JominiCommand(cmds) => {
                    jomini_commands.push(cmds.clone())
                }
                crate::loc_string::LocElement::Chars(_) => {}
            }
        }
    }

    Ok(Some(LocEntry {
        key: key.to_string(),
        value: version,
        desc: desc.to_string(),
        position,
        desc_column: desc_col_offset,
        error_range, // set by isLocValueChar check above
        refs,
        commands,
        jomini_commands,
    }))
}

pub fn is_loc_value_char(c: char) -> bool {
    let u = c as u32;
    c.is_ascii_alphabetic()
        || (0x0020..=0x007E).contains(&u)
        || (0x00A0..=0x024F).contains(&u)
        || (0x0400..=0x052F).contains(&u)
        || (0x1E00..=0x1EFF).contains(&u)
        || (0x2010..=0x2044).contains(&u)
        || (0x2460..=0x24FF).contains(&u)
        || (0x4E00..=0x9FFF).contains(&u)
        || (0x3000..=0x30FF).contains(&u)
        || (0x3400..=0x4DBF).contains(&u)
        || (0xFE30..=0xFE4F).contains(&u)
        || (0xFF00..=0xFFEF).contains(&u)
        || (0x1100..=0x11FF).contains(&u)
        || (0x3130..=0x318F).contains(&u)
        || (0xAC00..=0xD7A3).contains(&u)
        || (0x0600..=0x06FF).contains(&u)
        || (0x0750..=0x077F).contains(&u)
        || (0xFB50..=0xFDFF).contains(&u)
        || (0xFE70..=0xFEFF).contains(&u)
        || (0x0250..=0x02FF).contains(&u)
        || (0x0300..=0x036F).contains(&u)
        || (0x0370..=0x03FF).contains(&u)
        || (0x0531..=0x058F).contains(&u)
        || (0x0900..=0x097F).contains(&u)
        || (0x1200..=0x137F).contains(&u)
        || (0x20A0..=0x20CF).contains(&u)
        || (0x2100..=0x218F).contains(&u)
        || (0x2190..=0x21FF).contains(&u)
        || (0x2D30..=0x2D7F).contains(&u)
}

pub fn find_invalid_loc_char(desc: &str) -> Option<usize> {
    for (offset, c) in desc.char_indices() {
        if !is_loc_value_char(c) {
            return Some(offset);
        }
    }
    None
}

/* ======================================================================== */
/* Tests                                                                   */
/* ======================================================================== */

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Lang;

    #[test]
    fn test_loc_value_char_accepts_supported_scripts() {
        for c in [
            '한', 'ᄀ', 'ㄱ', // Korean
            'م', 'ا', // Arabic
            'あ', 'カ', '日', // Japanese
            '中', '文', '㐀', // Chinese (㐀 = U+3400, Ext A)
        ] {
            assert!(
                is_loc_value_char(c),
                "char {c:?} (U+{:04X}) should be valid",
                c as u32
            );
        }
    }

    #[test]
    fn loc_value_char_accepts_additional_legit_scripts_and_symbols() {
        for c in [
            '\u{2116}', // № numero sign
            '\u{2192}', // → rightwards arrow
            '\u{20AC}', // € euro sign
            '\u{2011}', // non-breaking hyphen
            '\u{2161}', // Ⅱ roman numeral two
            '\u{0301}', // combining acute accent
            '\u{0307}', // combining dot above
            '\u{0259}', // ə latin schwa (IPA)
            '\u{02BC}', // ʼ modifier letter apostrophe
            '\u{02BF}', // modifier letter left half ring
            '\u{0540}', // Հ Armenian
            '\u{0531}', // Ա Armenian
            '\u{03BF}', // ο Greek
            '\u{050C}', // Ԍ Cyrillic (Komi)
            '\u{049B}', // қ Cyrillic (Kazakh)
            '\u{2D63}', // Tifinagh yaz
            '\u{12E8}', // Ethiopic
            '\u{0915}', // क Devanagari ka
        ] {
            assert!(
                is_loc_value_char(c),
                "char U+{:04X} should be valid",
                c as u32
            );
        }
    }

    #[test]
    fn loc_value_char_still_rejects_invisible_junk() {
        for c in [
            '\u{200B}', // zero width space
            '\u{2007}', // figure space
            '\u{200E}', // left-to-right mark
            '\u{0009}', // tab
            '\u{FFFD}', // replacement char (mojibake)
        ] {
            assert!(
                !is_loc_value_char(c),
                "char U+{:04X} should remain invalid",
                c as u32
            );
        }
    }

    #[test]
    fn test_loc_value_char_finds_no_invalid_in_korean_value() {
        assert_eq!(
            find_invalid_loc_char("\"전쟁이 시작되었다 [USA.GetName]\""),
            None
        );
    }

    #[test]
    fn test_parse_language() {
        assert_eq!(key_to_language("l_english"), Some(Lang::English));
        assert_eq!(key_to_language("l_french"), Some(Lang::French));
        assert_eq!(key_to_language("l_unknown"), None);
    }

    #[test]
    fn test_hoi4_cursed_quotes() {
        let text = "l_english:\n loc_key1: \"this is valid loc\"\n loc_key2: \"this is \"also\" valid loc\"\n loc_key3: \"this is \\\"also\\\" valid loc\"\n loc_key4: \"this is \\\"also valid loc\"\n loc_key5: \"this is \"also valid loc\"\n loc_key6: \"this is invalid loc\n loc_key7: this is invalid loc\"\n loc_key8: this is invalid loc\n loc_key9: \"this is valid loc\" but with invalid stuff outside\n loc_key10: \"this is valid loc\" #but with comment\n";

        let file = parse_loc_text(text, "test.yml").unwrap();
        assert_eq!(file.lang, Some(Lang::English));
        assert_eq!(file.entries.len(), 10);

        assert_eq!(file.entries[0].key, "loc_key1");
        assert_eq!(file.entries[0].desc, "\"this is valid loc\"");

        assert_eq!(file.entries[1].key, "loc_key2");
        assert_eq!(file.entries[1].desc, "\"this is \"also\" valid loc\"");

        assert_eq!(file.entries[2].key, "loc_key3");
        assert_eq!(file.entries[2].desc, "\"this is \\\"also\\\" valid loc\"");

        assert_eq!(file.entries[3].key, "loc_key4");
        assert_eq!(file.entries[3].desc, "\"this is \\\"also valid loc\"");

        assert_eq!(file.entries[4].key, "loc_key5");
        assert_eq!(file.entries[4].desc, "\"this is \"also valid loc\"");

        assert_eq!(file.entries[5].key, "loc_key6");
        assert_eq!(file.entries[5].desc, "\"this is invalid loc");

        assert_eq!(file.entries[6].key, "loc_key7");
        assert_eq!(file.entries[6].desc, "this is invalid loc\"");

        assert_eq!(file.entries[7].key, "loc_key8");
        assert_eq!(file.entries[7].desc, "this is invalid loc");

        assert_eq!(file.entries[8].key, "loc_key9");
        assert_eq!(
            file.entries[8].desc,
            "\"this is valid loc\" but with invalid stuff outside"
        );

        assert_eq!(file.entries[9].key, "loc_key10");
        assert_eq!(
            file.entries[9].desc,
            "\"this is valid loc\" #but with comment"
        );
    }

    #[test]
    fn test_version_number() {
        let text = "l_english:\n key:0 \"desc\" \n";
        let file = parse_loc_text(text, "test.yml").unwrap();
        assert_eq!(file.entries[0].value, Some(0));
        assert_eq!(file.entries[0].desc, "\"desc\" ");
    }

    #[test]
    fn test_comments_in_desc() {
        let text = "l_english:\n key: \"a\"#comment\n";
        let file = parse_loc_text(text, "test.yml").unwrap();
        assert_eq!(file.entries[0].desc, "\"a\"#comment");
    }

    #[test]
    fn test_loc_key11_complex() {
        let text = "l_english:\n loc_key11: \"this is valid loc\" #but this is also valid and read as part of the string due to quote after\n";
        let file = parse_loc_text(text, "test.yml").unwrap();
        assert_eq!(file.entries[0].key, "loc_key11");
    }

    #[test]
    fn test_empty_file() {
        assert!(parse_loc_text("", "test.yml").is_err());
    }

    #[test]
    fn loc_file_parse_error_empty_file_variant_and_display() {
        let err = parse_loc_text("", "test.yml").unwrap_err();
        assert_eq!(err, LocFileParseError::EmptyFile);
        assert_eq!(err.to_string(), "empty file after stripping comments");
        let err2 = parse_loc_text("# just a comment\n\n", "test.yml").unwrap_err();
        assert_eq!(err2, LocFileParseError::EmptyFile);
    }

    #[test]
    fn loc_file_parse_error_missing_colon_variant_and_display() {
        let err = parse_loc_text("l_english\n key:0 \"v\"\n", "test.yml").unwrap_err();
        match &err {
            LocFileParseError::MissingColon { header } => assert_eq!(header, "l_english"),
            other => panic!("expected MissingColon, got {other:?}"),
        }
        assert_eq!(
            err.to_string(),
            "missing ':' in language header: \"l_english\""
        );
    }

    #[test]
    fn loc_file_parse_error_propagates_through_service_boundary() {
        let err = crate::service::parse_loc_files("bad.yml", "no colon here\n", None).unwrap_err();
        assert!(matches!(err, LocFileParseError::MissingColon { .. }));
        assert!(err.to_string().contains("missing ':'"));
        let svc = crate::service::LocService::from_files(vec![(
            "bad.yml".to_string(),
            "no colon here\n".to_string(),
        )]);
        assert_eq!(svc.files().len(), 0);
        assert!(!svc.errors().is_empty());
        assert!(svc.errors()[0].1.contains("missing ':'"));
    }

    // ---- UTF-8 BOM tests ---------------------------------------------------

    #[test]
    fn test_bom_present() {
        let bytes: &[u8] = &[0xEF, 0xBB, 0xBF, b'l', b'_'];
        assert!(check_utf8_bom(bytes, "test.yml").is_ok());
    }

    #[test]
    fn test_bom_missing() {
        let bytes: &[u8] = b"l_english:\n";
        let result = check_utf8_bom(bytes, "test.yml");
        assert!(result.is_err());
        let diag = result.unwrap_err();
        assert_eq!(diag.file, "test.yml");
    }

    #[test]
    fn test_bom_too_short() {
        let bytes: &[u8] = &[0xEF, 0xBB];
        assert!(check_utf8_bom(bytes, "short.yml").is_err());
    }

    #[test]
    fn test_bom_wrong_bytes() {
        // UTF-16 LE BOM — not a UTF-8 BOM
        let bytes: &[u8] = &[0xFF, 0xFE, 0x00];
        assert!(check_utf8_bom(bytes, "utf16.yml").is_err());
    }

    #[test]
    fn test_lang_header_default_is_ok() {
        assert_eq!(
            check_loc_file_lang("events_l_english.yml", "l_default"),
            None
        );
    }

    #[test]
    fn test_bom_prefixed_header_resolves_language() {
        // Loc files are UTF-8-with-BOM and the disk reader keeps the BOM in the
        // index silently empties → mass false CW100). See the BOM-strip in
        let text = "\u{feff}l_english:\n KEY_A: \"value\"\n";
        let file = parse_loc_text(text, "abilities_l_english.yml").unwrap();
        assert_eq!(file.lang, Some(Lang::English), "BOM must not hide the lang");
        assert_eq!(file.entries.len(), 1);
        assert_eq!(file.entries[0].key, "KEY_A");
    }

    #[test]
    fn test_leading_space_header_resolves_language() {
        // `<BOM> l_english:` — a leading space before the language token. F#
        let text = "\u{feff} l_english:\n KEY_A: \"value\"\n";
        let file = parse_loc_text(text, "factions_l_english.yml").unwrap();
        assert_eq!(file.lang, Some(Lang::English));
        assert_eq!(file.language_prefix, "l_english");
        assert_eq!(
            check_loc_file_lang("factions_l_english.yml", &file.language_prefix),
            None
        );
    }

    #[test]
    fn test_double_bom_header_resolves_language() {
        // `<BOM><BOM>l_french:` — a doubled BOM. F# matches the language as a
        // substring and tolerates it; strip every leading BOM.
        let text = "\u{feff}\u{feff}l_french:\n KEY_A: \"value\"\n";
        let file = parse_loc_text(text, "lockeys_l_french.yml").unwrap();
        assert_eq!(file.lang, Some(Lang::French));
        assert_eq!(
            check_loc_file_lang("lockeys_l_french.yml", &file.language_prefix),
            None
        );
    }

    #[test]
    fn test_languages_yml_exempt() {
        assert_eq!(check_loc_file_lang("languages.yml", "l_english"), None);
    }

    #[test]
    fn test_commands_in_desc() {
        let text = "l_english:\n key: \"Hello $TITLE$ [GetName]\"\n";
        let file = parse_loc_text(text, "test.yml").unwrap();
        let entry = &file.entries[0];
        assert_eq!(entry.desc, "\"Hello $TITLE$ [GetName]\"");
        assert_eq!(entry.refs, vec!["TITLE"]);
        assert_eq!(entry.commands, vec!["GetName"]);
    }

    #[test]
    fn test_event_target_command() {
        let text = "l_english:\n key: \"[event_target:foo]\"\n";
        let file = parse_loc_text(text, "test.yml").unwrap();
        assert_eq!(file.entries[0].commands, vec!["event_target:foo"]);
    }

    #[test]
    fn test_lone_quote_in_jomini_param_does_not_panic() {
        let text = "l_english:\n TEST_KEY:0 \"[GetName(')]\"\n";
        let file = parse_loc_text(text, "test_l_english.yml").unwrap();
        assert_eq!(file.entries.len(), 1);
        assert_eq!(file.entries[0].key, "TEST_KEY");
    }

    #[test]
    fn test_mixed_case_header_resolves_language() {
        let text = "L_English:\n KEY_A: \"value\"\n";
        let file = parse_loc_text(text, "events_l_english.yml").unwrap();
        assert_eq!(file.lang, Some(Lang::English));
        assert_eq!(file.entries.len(), 1);
        assert_eq!(file.entries[0].key, "KEY_A");
        assert_eq!(
            check_loc_file_lang("events_l_english.yml", "L_English"),
            None
        );
    }

    #[test]
    fn test_question_variable() {
        let text = "l_english:\n key: \"[?my_var]\"\n";
        let file = parse_loc_text(text, "test.yml").unwrap();
        assert_eq!(file.entries[0].commands, vec!["?my_var"]);
    }

    #[test]
    fn value_past_the_cap_keeps_its_key_and_skips_command_parsing() {
        let long = "[GetName]".repeat(MAX_LOC_VALUE_BYTES / 9 + 1);
        assert!(long.len() > MAX_LOC_VALUE_BYTES);
        let text = format!("l_english:\n KEY_LONG: \"{long}\"\n");
        let file = parse_loc_text(&text, "test_l_english.yml").unwrap();

        assert_eq!(file.entries.len(), 1);
        assert_eq!(file.entries[0].key, "KEY_LONG");
        assert!(file.entries[0].desc.len() > MAX_LOC_VALUE_BYTES);
        assert!(file.entries[0].commands.is_empty());
        assert!(
            file.parse_errors.is_empty(),
            "the cap is not a parse error: {:?}",
            file.parse_errors
        );
    }

    #[test]
    fn value_under_the_cap_still_reads_its_commands() {
        let filler = "x".repeat(MAX_LOC_VALUE_BYTES - 32);
        let text = format!("l_english:\n KEY_BIG: \"{filler}[GetName]\"\n");
        let file = parse_loc_text(&text, "test_l_english.yml").unwrap();
        assert_eq!(file.entries[0].commands, vec!["GetName"]);
    }
}
