use tower_lsp::lsp_types::*;

pub(crate) fn sort_for_kind(kind: Option<CompletionItemKind>, label: &str) -> Option<String> {
    let bucket = match kind? {
        CompletionItemKind::FIELD => "1",   // specific leaf key (concrete)
        CompletionItemKind::STRUCT => "2",  // specific node key + type def
        CompletionItemKind::KEYWORD => "3", // alias, bool yes/no
        CompletionItemKind::ENUM_MEMBER => "4", // enum value
        CompletionItemKind::VALUE => "5",   // scope name (value side)
        CompletionItemKind::CONSTANT => "6", // variable, value set member
        CompletionItemKind::REFERENCE => "7", // type instance reference
        CompletionItemKind::FUNCTION => "8", // scope command ([GetName])
        CompletionItemKind::TEXT => "9",    // loc key, generic text
        _ => "9",
    };
    Some(format!("{}_{}", bucket, label))
}

pub(crate) fn anchor_items(items: &mut [CompletionItem], range: Range) {
    for it in items.iter_mut() {
        if it.text_edit.is_some() {
            continue;
        }
        let new_text = it.insert_text.take().unwrap_or_else(|| it.label.clone());
        if it.filter_text.is_none() {
            it.filter_text = Some(it.label.clone());
        }
        it.text_edit = Some(CompletionTextEdit::Edit(TextEdit { range, new_text }));
    }
}

pub(crate) const CONTEXT_COMPLETE_THRESHOLD: usize = 750;
pub(crate) const CONTEXT_CAP: usize = 1000;

pub(crate) fn subsequence_match(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.is_ascii() && haystack.is_ascii() {
        let needle = needle.as_bytes();
        let mut want = 0;
        for byte in haystack.as_bytes() {
            if byte.eq_ignore_ascii_case(&needle[want]) {
                want += 1;
                if want == needle.len() {
                    return true;
                }
            }
        }
        return false;
    }
    let mut needle_it = needle.chars().flat_map(char::to_lowercase).peekable();
    for c in haystack.chars().flat_map(char::to_lowercase) {
        let Some(&want) = needle_it.peek() else {
            return true;
        };
        if want == c {
            needle_it.next();
        }
    }
    needle_it.peek().is_none()
}

fn token_match_rank(hay: &str, token: &str) -> u8 {
    if hay.eq_ignore_ascii_case(token) {
        0
    } else if hay
        .get(..token.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(token))
    {
        1
    } else {
        2
    }
}

/// token can never be truncated away behind better-bucketed items (#94). An
pub(crate) fn filter_by_token(items: Vec<CompletionItem>, token: &str) -> Vec<CompletionItem> {
    let mut items = items;
    items.retain(|it| token_matches(it, token));
    sort_by_token(&mut items, token);
    items
}

fn hay(it: &CompletionItem) -> &str {
    it.filter_text.as_deref().unwrap_or(it.label.as_str())
}

pub(crate) fn token_matches(it: &CompletionItem, token: &str) -> bool {
    token.is_empty() || subsequence_match(hay(it), token)
}

pub(crate) fn sort_by_token(items: &mut [CompletionItem], token: &str) {
    items.sort_by(|a, b| {
        if !token.is_empty() {
            let ra = token_match_rank(hay(a), token);
            let rb = token_match_rank(hay(b), token);
            if ra != rb {
                return ra.cmp(&rb);
            }
        }
        let ka = a.sort_text.as_deref().unwrap_or(a.label.as_str());
        let kb = b.sort_text.as_deref().unwrap_or(b.label.as_str());
        ka.cmp(kb)
    });
}

pub(crate) fn filter_and_cap(
    items: Vec<CompletionItem>,
    token: &str,
    cap: usize,
) -> (Vec<CompletionItem>, bool) {
    let total = items.len();
    let mut filtered = filter_by_token(items, token);
    let dropped = filtered.len() < total || filtered.len() > cap;
    filtered.truncate(cap);
    (filtered, dropped)
}

pub(crate) fn prepare_context_items(
    items: Vec<CompletionItem>,
    built_dropped: usize,
    token: &str,
    ast_clean: bool,
    ast_current: bool,
    complete_threshold: usize,
    cap: usize,
) -> (Vec<CompletionItem>, bool, &'static str) {
    if built_dropped == 0 && ast_clean && ast_current && items.len() <= complete_threshold {
        return (items, false, "complete");
    }
    let (items, _) = filter_and_cap(items, token, cap);
    (items, true, "filtered")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str) -> CompletionItem {
        CompletionItem {
            label: label.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn subsequence_match_matches_in_order_non_contiguous() {
        assert!(subsequence_match("create_equipment_variant", "cev"));
        assert!(!subsequence_match("create_equipment_variant", "vec"));
    }

    #[test]
    fn subsequence_match_is_case_insensitive() {
        assert!(subsequence_match("CreateEquipment", "create"));
    }

    #[test]
    fn subsequence_match_empty_needle_matches_everything() {
        assert!(subsequence_match("anything", ""));
        assert!(subsequence_match("", ""));
    }

    #[test]
    fn filter_and_cap_empty_token_is_passthrough_but_sorted() {
        let items = vec![item("b"), item("a")];
        let (out, dropped) = filter_and_cap(items, "", 10);
        assert!(!dropped);
        assert_eq!(
            out.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn filter_and_cap_drops_non_matching_items_and_flags_truncated() {
        let items = vec![item("has_completed_focus"), item("xyz_unrelated")];
        let (out, dropped) = filter_and_cap(items, "hcf", 10);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label, "has_completed_focus");
        assert!(dropped, "dropping a non-matching item must flag truncated");
    }

    #[test]
    fn filter_and_cap_enforces_cap_and_flags_truncated() {
        let items = vec![item("aa"), item("ab"), item("ac")];
        let (out, dropped) = filter_and_cap(items, "a", 2);
        assert!(dropped);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn filter_and_cap_no_drop_no_truncate() {
        let items = vec![item("aa"), item("ab")];
        let (out, dropped) = filter_and_cap(items, "a", 10);
        assert!(!dropped);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn prepare_context_items_marks_small_current_clean_list_complete() {
        let items = vec![item("a"), item("b")];
        let (out, incomplete, strategy) = prepare_context_items(items, 0, "a", true, true, 10, 10);
        assert!(!incomplete);
        assert_eq!(strategy, "complete");
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn prepare_context_items_marks_small_stale_list_incomplete() {
        let items = vec![item("a")];
        let (_, incomplete, strategy) = prepare_context_items(items, 0, "a", true, false, 10, 10);
        assert!(incomplete);
        assert_eq!(strategy, "filtered");
    }

    #[test]
    fn prepare_context_items_marks_dirty_list_incomplete() {
        let items = vec![item("a")];
        let (_, incomplete, strategy) = prepare_context_items(items, 0, "a", false, true, 10, 10);
        assert!(incomplete);
        assert_eq!(strategy, "filtered");
    }

    #[test]
    fn prepare_context_items_filters_and_caps_large_list() {
        let items = (0..20).map(|i| item(&format!("a{i}"))).collect();
        let (out, incomplete, strategy) = prepare_context_items(items, 0, "a", true, true, 5, 3);
        assert!(incomplete);
        assert_eq!(strategy, "filtered");
        assert_eq!(out.len(), 3);
    }
}
