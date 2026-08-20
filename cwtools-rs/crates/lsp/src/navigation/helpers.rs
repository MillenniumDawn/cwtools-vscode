use std::collections::{HashMap, HashSet};

use tower_lsp::lsp_types::*;

use cwtools_string_table::string_table::StringTable;

use crate::paths::{
    current_token_range_with_encoding, encoded_position_len, source_position_to_lsp,
};
use crate::{Backend, FileTextSnapshot};

pub(crate) async fn resolve_file_ref(
    roots: &[std::path::PathBuf],
    path: &str,
) -> Option<std::path::PathBuf> {
    let path = unquote(path).trim();
    if path.is_empty() {
        return None;
    }
    let rel = std::path::Path::new(path.trim_start_matches(['/', '\\']));
    crate::access::contained_search_path(roots, rel).await
}

/// The rename-cancelled error for a target the edit boundary refused, naming
/// the cause it reported. `-32002` is RequestFailed, the same code the
/// unresolvable-reference refusal uses, so the client shows the reason instead
/// of applying a partial rename.
pub(crate) fn rename_refused(
    uri: &str,
    refusal: crate::access::EditRefusal,
) -> tower_lsp::jsonrpc::Error {
    tower_lsp::jsonrpc::Error {
        code: tower_lsp::jsonrpc::ErrorCode::ServerError(-32002),
        message: format!(
            "Rename cancelled: '{uri}' {}; cwtools only edits files in the workspace folders.",
            refusal.reason()
        )
        .into(),
        data: None,
    }
}

/// Remove duplicate `Location` values from a goto-definition result, keeping
/// the first occurrence of each `(uri, start_line, start_char)` triple.
///
/// Identical entries arise when the same definition is reached through more than
/// one path (the type-instance index and the heuristic node-key index, say).
/// Genuinely distinct locations (different file or different position) are
/// preserved — a mod and vanilla file defining the same entity are two real
/// sites and both survive.
pub(crate) fn dedup_locations(locs: Vec<Location>) -> Vec<Location> {
    let mut seen = HashSet::new();
    locs.into_iter()
        .filter(|l| {
            seen.insert((
                l.uri.to_string(),
                l.range.start.line,
                l.range.start.character,
            ))
        })
        .collect()
}

/// Case-insensitive substring test for the `workspace/symbol` query, run over
/// every instance in the type index. `query` must already be lowercased.
/// Instance names are ASCII-dominant, so the common case is matched by
/// ASCII-folding bytes with no allocation; a name containing non-ASCII bytes
/// falls back to `to_lowercase().contains(..)` so multi-byte case folding
/// still matches (results are identical to that for every input, just not
/// allocation-free).
pub(crate) fn name_contains_ignore_case(name: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    if !name.is_ascii() {
        return name.to_lowercase().contains(query);
    }
    let (n, q) = (name.as_bytes(), query.as_bytes());
    q.len() <= n.len() && n.windows(q.len()).any(|w| w.eq_ignore_ascii_case(q))
}

/// One `workspace/symbol` match before range conversion: where it lives
/// (0-based line, char col) plus how it sorts (`rank`, then name, then uri,
/// then position — the `Ord` impl below).
pub(crate) struct SymbolCandidate {
    pub(crate) rank: u8,
    pub(crate) name: String,
    pub(crate) container: Option<String>,
    pub(crate) kind: SymbolKind,
    pub(crate) file_uri: String,
    pub(crate) line0: u32,
    pub(crate) col: u32,
}

impl SymbolCandidate {
    fn sort_key(&self) -> (u8, &str, &str, u32, u32) {
        (self.rank, &self.name, &self.file_uri, self.line0, self.col)
    }
}

impl Ord for SymbolCandidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

impl PartialOrd for SymbolCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for SymbolCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.sort_key() == other.sort_key()
    }
}

impl Eq for SymbolCandidate {}

/// Response cap for `workspace/symbol`, matching what symbol pickers show.
pub(crate) const WORKSPACE_SYMBOL_LIMIT: usize = 500;

/// Bounded top-k accumulator for `workspace/symbol`: a max-heap of at most
/// `limit` candidates whose root is the worst one kept. [`accepts`](Self::accepts)
/// compares an incoming candidate's borrowed sort key against that root, so
/// callers only clone name/uri strings for candidates that make the cut.
pub(crate) struct TopSymbols {
    limit: usize,
    heap: std::collections::BinaryHeap<SymbolCandidate>,
}

impl TopSymbols {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            heap: std::collections::BinaryHeap::with_capacity(limit + 1),
        }
    }

    /// Whether a candidate with this sort key would be kept. Equal-to-worst is
    /// rejected: it could only displace an identically-ordered entry.
    pub(crate) fn accepts(
        &self,
        rank: u8,
        name: &str,
        file_uri: &str,
        line0: u32,
        col: u32,
    ) -> bool {
        if self.heap.len() < self.limit {
            return true;
        }
        let Some(worst) = self.heap.peek() else {
            return true;
        };
        (rank, name, file_uri, line0, col) < worst.sort_key()
    }

    pub(crate) fn push(&mut self, cand: SymbolCandidate) {
        self.heap.push(cand);
        if self.heap.len() > self.limit {
            self.heap.pop();
        }
    }

    /// The kept candidates, best first.
    pub(crate) fn into_sorted_vec(self) -> Vec<SymbolCandidate> {
        self.heap.into_sorted_vec()
    }
}

/// Rank of a workspace-symbol candidate against the (already lowercased)
/// query: 0 exact, 1 prefix, 2 substring, `None` when it doesn't match. The
/// empty query admits everything (the picker's initial, unfiltered list).
/// ASCII names (the dominant case) rank with no allocation; non-ASCII names
/// take the same `to_lowercase` fallback as `name_contains_ignore_case`.
pub(crate) fn symbol_rank(name: &str, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(2);
    }
    if !name_contains_ignore_case(name, query) {
        return None;
    }
    if name.is_ascii() {
        let (n, q) = (name.as_bytes(), query.as_bytes());
        return if n.eq_ignore_ascii_case(q) {
            Some(0)
        } else if q.len() <= n.len() && n[..q.len()].eq_ignore_ascii_case(q) {
            Some(1)
        } else {
            Some(2)
        };
    }
    let lower = name.to_lowercase();
    if lower == query {
        Some(0)
    } else if lower.starts_with(query) {
        Some(1)
    } else {
        Some(2)
    }
}

/// Whether `c` continues an identifier token (bare id charset plus `.` for
/// dotted event ids). Used to word-bound the token searches below.
pub(crate) fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '.'
}

