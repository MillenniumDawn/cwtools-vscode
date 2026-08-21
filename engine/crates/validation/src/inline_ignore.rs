//! Inline diagnostic suppression: `# cwtools-ignore CWxxx` comments.
//!
//! A directive on line M suppresses the named codes on line M itself and on
//! the lines directly above and below it, so both written forms work: the
//! comment trailing the offending line, and the comment on its own line
//! beside it (either side). Codes match case-insensitively, like the
//! workspace-level `errors.ignore` setting. The directive text is scanned
//! from raw source, not the AST, so it also covers files whose comments are
//! discarded by the parse cache (`parse_string_without_comments`) and parse
//! errors in files that never formed a tree.

use std::collections::{HashMap, HashSet};

/// The directive word, matched case-insensitively after a `#` comment marker.
const IGNORE_DIRECTIVE: &str = "cwtools-ignore";

/// 1-based line of each directive → the lowercased codes it names.
pub type InlineIgnoreMap = HashMap<u32, HashSet<String>>;

/// Codes a single line's `# cwtools-ignore` directive names, lowercased.
/// `None` when the line carries no directive.
pub fn inline_directive_codes(line: &str) -> Option<Vec<String>> {
    for (idx, _) in line.match_indices('#') {
        let rest = &line[idx + 1..];
        let after = rest.trim_start();
        // `get(..len)` rather than a direct slice: a non-ASCII char before the
        // directive word can put the byte cut inside a char boundary.
        if !after
            .get(..IGNORE_DIRECTIVE.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(IGNORE_DIRECTIVE))
        {
            continue;
        }
        // One directive per line: tokens run to the next `#`, so a trailing
        // human note after a second comment marker never counts as a code.
        let tokens = after.get(IGNORE_DIRECTIVE.len()..).unwrap_or("");
        let tokens = tokens.split('#').next().unwrap_or("");
        let codes: Vec<String> = tokens
            .split_whitespace()
            .map(|c| c.to_ascii_lowercase())
            .collect();
        return Some(codes);
    }
    None
}

/// Extract every `# cwtools-ignore` directive from `text` into an
/// [`InlineIgnoreMap`]. Empty unless the text mentions the directive at all,
/// so the common file costs one substring scan.
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

/// Whether a diagnostic with 1-based `line` and `code` is suppressed by the
/// directives in `map`: a directive on the diagnostic's own line, or on the
/// line directly above or below it.
pub fn inline_suppressed(map: &InlineIgnoreMap, line: u32, code: &str) -> bool {
    // Line 0 is a whole-file diagnostic with no line; a line-1 directive
    // must not suppress it.
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
