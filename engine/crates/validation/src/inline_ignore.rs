use std::collections::{HashMap, HashSet};

const IGNORE_DIRECTIVE: &str = "cwtools-ignore";

pub type InlineIgnoreMap = HashMap<u32, HashSet<String>>;

pub fn inline_directive_codes(line: &str) -> Option<Vec<String>> {
    for (idx, _) in line.match_indices('#') {
        let rest = &line[idx + 1..];
        let after = rest.trim_start();
        if !after
            .get(..IGNORE_DIRECTIVE.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(IGNORE_DIRECTIVE))
        {
            continue;
        }
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