/// Every 0-based char column where `name` appears on `line` as a whole
/// identifier (bounded by non-identifier chars), ignoring anything behind an
/// unquoted `#` comment. Quoted occurrences still match (values may be
/// quoted). Char-based to match the parser's column counting.
pub(crate) fn code_token_cols_in_line(line: &str, name: &str) -> Vec<u32> {
    let chars: Vec<char> = line.chars().collect();
    let needle: Vec<char> = name.chars().collect();
    let mut out = Vec::new();
    if needle.is_empty() || needle.len() > chars.len() {
        return out;
    }
    let mut in_string = false;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '"' => in_string = !in_string,
            '#' if !in_string => break,
            _ => {}
        }
        if i + needle.len() <= chars.len() && chars[i..i + needle.len()] == needle[..] {
            let before_ok = i == 0 || !is_ident_char(chars[i - 1]);
            let after = i + needle.len();
            let after_ok = after >= chars.len() || !is_ident_char(chars[after]);
            if before_ok && after_ok {
                out.push(i as u32);
            }
        }
        i += 1;
    }
    out
}

/// WRITE when the token at `col` is an assignment key (the next non-space char
/// after it is `=`), READ otherwise. Advisory: clients only use this to tint
/// the highlight.
pub(crate) fn highlight_kind(line: &str, col: u32, name: &str) -> DocumentHighlightKind {
    let after = col as usize + name.chars().count();
    match line.chars().skip(after).find(|c| !c.is_whitespace()) {
        Some('=') => DocumentHighlightKind::WRITE,
        _ => DocumentHighlightKind::READ,
    }
}

/// The 0-based char column just past the assignment `=` at/after `key_col` on
/// `line`. Quoted keys may contain an equals sign, so only an equals outside a
/// quoted string can be the assignment operator.
pub(crate) fn value_start_after_eq(line: &str, key_col: u32) -> Option<u32> {
    let mut quoted = false;
    let mut backslashes = 0;
    line.chars()
        .enumerate()
        .skip(key_col as usize)
        .find(|(_, c)| {
            let escaped = backslashes % 2 == 1;
            if *c == '"' && !escaped {
                quoted = !quoted;
            }
            let is_assignment = *c == '=' && !quoted;
            if *c == '\\' {
                backslashes += 1;
            } else {
                backslashes = 0;
            }
            is_assignment
        })
        .map(|(i, _)| i as u32 + 1)
}

/// The 0-based char column of the value token `name` on `line`, scanning only
/// the region at/after char column `from` and stopping at an unquoted `#`
/// comment. Takes the FIRST whole-token match so a repeat of the name inside a
/// trailing comment (`x = MY_FOCUS # keep MY_FOCUS`) or a second `key = value`
/// pair later on the line can't be mistaken for the value. Quoted values
/// (`"MY_FOCUS"`) match the inner token. `None` when `name` doesn't occur here.
pub(crate) fn value_col_in_line(line: &str, name: &str, from: u32) -> Option<u32> {
    let chars: Vec<char> = line.chars().collect();
    let needle: Vec<char> = name.chars().collect();
    if needle.is_empty() {
        return None;
    }
    let mut in_string = false;
    let mut i = from as usize;
    while i + needle.len() <= chars.len() {
        match chars[i] {
            '"' => in_string = !in_string,
            '#' if !in_string => break,
            _ => {}
        }
        if chars[i..i + needle.len()] == needle[..] {
            let before_ok = i == 0 || !is_ident_char(chars[i - 1]);
            let after = i + needle.len();
            let after_ok = after >= chars.len() || !is_ident_char(chars[after]);
            if before_ok && after_ok {
                return Some(i as u32);
            }
        }
        i += 1;
    }
    None
}

/// Classify the `.cwt` construct at char `col` on `line`: a `<type>` /
/// `<!type>` / `<type.subtype>` reference, `enum[..]` / `complex_enum[..]`,
/// or `single_alias_right[..]`. Alias categories and value sets are out —
/// they have no single definition site (consistent with the structural lint).
pub(crate) fn cwt_ref_at(
    line: &str,
    col: u32,
) -> Option<(cwtools_rules::rules_types::CwtDefKind, String)> {
    use cwtools_rules::rules_types::CwtDefKind;
    let chars: Vec<char> = line.chars().collect();
    let col = col as usize;
    // `<...>` spans (angle brackets included in the hit area).
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<'
            && let Some(close) = chars[i + 1..]
                .iter()
                .position(|&c| c == '>')
                .map(|p| p + i + 1)
        {
            if (i..=close).contains(&col) {
                let inner: String = chars[i + 1..close].iter().collect();
                let name = inner.trim_start_matches('!');
                // `<type.subtype>` is defined by its base type.
                let base = name.split('.').next().unwrap_or(name);
                return (!base.is_empty()).then(|| (CwtDefKind::Type, base.to_string()));
            }
            i = close + 1;
            continue;
        }
        i += 1;
    }
    for (prefix, kind) in [
        ("complex_enum", CwtDefKind::Enum),
        ("enum", CwtDefKind::Enum),
        ("single_alias_right", CwtDefKind::SingleAlias),
    ] {
        if let Some(name) = bracket_ref_at(&chars, col, prefix) {
            return Some((kind, name));
        }
    }
    None
}

/// The bracketed name of a `prefix[NAME]` occurrence whose span covers `col`,
/// word-bounded so `enum[` inside `complex_enum[` doesn't match.
pub(crate) fn bracket_ref_at(chars: &[char], col: usize, prefix: &str) -> Option<String> {
    let p: Vec<char> = prefix.chars().collect();
    let mut i = 0;
    while i + p.len() < chars.len() {
        if chars[i..i + p.len()] == p[..]
            && chars.get(i + p.len()) == Some(&'[')
            && (i == 0 || !is_ident_char(chars[i - 1]))
            && let Some(close) = chars[i + p.len() + 1..]
                .iter()
                .position(|&c| c == ']')
                .map(|q| q + i + p.len() + 1)
        {
            if (i..=close).contains(&col) {
                let name: String = chars[i + p.len() + 1..close].iter().collect();
                return (!name.is_empty()).then_some(name);
            }
            i = close + 1;
            continue;
        }
        i += 1;
    }
    None
}

/// The `@name` script-constant token at (line0, col): the full token including
/// the sigil, and its 0-based start char col. `None` when the cursor isn't on
/// one.
pub(crate) fn at_var_at_cursor(text: &str, line0: u32, col: u32) -> Option<(String, u32)> {
    let line = text.lines().nth(line0 as usize)?;
    let chars: Vec<char> = line.chars().collect();
    let mut cur = (col as usize).min(chars.len());
    // Cursor on the sigil itself: step into the name.
    if cur < chars.len() && chars[cur] == '@' {
        cur += 1;
    }
    let mut start = cur;
    while start > 0 && is_ident_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = cur;
    while end < chars.len() && is_ident_char(chars[end]) {
        end += 1;
    }
    if start == end || start == 0 || chars[start - 1] != '@' {
        return None;
    }
    let name: String = std::iter::once('@')
        .chain(chars[start..end].iter().copied())
        .collect();
    Some((name, start as u32 - 1))
}

