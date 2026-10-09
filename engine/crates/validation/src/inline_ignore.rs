use std::collections::{HashMap, HashSet};

const IGNORE_DIRECTIVE: &str = "cwtools-ignore";

pub type InlineIgnoreMap = HashMap<u32, HashSet<String>>;

pub fn inline_directive_codes(line: &str) -> Option<Vec<String>> {
    // Localization allows embedded unescaped quotes; its literal extends to
    // the final quote, unlike script tokens. Recognize only an entry prefix,
    // excluding script operators, and keep ambiguous quoted notes literal.
    let line = localization_literal_end(line).map_or(line, |end| &line[end..]);
    let mut quoted = false;
    let mut chars = line.char_indices();
    while let Some((idx, ch)) = chars.next() {
        match ch {
            '\\' if quoted => {
                // Match script string escapes: only quote and backslash consume
                // the following character. Quote state is local to this line,
                // so an unclosed string cannot hide a later standalone comment.
                if chars
                    .clone()
                    .next()
                    .is_some_and(|(_, c)| matches!(c, '"' | '\\'))
                {
                    chars.next();
                }
            }
            '"' => quoted = !quoted,
            '#' if !quoted => {
                // Everything after the first real hash is comment text. Keep
                // accepting a directive after a later hash within that comment.
                for (offset, _) in line[idx..].match_indices('#') {
                    let after = line[idx + offset + 1..].trim_start();
                    if !after
                        .get(..IGNORE_DIRECTIVE.len())
                        .is_some_and(|head| head.eq_ignore_ascii_case(IGNORE_DIRECTIVE))
                    {
                        continue;
                    }
                    let tokens = after.get(IGNORE_DIRECTIVE.len()..).unwrap_or("");
                    let tokens = tokens.split('#').next().unwrap_or("");
                    return Some(
                        tokens
                            .split_whitespace()
                            .map(|c| c.to_ascii_lowercase())
                            .collect(),
                    );
                }
                return None;
            }
            _ => {}
        }
    }
    None
}

fn localization_literal_end(line: &str) -> Option<usize> {
    let (key, rest) = line.trim_start().split_once(':')?;
    if key.trim().is_empty()
        || key
            .chars()
            .any(|c| matches!(c, '=' | '{' | '}' | '#' | '"' | '<' | '>'))
    {
        return None;
    }
    let rest = rest
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start();
    if !rest.starts_with('"') {
        return None;
    }
    let start = line.len() - rest.len();
    let mut chars = rest.char_indices();
    let mut last_quote = None;
    while let Some((idx, ch)) = chars.next() {
        if ch == '\\'
            && chars
                .clone()
                .next()
                .is_some_and(|(_, next)| matches!(next, '"' | '\\'))
        {
            chars.next();
        } else if ch == '"' {
            last_quote = Some(idx);
        }
    }
    // An opening quote alone leaves the remainder of this malformed line
    // literal. A later line starts fresh and can still contain a directive.
    Some(
        last_quote
            .filter(|idx| *idx > 0)
            .map_or(line.len(), |idx| start + idx + 1),
    )
}

pub fn extract_inline_ignored_codes(text: &str) -> InlineIgnoreMap {
    let mut map = InlineIgnoreMap::new();
    let needle = IGNORE_DIRECTIVE.as_bytes();
    if !text
        .as_bytes()
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle))
    {
        return map;
    }
    for (i, line) in text.lines().enumerate() {
        if let Some(codes) = inline_directive_codes(line) {
            map.insert(i as u32 + 1, codes.into_iter().collect());
        }
    }
    map
}

