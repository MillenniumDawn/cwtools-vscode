use tower_lsp::lsp_types::*;

/// Build a `sortText` so the most relevant items surface first as the user
/// iterates. The LSP spec is clear the SERVER must return all valid items and
/// let the client filter by the typed prefix, so the natural label sort is
/// what the user sees if every item has the same prefix. The kind buckets
/// below keep the order useful even when a half-typed word matches many
/// items: specific leaf fields ahead of node blocks ahead of alias-driven
/// keys ahead of type instances ahead of enum values ahead of scope names
/// ahead of generic text. The bucket prefix (`0_` ... `9_`) is fixed-width
/// so a later secondary sort by label stays stable.
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

/// Stamp an explicit replace-range on every item so the client filters and
/// inserts against exactly the identifier token under the cursor. The LSP spec
/// lets the client guess the replaced word when an item carries no `textEdit`,
/// and that guess is wrong right after a backspace across a `=` / `<` / `>`:
/// the client filters the whole list against the operator (or empty string)
/// and the ranking collapses to noise — the "matching is off / irrelevant
/// context after backspace" symptom. An explicit range pins the filter input
/// to the typed text. `insert_text` (snippets) moves into `text_edit.new_text`
/// so `insert_text_format` still applies; `filter_text` is pinned to the label
/// so the client never filters against a snippet body.
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

/// A resolved-context list at or under this size is returned unfiltered with
/// `is_incomplete: false`: small enough that VS Code filters and re-filters it
/// client-side for free as the user keeps typing, with zero further requests
/// until a word boundary or trigger char forces a re-query.
pub(crate) const CONTEXT_COMPLETE_THRESHOLD: usize = 750;
/// Above the threshold, a resolved-context list is subsequence-filtered by the
/// typed token and truncated to this many items (see [`filter_and_cap`])
/// before it's marked `is_incomplete: true` — the response stays cheap to
/// serialize and the client re-queries on the next keystroke anyway.
pub(crate) const CONTEXT_CAP: usize = 1000;

/// Case-insensitive subsequence match: every character of `needle` appears in
/// `haystack` in the same order, not necessarily contiguously. This is a
/// superset of VS Code's own fuzzy matcher, so filtering by it never hides an
/// item the client would otherwise show — it only trims candidates the client
/// would filter out anyway, shrinking the payload. An empty `needle` matches
/// everything.
pub(crate) fn subsequence_match(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    // Both sides plain ASCII (every identifier and loc key in practice): case
    // folding is a byte op and the walk needs no char decoding. The haystack
    // check has to stay — a non-ASCII char can still fold to an ASCII one
    // (U+212A KELVIN SIGN lowercases to `k`), which the byte walk would miss.
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
        // Stop as soon as the needle is exhausted: the rest of the haystack
        // can't change the answer, and on a 400K-key sweep that tail is most
        // of the work.
        let Some(&want) = needle_it.peek() else {
            return true;
        };
        if want == c {
            needle_it.next();
        }
    }
    needle_it.peek().is_none()
}

/// How well `hay` matches the typed token: exact (0) ahead of prefix (1)
/// ahead of mere subsequence (2), case-insensitively.
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

/// Drop every item whose `filter_text` (or `label`) doesn't subsequence-match
/// `token`, then sort by match quality ([`token_match_rank`]) and `sort_text`
/// (falling back to `label`, same as the client would) so a later truncation
/// keeps the most relevant items — in particular an exact match for the typed
/// token can never be truncated away behind better-bucketed items (#94). An
/// empty `token` matches everything, so only the `sort_text` order applies.
pub(crate) fn filter_by_token(items: Vec<CompletionItem>, token: &str) -> Vec<CompletionItem> {
    let mut items = items;
    items.retain(|it| token_matches(it, token));
    sort_by_token(&mut items, token);
    items
}

/// The text a filter/sort compares against, matching what the client would use.
fn hay(it: &CompletionItem) -> &str {
    it.filter_text.as_deref().unwrap_or(it.label.as_str())
}

/// [`filter_by_token`]'s predicate half, so a caller holding a borrow of a
/// shared list can clone only the survivors instead of the whole list.
pub(crate) fn token_matches(it: &CompletionItem, token: &str) -> bool {
    token.is_empty() || subsequence_match(hay(it), token)
}

/// [`filter_by_token`]'s ordering half.
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

/// [`filter_by_token`] then truncate to `cap`. Returns the (possibly shrunk)
/// list plus whether anything was dropped — by the filter, the cap, or both —
/// so the caller can decide whether the result is safe to mark complete.
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
    // A list is only "complete" (client filters it locally with no further
    // requests) if the builders dropped nothing: any build-time prefiltered
    // candidate could match a different token after a backspace.
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