/// The identifier token the cursor sits in (extended both directions over the
/// identifier charset). `None` when the cursor isn't on an identifier.
pub(crate) fn word_at_position(text: &str, line0: u32, char0: u32) -> Option<String> {
    let line = text.lines().nth(line0 as usize)?;
    let chars: Vec<char> = line.chars().collect();
    let cur = (char0 as usize).min(chars.len());
    let mut start = cur;
    while start > 0 && is_ident_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = cur;
    while end < chars.len() && is_ident_char(chars[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    Some(chars[start..end].iter().collect())
}

/// Region folding ranges for every multi-line `{ … }` block, from a brace-match
/// scan of the text (comments and quoted strings ignored). More accurate than
/// the AST for the closing-brace line, which the parser doesn't retain.
pub(crate) fn brace_folding_ranges(text: &str) -> Vec<FoldingRange> {
    let mut ranges = Vec::new();
    let mut stack: Vec<u32> = Vec::new();
    let mut line: u32 = 0;
    let mut in_string = false;
    let mut in_comment = false;
    for c in text.chars() {
        if c == '\n' {
            line += 1;
            in_comment = false;
            // Quoted strings never span lines in this grammar.
            in_string = false;
            continue;
        }
        if c == '\r' || in_comment {
            continue;
        }
        if in_string {
            if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '#' => in_comment = true,
            '"' => in_string = true,
            '{' => stack.push(line),
            '}' => {
                if let Some(start) = stack.pop()
                    && line > start
                {
                    ranges.push(FoldingRange {
                        start_line: start,
                        start_character: None,
                        end_line: line,
                        end_character: None,
                        kind: Some(FoldingRangeKind::Region),
                        collapsed_text: None,
                    });
                }
            }
            _ => {}
        }
    }
    ranges
}

/// A start/end span in source char coordinates: ((line, col), (line, col)),
/// end-exclusive.
pub(crate) type CharSpan = ((u32, u32), (u32, u32));

/// Every matched `{ … }` pair in `text` as ((open_line, open_col),
/// (close_line, close_col)) char positions of the braces themselves, from the
/// same comment- and string-aware scan folding uses.
pub(crate) fn brace_pairs(text: &str) -> Vec<CharSpan> {
    let mut pairs = Vec::new();
    let mut stack: Vec<(u32, u32)> = Vec::new();
    let (mut line, mut col): (u32, u32) = (0, 0);
    let mut in_string = false;
    let mut in_comment = false;
    for c in text.chars() {
        if c == '\n' {
            line += 1;
            col = 0;
            in_comment = false;
            // Quoted strings never span lines in this grammar.
            in_string = false;
            continue;
        }
        let here = (line, col);
        col += 1;
        if c == '\r' || in_comment {
            continue;
        }
        if in_string {
            if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '#' => in_comment = true,
            '"' => in_string = true,
            '{' => stack.push(here),
            '}' => {
                if let Some(open) = stack.pop() {
                    pairs.push((open, here));
                }
            }
            _ => {}
        }
    }
    pairs
}

/// The position of `member` inside the `{ … }` block a definition at
/// (`def_line0`, `def_col`) opens — the first whole-token, non-comment
/// occurrence between its braces. `None` when the definition opens no block or
/// doesn't list `member`.
pub(crate) fn member_pos_in_block(
    text: &str,
    def_line0: u32,
    def_col: u32,
    member: &str,
) -> Option<(u32, u32)> {
    let (open, close) = brace_pairs(text)
        .into_iter()
        .filter(|(open, _)| *open >= (def_line0, def_col))
        .min()?;
    let lines: Vec<&str> = text.lines().collect();
    (open.0..=close.0).find_map(|line0| {
        let line = lines.get(line0 as usize)?;
        code_token_cols_in_line(line, member)
            .into_iter()
            .find(|col| (line0 != open.0 || *col > open.1) && (line0 != close.0 || *col < close.1))
            .map(|col| (line0, col))
    })
}

/// The innermost-first selection chain at (line0, col): the identifier token
/// under the cursor, then for each enclosing brace pair its content span
/// (inside the braces) followed by the full span (including them). Every span
/// contains the previous one, as `textDocument/selectionRange` requires.
pub(crate) fn selection_spans(
    text: &str,
    pairs: &[CharSpan],
    line0: u32,
    col: u32,
) -> Vec<CharSpan> {
    let mut spans: Vec<CharSpan> = Vec::new();
    if let Some(line) = text.lines().nth(line0 as usize) {
        let chars: Vec<char> = line.chars().collect();
        let cur = (col as usize).min(chars.len());
        let mut start = cur;
        while start > 0 && is_ident_char(chars[start - 1]) {
            start -= 1;
        }
        let mut end = cur;
        while end < chars.len() && is_ident_char(chars[end]) {
            end += 1;
        }
        if start < end {
            spans.push(((line0, start as u32), (line0, end as u32)));
        }
    }
    let pos = (line0, col);
    let mut enclosing: Vec<&CharSpan> = pairs
        .iter()
        .filter(|(open, close)| *open <= pos && pos <= *close)
        .collect();
    // Innermost first: the latest-opening enclosing pair is the tightest.
    enclosing.sort_by_key(|p| std::cmp::Reverse(p.0));
    for &&((ol, oc), (cl, cc)) in &enclosing {
        let inner = ((ol, oc + 1), (cl, cc));
        if inner.0 < inner.1 {
            spans.push(inner);
        }
        spans.push(((ol, oc), (cl, cc + 1)));
    }
    spans
}

/// Folding ranges the brace scan can't produce: runs of two or more full-line
/// `#` comments fold as `Comment`, and `#region` / `#endregion` marker pairs
/// (stack-matched, so they nest) fold as `Region`. Marker lines belong to
/// their region fold and never count toward a comment run; unmatched markers
/// are ignored.
pub(crate) fn comment_and_region_folds(text: &str) -> Vec<FoldingRange> {
    let mut folds = Vec::new();
    let mut region_stack: Vec<u32> = Vec::new();
    let mut run_start: Option<u32> = None;
    let mut prev_line: u32 = 0;
    let close_run = |start: Option<u32>, end_line: u32, folds: &mut Vec<FoldingRange>| {
        if let Some(start) = start
            && end_line > start
        {
            folds.push(FoldingRange {
                start_line: start,
                start_character: None,
                end_line,
                end_character: None,
                kind: Some(FoldingRangeKind::Comment),
                collapsed_text: None,
            });
        }
    };
    for (line0, line) in text.lines().enumerate() {
        let line0 = line0 as u32;
        prev_line = line0;
        let trimmed = line.trim_start();
        let is_marker_start = region_marker(trimmed) == Some(true);
        let is_marker_end = region_marker(trimmed) == Some(false);
        if is_marker_start || is_marker_end || !trimmed.starts_with('#') {
            close_run(run_start.take(), line0.saturating_sub(1), &mut folds);
        } else if run_start.is_none() {
            run_start = Some(line0);
        }
        if is_marker_start {
            region_stack.push(line0);
        } else if is_marker_end
            && let Some(start) = region_stack.pop()
            && line0 > start
        {
            folds.push(FoldingRange {
                start_line: start,
                start_character: None,
                end_line: line0,
                end_character: None,
                kind: Some(FoldingRangeKind::Region),
                collapsed_text: None,
            });
        }
    }
    close_run(run_start.take(), prev_line, &mut folds);
    folds
}

/// `Some(true)` for a `#region` marker, `Some(false)` for `#endregion`, `None`
/// for anything else. `line` must already be left-trimmed. A marker may carry
/// a trailing label (`#region Alpha`).
pub(crate) fn region_marker(line: &str) -> Option<bool> {
    if let Some(rest) = line.strip_prefix("#endregion") {
        (rest.is_empty() || rest.starts_with(char::is_whitespace)).then_some(false)
    } else if let Some(rest) = line.strip_prefix("#region") {
        (rest.is_empty() || rest.starts_with(char::is_whitespace)).then_some(true)
    } else {
        None
    }
}

/// The identity value of a block (`id` / `name` / `tag` child leaf, in that
/// priority), used to give repeated block keys (`focus`, `country_event`, …)
/// distinct outline names. `None` when the block has no such leaf.
pub(crate) fn identity_value(
    children: &[cwtools_parser::ast::Child],
    arena: &cwtools_parser::ast::Arena,
    table: &StringTable,
) -> Option<String> {
    use cwtools_parser::ast::{Child, Value};
    for want in ["id", "name", "tag"] {
        for child in children {
            let Child::Leaf(idx) = child else { continue };
            let leaf = &arena.leaves[*idx as usize];
            let key = table.get_string(leaf.key.normal).unwrap_or_default();
            if key.eq_ignore_ascii_case(want)
                && let Value::String(t) | Value::QString(t) = &leaf.value
                && let Some(raw) = table.get_string(t.normal)
            {
                let v = raw
                    .strip_prefix('"')
                    .and_then(|x| x.strip_suffix('"'))
                    .unwrap_or(&raw);
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Build a nested `DocumentSymbol` tree from AST children: every keyed clause
/// becomes a STRUCT symbol (named by its identity leaf when present, else its
/// key) whose children are the nested clauses. `range` is the block span,
/// `selection_range` the key token (⊆ range, as LSP requires). Sibling ranges
/// are clamped so the parser's trailing-whitespace overshoot can't nest them.
pub(crate) fn build_doc_symbols(
    children: &[cwtools_parser::ast::Child],
    arena: &cwtools_parser::ast::Arena,
    table: &StringTable,
    text: &str,
    encoding: &PositionEncodingKind,
) -> Vec<DocumentSymbol> {
    let mut syms: Vec<DocumentSymbol> = Vec::new();
    for child in children {
        let Some(kc) = arena.keyed_clause(child) else {
            continue;
        };
        let key = table.get_string(kc.key.normal).unwrap_or_default();
        if key.is_empty() {
            continue;
        }
        let child_syms = build_doc_symbols(kc.children, arena, table, text, encoding);
        let (name, detail) = match identity_value(kc.children, arena, table) {
            Some(v) if v != key => (v, Some(key.clone())),
            _ => (key.clone(), None),
        };
        let start = source_position_to_lsp(
            text,
            kc.pos.start.line.saturating_sub(1),
            kc.pos.start.col as u32,
            encoding,
        );
        let end = source_position_to_lsp(
            text,
            kc.pos.end.line.saturating_sub(1),
            kc.pos.end.col as u32,
            encoding,
        );
        let selection_end = source_range_in_text(
            text,
            kc.pos.start.line.saturating_sub(1),
            kc.pos.start.col as u32,
            &key,
            encoding,
        )
        .end;
        #[allow(deprecated)]
        syms.push(DocumentSymbol {
            name,
            detail,
            kind: SymbolKind::STRUCT,
            tags: None,
            deprecated: None,
            range: Range { start, end },
            selection_range: Range {
                start,
                end: selection_end,
            },
            children: (!child_syms.is_empty()).then_some(child_syms),
        });
    }
    // Clamp each range end to the next sibling's start so the overshoot past
    // `}` (the parser consumes trailing whitespace) can't swallow a sibling.
    for i in 0..syms.len().saturating_sub(1) {
        let next_start = syms[i + 1].range.start;
        let cur_end = syms[i].range.end;
        if (next_start.line, next_start.character) < (cur_end.line, cur_end.character) {
            syms[i].range.end = next_start;
            let sel_end = syms[i].selection_range.end;
            if (sel_end.line, sel_end.character) > (next_start.line, next_start.character) {
                syms[i].selection_range.end = next_start;
            }
        }
    }
    syms
}

/// Strip matching outer double quotes from a token. Quoted string values keep
/// their quotes through the parser/string-table, but indexed instance names and
/// loc keys are unquoted, so references must be unquoted before comparison.
pub(crate) fn unquote(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .unwrap_or(s)
}

/// Build Locations from `(file_uri, location)` pairs, each highlighting a token
/// of `name`'s length. Text is fetched in one batch before the pure conversion.
pub(crate) async fn locations_at(
    backend: &Backend,
    pairs: impl IntoIterator<Item = (String, cwtools_info::SourceLocation)>,
    name: &str,
    fallback: &Url,
) -> Vec<Location> {
    let pairs: Vec<_> = pairs.into_iter().collect();
    let uris: Vec<String> = pairs.iter().map(|(file_uri, _)| file_uri.clone()).collect();
    let texts = backend.file_text_snapshots_for(&uris).await;
    locations_at_with_texts(backend, pairs, name, fallback, &texts)
}

pub(crate) fn locations_at_with_texts(
    backend: &Backend,
    pairs: impl IntoIterator<Item = (String, cwtools_info::SourceLocation)>,
    name: &str,
    fallback: &Url,
    texts: &HashMap<String, FileTextSnapshot>,
) -> Vec<Location> {
    pairs
        .into_iter()
        .map(|(file_uri, loc)| {
            backend.source_location_with_text(
                &file_uri,
                loc.line.saturating_sub(1),
                loc.col as u32,
                name,
                fallback,
                texts.get(&file_uri).map(|snapshot| snapshot.text.as_str()),
            )
        })
        .collect()
}

pub(crate) fn prepare_rename_range(
    text: Option<&str>,
    pos: Position,
    instance_name: &str,
    encoding: &PositionEncodingKind,
) -> Range {
    let start = text.map_or(pos, |text| {
        current_token_range_with_encoding(text, pos.line, pos.character, encoding).start
    });
    Range {
        start,
        end: Position {
            line: start.line,
            character: start.character + encoded_position_len(instance_name, encoding),
        },
    }
}

pub(crate) fn source_range_in_text(
    text: &str,
    line: u32,
    column: u32,
    token: &str,
    encoding: &PositionEncodingKind,
) -> Range {
    Range {
        start: source_position_to_lsp(text, line, column, encoding),
        end: source_position_to_lsp(text, line, column + token.chars().count() as u32, encoding),
    }
}

pub(crate) fn source_range_without_text(
    line: u32,
    column: u32,
    token: &str,
    encoding: &PositionEncodingKind,
) -> Range {
    let start = Position::new(line, column);
    Range::new(
        start,
        Position::new(line, column + encoded_position_len(token, encoding)),
    )
}

/// Every 0-based char column where `needle_lower` appears on `line` as a whole
/// identifier (bounded by non-identifier chars), ignoring anything behind an
/// unquoted `#` comment. `needle_lower` must already be lowercased. Case
/// insensitive: `MY_KEY` matches `my_key`. Used for loc keys which are stored
/// lowercased.
pub(crate) fn code_token_cols_in_line_ignore_case(line: &str, needle_lower: &str) -> Vec<u32> {
    let needle: Vec<char> = needle_lower.chars().collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = line.chars().collect();
    if needle.len() > chars.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut in_string = false;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '"' => in_string = !in_string,
            '#' if !in_string => break,
            _ => {}
        }
        if i + needle.len() <= chars.len() {
            let slice = &chars[i..i + needle.len()];
            let matches = slice
                .iter()
                .zip(needle.iter())
                .all(|(a, b)| a.to_ascii_lowercase() == *b);
            if matches {
                let before_ok = i == 0 || !is_ident_char(chars[i - 1]);
                let after = i + needle.len();
                let after_ok = after >= chars.len() || !is_ident_char(chars[after]);
                if before_ok && after_ok {
                    out.push(i as u32);
                }
            }
        }
        i += 1;
    }
    out
}

/// Every 0-based char column where `$needle_lower$` appears as a loc ref in
/// `line` (including `|colour` suffix, e.g. `$MY_KEY|Y$`). Returns the column
/// of the inner key's first character (after the opening `$`), case-insensitive.
/// Used for yml `$REF$` rename. Skips `$` refs inside unquoted `#` comments
/// and handles currency `$` like "$5 for $ITEM$" by advancing one dollar on
/// invalid ident.
pub(crate) fn loc_ref_key_cols_in_line(line: &str, needle_lower: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    // Collect `$` positions, stopping at an unquoted `#` comment.
    let mut dollars: Vec<usize> = Vec::new();
    let mut in_string = false;
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '"' => in_string = !in_string,
            '#' if !in_string => break,
            '$' => dollars.push(i),
            _ => {}
        }
    }
    let mut i = 0;
    while i + 1 < dollars.len() {
        let open = dollars[i];
        let close = dollars[i + 1];
        if close <= open + 1 {
            i += 1;
            continue;
        }
        let inner: String = chars[open + 1..close].iter().collect();
        let key = inner.split('|').next().unwrap_or(&inner);
        let is_valid = !key.is_empty()
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
        if !is_valid {
            // Currency or malformed like "$5 for $" — skip only the opening `$"
            i += 1;
            continue;
        }
        if key.to_ascii_lowercase() == needle_lower {
            out.push((open + 1) as u32);
        }
        i += 2;
    }
    out
}

/// Strip any `_desc` / `_tooltip` suffixes (repeatedly) to get the family root.
/// `"my_thing_desc_tooltip"` -> `"my_thing"`.
pub(crate) fn loc_root(key_lower: &str) -> String {
    let mut k = key_lower;
    loop {
        if let Some(stripped) = k.strip_suffix("_desc") {
            k = stripped;
            continue;
        }
        if let Some(stripped) = k.strip_suffix("_tooltip") {
            k = stripped;
            continue;
        }
        break;
    }
    k.to_string()
}

/// Build a `SymbolInformation` (the `deprecated` field is required by the
/// struct but deprecated by the protocol).
pub(crate) fn make_symbol(
    name: String,
    kind: SymbolKind,
    location: Location,
    container_name: Option<String>,
) -> SymbolInformation {
    #[allow(deprecated)]
    SymbolInformation {
        name,
        kind,
        tags: None,
        deprecated: None,
        location,
        container_name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_location(uri: &str, line: u32, ch: u32) -> Location {
        Location {
            uri: uri.parse().unwrap(),
            range: Range {
                start: Position {
                    line,
                    character: ch,
                },
                end: Position {
                    line,
                    character: ch + 5,
                },
            },
        }
    }

    /// The goto side of #176: the `FilepathField` value is mod content, so one
    /// climbing out of the search roots resolves to nothing rather than
    /// reporting whether the file it names exists.
    #[tokio::test]
    async fn a_file_ref_that_climbs_out_of_the_roots_resolves_to_nothing() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let root = tmp.path().join("mod");
        std::fs::create_dir_all(root.join("gfx")).unwrap();
        std::fs::write(root.join("gfx/pic.dds"), "x").unwrap();
        std::fs::write(tmp.path().join("secret.dds"), "secret").unwrap();
        let roots = vec![root.clone()];

        assert_eq!(resolve_file_ref(&roots, "../secret.dds").await, None);
        assert_eq!(
            resolve_file_ref(&roots, "\"/gfx/../../secret.dds\"").await,
            None
        );
        assert_eq!(
            resolve_file_ref(&roots, "gfx/pic.dds").await,
            Some(root.join("gfx/pic.dds"))
        );
    }

    #[test]
    fn code_token_cols_skip_comment_matches() {
        assert_eq!(
            code_token_cols_in_line("x = FOO # FOO again", "FOO"),
            vec![4]
        );
        assert_eq!(
            code_token_cols_in_line("# only FOO", "FOO"),
            Vec::<u32>::new()
        );
        assert_eq!(code_token_cols_in_line("x = \"FOO\" # FOO", "FOO"), vec![5]);
        // A `#` inside a quoted string does not start a comment.
        assert_eq!(code_token_cols_in_line("x = \"# FOO\"", "FOO"), vec![7]);
        assert_eq!(code_token_cols_in_line("FOO = FOO", "FOO"), vec![0, 6]);
    }

    #[test]
    fn highlight_kind_write_for_assignment_key_read_otherwise() {
        assert_eq!(
            highlight_kind("MY_FOCUS = { }", 0, "MY_FOCUS"),
            DocumentHighlightKind::WRITE
        );
        assert_eq!(
            highlight_kind("    has_focus = MY_FOCUS", 16, "MY_FOCUS"),
            DocumentHighlightKind::READ
        );
        assert_eq!(
            highlight_kind("    var >= MY_FOCUS", 11, "MY_FOCUS"),
            DocumentHighlightKind::READ
        );
    }

    #[test]
    fn value_start_after_eq_ignores_quoted_key_equals() {
        assert_eq!(value_start_after_eq("\"a=b\" = { }", 0), Some(7));
        assert_eq!(value_start_after_eq("\"a\\\"=b\" = { }", 0), Some(9));
        assert_eq!(value_start_after_eq("\"a\\\\\"=b\" = { }", 0), Some(6));
    }

    #[test]
    fn name_contains_ignore_case_matches_ascii_case_insensitively() {
        assert!(name_contains_ignore_case("Ship_Hull_Submarine", "hull_sub"));
        assert!(name_contains_ignore_case("Ship_Hull_Submarine", ""));
        assert!(!name_contains_ignore_case("Ship_Hull_Submarine", "cruiser"));
        assert!(!name_contains_ignore_case("abc", "abcd"));
    }

    #[test]
    fn comment_folds_need_two_consecutive_lines() {
        let folds = comment_and_region_folds("# one\n# two\n# three\nx = 1\n# lone\n");
        assert_eq!(folds.len(), 1);
        assert_eq!((folds[0].start_line, folds[0].end_line), (0, 2));
        assert_eq!(folds[0].kind, Some(FoldingRangeKind::Comment));
    }

    #[test]
    fn comment_fold_at_eof_without_newline() {
        let folds = comment_and_region_folds("x = 1\n# a\n# b");
        assert_eq!(folds.len(), 1);
        assert_eq!((folds[0].start_line, folds[0].end_line), (1, 2));
    }

    #[test]
    fn region_markers_fold_and_nest() {
        let text = "#region outer\na = 1\n#region inner\nb = 2\n#endregion\n#endregion\n";
        let folds = comment_and_region_folds(text);
        let regions: Vec<(u32, u32)> = folds
            .iter()
            .filter(|f| f.kind == Some(FoldingRangeKind::Region))
            .map(|f| (f.start_line, f.end_line))
            .collect();
        assert!(regions.contains(&(0, 5)), "outer region, got {:?}", regions);
        assert!(regions.contains(&(2, 4)), "inner region, got {:?}", regions);
    }

    #[test]
    fn unmatched_region_markers_are_ignored() {
        assert!(comment_and_region_folds("#endregion\nx = 1\n").is_empty());
        assert!(comment_and_region_folds("#region only\nx = 1\n").is_empty());
    }

    #[test]
    fn region_markers_break_comment_runs() {
        // The marker line belongs to its region fold, not to a comment run, so
        // a lone comment next to a marker doesn't fold as a comment block.
        let text = "# note\n#region r\nx = 1\n#endregion\n";
        let folds = comment_and_region_folds(text);
        assert!(
            folds
                .iter()
                .all(|f| f.kind != Some(FoldingRangeKind::Comment)),
            "got {:?}",
            folds
        );
    }

    #[test]
    fn selection_spans_token_then_inner_then_full_pair() {
        let text = "a = {\n    foo = bar\n}\n";
        let pairs = brace_pairs(text);
        // Cursor inside `bar` (line 1, col 10).
        let spans = selection_spans(text, &pairs, 1, 10);
        assert_eq!(
            spans,
            vec![
                ((1, 10), (1, 13)), // the token
                ((0, 5), (2, 0)),   // inside the braces
                ((0, 4), (2, 1)),   // including the braces
            ]
        );
    }

    #[test]
    fn selection_spans_nested_pairs_chain_outward() {
        let text = "a = {\n    b = {\n        x = 1\n    }\n}\n";
        let pairs = brace_pairs(text);
        let spans = selection_spans(text, &pairs, 2, 8);
        assert_eq!(
            spans,
            vec![
                ((2, 8), (2, 9)), // `x`
                ((1, 9), (3, 4)), // inside inner braces
                ((1, 8), (3, 5)), // inner pair
                ((0, 5), (4, 0)), // inside outer braces
                ((0, 4), (4, 1)), // outer pair
            ]
        );
    }

    #[test]
    fn brace_pairs_ignore_comments_and_strings() {
        assert!(brace_pairs("# { not a block\n").is_empty());
        assert!(brace_pairs("x = \"{\"\n").is_empty());
    }

    #[test]
    fn selection_spans_on_whitespace_start_at_block() {
        let text = "a = {\n    foo = bar\n}\n";
        let pairs = brace_pairs(text);
        // Cursor on the indent whitespace: no token, chain starts at the block.
        let spans = selection_spans(text, &pairs, 1, 2);
        assert_eq!(spans, vec![((0, 5), (2, 0)), ((0, 4), (2, 1))]);
    }

    #[test]
    fn member_pos_in_block_finds_enum_value() {
        // One value per line, the shape a large enum is written in.
        let text = "enums = {\n    enum[terrain] = {\n        plains\n        forest\n    }\n}\n";
        assert_eq!(member_pos_in_block(text, 1, 4, "forest"), Some((3, 8)));
        // A single-line body: the value sits on the defining line itself.
        let text = "enums = {\n    enum[terrain] = { plains forest }\n}\n";
        assert_eq!(member_pos_in_block(text, 1, 4, "forest"), Some((1, 29)));
    }

    #[test]
    fn member_pos_in_block_skips_what_isnt_a_member() {
        // A complex enum's members come from the game files, not the block.
        let text =
            "enums = {\n    complex_enum[tags] = {\n        path = \"game/common\"\n    }\n}\n";
        assert_eq!(member_pos_in_block(text, 1, 4, "USA"), None);
        // A commented-out value is not a member, and neither is a substring of one.
        let text =
            "enums = {\n    enum[terrain] = {\n        # forest\n        forestry\n    }\n}\n";
        assert_eq!(member_pos_in_block(text, 1, 4, "forest"), None);
    }

    #[test]
    fn cwt_ref_at_classifies_rule_references() {
        use cwtools_rules::rules_types::CwtDefKind;
        // `<focus>` anywhere in the span, including the angle brackets.
        let line = "    has_focus = <focus>";
        assert_eq!(
            cwt_ref_at(line, 18),
            Some((CwtDefKind::Type, "focus".to_string()))
        );
        assert_eq!(
            cwt_ref_at(line, 16),
            Some((CwtDefKind::Type, "focus".to_string()))
        );
        // `<!focus>` negation still names the type.
        assert_eq!(
            cwt_ref_at("    a = <!focus>", 12),
            Some((CwtDefKind::Type, "focus".to_string()))
        );
        assert_eq!(
            cwt_ref_at("    stat = enum[stat]", 17),
            Some((CwtDefKind::Enum, "stat".to_string()))
        );
        assert_eq!(
            cwt_ref_at("    b = single_alias_right[block]", 29),
            Some((CwtDefKind::SingleAlias, "block".to_string()))
        );
        // Alias categories and value sets are out of scope.
        assert_eq!(cwt_ref_at("    alias_name[effect] = x", 12), None);
        assert_eq!(cwt_ref_at("    v = value[my_set]", 16), None);
        // Cursor outside any construct.
        assert_eq!(cwt_ref_at("    has_focus = <focus>", 6), None);
    }

    #[test]
    fn at_var_at_cursor_finds_sigil_token() {
        let text = "y = @foo\n";
        assert_eq!(at_var_at_cursor(text, 0, 6), Some(("@foo".to_string(), 4)));
        assert_eq!(at_var_at_cursor(text, 0, 4), Some(("@foo".to_string(), 4)));
        // End-of-token cursor still resolves.
        assert_eq!(at_var_at_cursor(text, 0, 8), Some(("@foo".to_string(), 4)));
        // A plain identifier has no sigil.
        assert_eq!(at_var_at_cursor("y = foo\n", 0, 5), None);
        // A lone `@` is not a constant.
        assert_eq!(at_var_at_cursor("y = @\n", 0, 4), None);
    }

    #[test]
    fn symbol_rank_orders_exact_prefix_substring() {
        assert_eq!(symbol_rank("MY_FOCUS", "my_focus"), Some(0));
        assert_eq!(symbol_rank("my_focus_tooltip", "my_focus"), Some(1));
        assert_eq!(symbol_rank("@my_const", "my"), Some(2));
        assert_eq!(symbol_rank("unrelated", "my_focus"), None);
        // Empty query admits everything (the picker's initial list).
        assert_eq!(symbol_rank("anything", ""), Some(2));
        // Non-ASCII names take the to_lowercase fallback, same tiers.
        assert_eq!(symbol_rank("İstanbul", &"İstanbul".to_lowercase()), Some(0));
        assert_eq!(
            symbol_rank("İstanbul_x", &"İstanbul".to_lowercase()),
            Some(1)
        );
        assert_eq!(
            symbol_rank("x_İstanbul", &"İstanbul".to_lowercase()),
            Some(2)
        );
        // A query longer than the name must miss, not panic: the ASCII prefix
        // arm slices `name[..query.len()]` behind the containment precheck.
        assert_eq!(symbol_rank("abc", "abcdef"), None);
    }

    fn cand(rank: u8, name: &str, uri: &str, line0: u32, col: u32) -> SymbolCandidate {
        SymbolCandidate {
            rank,
            name: name.to_string(),
            container: None,
            kind: SymbolKind::STRUCT,
            file_uri: uri.to_string(),
            line0,
            col,
        }
    }

    #[test]
    fn top_symbols_matches_sort_and_truncate() {
        // The heap must keep exactly what sort-everything-then-truncate kept.
        // The key is written out independently of SymbolCandidate::sort_key so
        // a field dropped from the impl fails here instead of agreeing with
        // itself on both sides.
        let key =
            |c: &SymbolCandidate| (c.rank, c.name.clone(), c.file_uri.clone(), c.line0, c.col);
        let mut all = Vec::new();
        for (i, rank) in [2u8, 0, 1, 2, 0, 1, 2, 2].into_iter().enumerate() {
            all.push(cand(
                rank,
                &format!("name_{}", i % 5),
                "file:///a",
                i as u32,
                0,
            ));
            all.push(cand(
                rank,
                &format!("name_{}", i % 3),
                "file:///b",
                i as u32,
                7,
            ));
        }
        // Exact duplicates of a front-ranked and a mid-ranked entry, so ties
        // sit on the truncation boundary at the small limits.
        all.push(cand(0, "name_1", "file:///a", 1, 0));
        all.push(cand(2, "name_3", "file:///a", 3, 0));
        for limit in [0, 1, 2, 3, 5, all.len(), all.len() + 10] {
            let mut top = TopSymbols::new(limit);
            for c in &all {
                if top.accepts(c.rank, &c.name, &c.file_uri, c.line0, c.col) {
                    top.push(cand(c.rank, &c.name, &c.file_uri, c.line0, c.col));
                }
            }
            let mut expected: Vec<_> = all.iter().map(&key).collect();
            expected.sort();
            expected.truncate(limit);
            let got: Vec<_> = top.into_sorted_vec().iter().map(&key).collect();
            assert_eq!(got, expected, "limit {limit}");
        }
    }

    #[test]
    fn top_symbols_accepts_only_improving_candidates_when_full() {
        let mut top = TopSymbols::new(2);
        top.push(cand(0, "aaa", "file:///a", 0, 0));
        top.push(cand(1, "bbb", "file:///a", 1, 0));
        // Worse than the kept worst: rejected without displacing anything.
        assert!(!top.accepts(2, "ccc", "file:///a", 2, 0));
        // Equal to the kept worst: rejected (could only swap an identical key).
        assert!(!top.accepts(1, "bbb", "file:///a", 1, 0));
        // Better than the kept worst: accepted, and pushing evicts it.
        assert!(top.accepts(0, "aab", "file:///a", 3, 0));
        top.push(cand(0, "aab", "file:///a", 3, 0));
        let names: Vec<_> = top.into_sorted_vec().into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["aaa", "aab"]);
    }

    #[test]
    fn name_contains_ignore_case_falls_back_for_non_ascii_names() {
        // Matches the old `to_lowercase().contains(..)` behavior exactly, just
        // via the slow path (Turkish İ lowercases to "i̇", not ASCII "i").
        let name = "İstanbul";
        let query = name.to_lowercase();
        assert!(name_contains_ignore_case(name, &query));
        assert!(!name_contains_ignore_case(name, "nomatch"));
    }

    #[test]
    fn prepare_rename_range_uses_negotiated_encoding() {
        let text = "😀 target";
        assert_eq!(
            prepare_rename_range(
                Some(text),
                Position::new(0, 9),
                "target",
                &PositionEncodingKind::UTF16,
            ),
            Range::new(Position::new(0, 3), Position::new(0, 9))
        );
        assert_eq!(
            prepare_rename_range(
                Some(text),
                Position::new(0, 8),
                "target",
                &PositionEncodingKind::UTF32,
            ),
            Range::new(Position::new(0, 2), Position::new(0, 8))
        );
    }

    #[test]
    fn prepare_rename_range_counts_non_bmp_name_units() {
        let text = "name𐐀";
        assert_eq!(
            prepare_rename_range(
                Some(text),
                Position::new(0, 6),
                "name𐐀",
                &PositionEncodingKind::UTF16,
            ),
            Range::new(Position::new(0, 0), Position::new(0, 6))
        );
        assert_eq!(
            prepare_rename_range(
                Some(text),
                Position::new(0, 5),
                "name𐐀",
                &PositionEncodingKind::UTF32,
            ),
            Range::new(Position::new(0, 0), Position::new(0, 5))
        );
    }

    #[test]
    fn source_ranges_use_negotiated_encoding() {
        let text = "😀 name𐐀";
        assert_eq!(
            source_range_in_text(text, 0, 2, "name𐐀", &PositionEncodingKind::UTF16),
            Range::new(Position::new(0, 3), Position::new(0, 9))
        );
        assert_eq!(
            source_range_in_text(text, 0, 2, "name𐐀", &PositionEncodingKind::UTF32),
            Range::new(Position::new(0, 2), Position::new(0, 7))
        );
    }

    #[test]
    fn dedup_locations_collapses_identical() {
        // Issue #62: the same definition reached through two index paths yields
        // two Locations at the same (uri, line, char). They must collapse to one
        // (distinct sites are covered by the tests below).
        let file = "file:///mod/events/a.txt";
        let locs = vec![
            make_location(file, 2, 0),
            make_location(file, 2, 0), // duplicate
        ];
        let deduped = dedup_locations(locs);
        assert_eq!(deduped.len(), 1, "identical locations must collapse to one");
    }

    #[test]
    fn dedup_locations_preserves_distinct_positions() {
        // Mod at line 2, vanilla fallback happens to be at line 6 — two
        // genuinely different definition sites, both must survive.
        let file = "file:///mod/events/a.txt";
        let locs = vec![make_location(file, 2, 0), make_location(file, 6, 0)];
        let deduped = dedup_locations(locs);
        assert_eq!(deduped.len(), 2, "distinct positions must both survive");
    }

    #[test]
    fn dedup_locations_preserves_distinct_uris() {
        // Mod file and a different (real) vanilla file: two separate definitions.
        let locs = vec![
            make_location("file:///mod/events/a.txt", 2, 0),
            make_location("file:///vanilla/events/a.txt", 2, 0),
        ];
        let deduped = dedup_locations(locs);
        assert_eq!(
            deduped.len(),
            2,
            "different URIs at same position must both survive"
        );
    }

    #[test]
    fn code_token_ignore_case_matches_case_insensitively_and_respects_boundaries() {
        // Case-insensitive whole-token: MY_KEY matches my_key / My_Key but not my_key_extra
        assert_eq!(
            code_token_cols_in_line_ignore_case("x = MY_KEY y", "my_key"),
            vec![4]
        );
        assert_eq!(
            code_token_cols_in_line_ignore_case("x = my_key_extra", "my_key"),
            Vec::<u32>::new()
        );
        // Comment handling: # my_key inside comment not counted
        assert_eq!(
            code_token_cols_in_line_ignore_case("x = my_key # MY_KEY", "my_key"),
            vec![4]
        );
        // Quoted: inside "..." still matches (script values are quoted)
        assert_eq!(
            code_token_cols_in_line_ignore_case("x = \"MY_KEY\" # MY_KEY", "my_key"),
            vec![5]
        );
        // Dots are identifier chars, so my.key should not match my key
        assert_eq!(
            code_token_cols_in_line_ignore_case("a = my.key", "my_key"),
            Vec::<u32>::new()
        );
    }

    #[test]
    fn loc_ref_key_cols_finds_dollar_refs_inside_yml() {
        // $KEY$ and $KEY|Y$ both count, case-insensitive, whole-token
        assert_eq!(
            loc_ref_key_cols_in_line("  desc: \"See $MY_KEY$ and $OTHER|Y$\"", "my_key"),
            vec![14]
        );
        assert_eq!(
            loc_ref_key_cols_in_line("  desc: \"See $my_key|Y$\"", "my_key"),
            vec![14]
        );
        assert_eq!(
            loc_ref_key_cols_in_line("x = foo $my_key_extra$ y", "my_key"),
            Vec::<u32>::new()
        );
        // Currency: "$5 for $ITEM$" should pair ITEM correctly (invalid "$5 for $" skips one dollar)
        assert_eq!(
            loc_ref_key_cols_in_line("a $5 for $ITEM$ b", "item").len(),
            1
        );
        assert_eq!(
            loc_ref_key_cols_in_line("a $5 for $ITEM$ b", "5"),
            Vec::<u32>::new()
        );
        // Unquoted # comment: refs after # are ignored
        assert!(loc_ref_key_cols_in_line("x = foo # $my_key$ comment", "my_key").is_empty());
        assert_eq!(
            loc_ref_key_cols_in_line("x = \"foo # $my_key$\" # $my_key$", "my_key").len(),
            1
        );
    }

    #[test]
    fn loc_root_strips_desc_and_tooltip_repeatedly() {
        assert_eq!(loc_root("my_key"), "my_key");
        assert_eq!(loc_root("my_key_desc"), "my_key");
        assert_eq!(loc_root("my_key_tooltip"), "my_key");
        assert_eq!(loc_root("my_key_desc_tooltip"), "my_key");
        assert_eq!(loc_root("my_key_tooltip_desc"), "my_key");
        assert_eq!(loc_root("my_key_desc_desc"), "my_key");
        // Non-suffix substring not stripped
        assert_eq!(loc_root("my_key_desc_extra"), "my_key_desc_extra");
    }

    #[test]
    fn dedup_locations_keeps_first_occurrence() {
        // When two are identical the first must be kept (stable ordering).
        let file = "file:///mod/events/a.txt";
        let first = Location {
            uri: file.parse().unwrap(),
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 10,
                },
            },
        };
        let second = Location {
            uri: file.parse().unwrap(),
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 99,
                }, // different end, same start key
            },
        };
        let deduped = dedup_locations(vec![first.clone(), second]);
        assert_eq!(deduped.len(), 1);
        assert_eq!(
            deduped[0].range.end.character, 10,
            "must keep first occurrence"
        );
    }
}