pub fn inline_suppressed(map: &InlineIgnoreMap, line: u32, code: &str) -> bool {
    if map.is_empty() || line == 0 || code.is_empty() {
        return false;
    }
    let code = code.to_ascii_lowercase();
    let mut candidate = line.saturating_sub(1);
    let ceiling = line.saturating_add(1);
    while candidate <= ceiling {
        if map
            .get(&candidate)
            .is_some_and(|codes| codes.contains(&code))
        {
            return true;
        }
        candidate += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_directive_examples_do_not_suppress_diagnostics() {
        for text in [
            r#"example = "literal # cwtools-ignore CW100 example""#,
            r#" KEY:0 "literal # cwtools-ignore CW100 example""#,
            r#"example = "escaped \" # cwtools-ignore CW100 example""#,
            r#" KEY:0 "escaped \" # cwtools-ignore CW100 example""#,
            r#"example = "unclosed # cwtools-ignore CW100"#,
            r#" KEY:0 "unclosed # cwtools-ignore CW100"#,
        ] {
            let map = extract_inline_ignored_codes(text);
            for line in 1..=2 {
                assert!(
                    !inline_suppressed(&map, line, "CW100"),
                    "{text:?}, line {line}"
                );
            }
        }
    }

    #[test]
    fn real_comments_after_quoted_hashes_and_escapes_still_suppress() {
        for text in [
            r#"example = "literal # hash" # CWTOOLS-IGNORE cw100 CW246"#,
            r#" KEY:0 "literal # hash" # CWTOOLS-IGNORE cw100 CW246"#,
            r#"example = "escaped \" quote" # cwtools-ignore CW100 CW246"#,
            r#" KEY:0 "escaped \" quote" # cwtools-ignore CW100 CW246"#,
            r#"example = "backslash \\" # cwtools-ignore CW100 CW246"#,
            r#"# note with "unclosed quote # cwtools-ignore CW100 CW246"#,
        ] {
            let map = extract_inline_ignored_codes(text);
            assert!(inline_suppressed(&map, 1, "CW100"), "{text:?}");
            assert!(inline_suppressed(&map, 2, "CW246"), "{text:?}");
        }
    }

    #[test]
    fn unclosed_quotes_recover_at_newlines_for_raw_source_directives() {
        let text = "bad = \"unclosed # cwtools-ignore CW100\n# cwtools-ignore CW246\nnext = {\n";
        let map = extract_inline_ignored_codes(text);
        assert!(!inline_suppressed(&map, 1, "CW100"));
        assert!(inline_suppressed(&map, 1, "CW246"));
        assert!(inline_suppressed(&map, 3, "CW246"));
    }

    #[test]
    fn localization_embedded_quotes_and_quoted_notes_are_literal() {
        for line in [
            r#" KEY:0 "text "quoted # cwtools-ignore CW100 example" rest""#,
            r#" invalid key:0 "text "quoted # cwtools-ignore CW100 example" rest""#,
            r#" KEY:0 "value" # cwtools-ignore CW100 # "note""#,
            r#" KEY:0 "escaped \" # cwtools-ignore CW100 example""#,
        ] {
            assert!(extract_inline_ignored_codes(line).is_empty(), "{line}");
        }
    }

    #[test]
    fn valid_localization_embedded_quotes_keep_undefined_reference_diagnostics() {
        let text =
            "l_english:\n KEY:0 \"$missing_key$ \"quoted # cwtools-ignore CW225 example\" rest\"\n";
        let file = cwtools_localization::parse_loc_text(text, "test_l_english.yml").unwrap();
        assert!(file.parse_errors.is_empty());
        let errors = cwtools_localization::validate_loc_file(
            &file,
            &["key".into()].into_iter().collect(),
            &HashSet::new(),
            &[] as &[&str],
        );
        assert!(
            !errors
                .iter()
                .any(|error| error.kind == cwtools_localization::LocErrorKind::LocMissingQuote)
        );
        let error = errors
            .iter()
            .find(|error| {
                matches!(
                    error.kind,
                    cwtools_localization::LocErrorKind::UndefinedLocReference { .. }
                )
            })
            .expect("undefined reference diagnostic");
        let map = extract_inline_ignored_codes(text);
        assert!(!inline_suppressed(&map, error.line as u32, "CW225"));
    }

    #[test]
    fn plain_text_yields_no_directives() {
        let map = extract_inline_ignored_codes("foo = 1\nbar = 2\n");
        assert!(map.is_empty());
    }

    #[test]
    fn same_line_directive_names_its_codes() {
        let map = extract_inline_ignored_codes("foo = 1 # cwtools-ignore CW100 CW246\n");
        assert!(inline_suppressed(&map, 1, "CW100"));
        assert!(inline_suppressed(&map, 1, "CW246"));
        assert!(!inline_suppressed(&map, 1, "CW107"));
    }

    #[test]
    fn standalone_directive_covers_the_adjacent_lines() {
        let text = "foo = 1\n# cwtools-ignore CW100\nbar = 2\n";
        let map = extract_inline_ignored_codes(text);
        assert!(inline_suppressed(&map, 1, "CW100"), "line above");
        assert!(inline_suppressed(&map, 2, "CW100"), "own line");
        assert!(inline_suppressed(&map, 3, "CW100"), "line below");
        assert!(!inline_suppressed(&map, 4, "CW100"), "two lines away");
    }

    #[test]
    fn codes_match_case_insensitively() {
        let map = extract_inline_ignored_codes("foo = 1 # CWTOOLS-IGNORE cw100\n");
        assert!(inline_suppressed(&map, 1, "CW100"));
        assert!(inline_suppressed(&map, 1, "cw100"));
    }

    #[test]
    fn trailing_note_after_a_second_marker_is_not_a_code() {
        let map = extract_inline_ignored_codes("foo = 1 # cwtools-ignore CW100 # not a code\n");
        assert!(inline_suppressed(&map, 1, "CW100"));
        assert!(!inline_suppressed(&map, 1, "not"));
        assert!(!inline_suppressed(&map, 1, "a"));
        assert!(!inline_suppressed(&map, 1, "code"));
    }

    #[test]
    fn directive_without_codes_suppresses_nothing() {
        let map = extract_inline_ignored_codes("foo = 1 # cwtools-ignore\n");
        assert!(map.contains_key(&1));
        assert!(!inline_suppressed(&map, 1, "CW100"));
    }

    #[test]
    fn empty_code_is_never_suppressed() {
        let map = extract_inline_ignored_codes("foo = 1 # cwtools-ignore CW100\n");
        assert!(!inline_suppressed(&map, 1, ""));
    }

    #[test]
    fn line_zero_is_not_suppressed_by_line_one() {
        let map = extract_inline_ignored_codes("# cwtools-ignore CW100\nfoo = 1\n");
        assert!(!inline_suppressed(&map, 0, "CW100"));
    }
}
