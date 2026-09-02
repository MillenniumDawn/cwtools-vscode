use std::collections::{HashMap, HashSet};

use tower_lsp::lsp_types::*;

use cwtools_string_table::string_table::StringTable;

use crate::Backend;
use crate::lines::DocLines;
use crate::paths::{current_token_range_with_encoding, encoded_position_len};

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

pub(crate) const WORKSPACE_SYMBOL_LIMIT: usize = 500;

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

    pub(crate) fn into_sorted_vec(self) -> Vec<SymbolCandidate> {
        self.heap.into_sorted_vec()
    }
}

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

pub(crate) fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '.'
}

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

pub(crate) fn highlight_kind(line: &str, col: u32, name: &str) -> DocumentHighlightKind {
    let after = col as usize + name.chars().count();
    match line.chars().skip(after).find(|c| !c.is_whitespace()) {
        Some('=') => DocumentHighlightKind::WRITE,
        _ => DocumentHighlightKind::READ,
    }
}

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

pub(crate) fn cwt_ref_at(
    line: &str,
    col: u32,
) -> Option<(cwtools_rules::rules_types::CwtDefKind, String)> {
    use cwtools_rules::rules_types::CwtDefKind;
    let chars: Vec<char> = line.chars().collect();
    let col = col as usize;
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

pub(crate) fn at_var_at_cursor(text: &str, line0: u32, col: u32) -> Option<(String, u32)> {
    let line = text.lines().nth(line0 as usize)?;
    let chars: Vec<char> = line.chars().collect();
    let mut cur = (col as usize).min(chars.len());
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

pub(crate) type CharSpan = ((u32, u32), (u32, u32));

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

pub(crate) fn region_marker(line: &str) -> Option<bool> {
    if let Some(rest) = line.strip_prefix("#endregion") {
        (rest.is_empty() || rest.starts_with(char::is_whitespace)).then_some(false)
    } else if let Some(rest) = line.strip_prefix("#region") {
        (rest.is_empty() || rest.starts_with(char::is_whitespace)).then_some(true)
    } else {
        None
    }
}

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

pub(crate) fn build_doc_symbols(
    children: &[cwtools_parser::ast::Child],
    arena: &cwtools_parser::ast::Arena,
    table: &StringTable,
    lines: &DocLines,
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
        let child_syms = build_doc_symbols(kc.children, arena, table, lines);
        let (name, detail) = match identity_value(kc.children, arena, table) {
            Some(v) if v != key => (v, Some(key.clone())),
            _ => (key.clone(), None),
        };
        let start_line = kc.pos.start.line.saturating_sub(1);
        let start = lines.position(start_line, kc.pos.start.col as u32);
        let end = lines.position(kc.pos.end.line.saturating_sub(1), kc.pos.end.col as u32);
        let selection_end = lines
            .token_range(start_line, kc.pos.start.col as u32, &key)
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

pub(crate) fn unquote(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .unwrap_or(s)
}

pub(crate) async fn locations_at(
    backend: &Backend,
    pairs: impl IntoIterator<Item = (String, cwtools_info::SourceLocation)>,
    name: &str,
    fallback: &Url,
) -> Vec<Location> {
    let pairs: Vec<_> = pairs.into_iter().collect();
    let uris: Vec<String> = pairs.iter().map(|(file_uri, _)| file_uri.clone()).collect();
    let texts = backend.file_text_snapshots_for(&uris).await;
    let indexed = crate::lines::index_snapshots(&texts, &backend.position_encoding());
    locations_at_with_lines(backend, pairs, name, fallback, &indexed)
}

pub(crate) fn locations_at_with_lines(
    backend: &Backend,
    pairs: impl IntoIterator<Item = (String, cwtools_info::SourceLocation)>,
    name: &str,
    fallback: &Url,
    indexed: &HashMap<&str, DocLines>,
) -> Vec<Location> {
    pairs
        .into_iter()
        .map(|(file_uri, loc)| {
            backend.source_location_with_lines(
                &file_uri,
                loc.line.saturating_sub(1),
                loc.col as u32,
                name,
                fallback,
                indexed.get(file_uri.as_str()),
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

pub(crate) fn loc_ref_key_cols_in_line(line: &str, needle_lower: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let chars: Vec<char> = line.chars().collect();
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
        let spans = selection_spans(text, &pairs, 1, 2);
        assert_eq!(spans, vec![((0, 5), (2, 0)), ((0, 4), (2, 1))]);
    }

    #[test]
    fn member_pos_in_block_finds_enum_value() {
        let text = "enums = {\n    enum[terrain] = {\n        plains\n        forest\n    }\n}\n";
        assert_eq!(member_pos_in_block(text, 1, 4, "forest"), Some((3, 8)));
        let text = "enums = {\n    enum[terrain] = { plains forest }\n}\n";
        assert_eq!(member_pos_in_block(text, 1, 4, "forest"), Some((1, 29)));
    }

    #[test]
    fn member_pos_in_block_skips_what_isnt_a_member() {
        let text =
            "enums = {\n    complex_enum[tags] = {\n        path = \"game/common\"\n    }\n}\n";
        assert_eq!(member_pos_in_block(text, 1, 4, "USA"), None);
        let text =
            "enums = {\n    enum[terrain] = {\n        # forest\n        forestry\n    }\n}\n";
        assert_eq!(member_pos_in_block(text, 1, 4, "forest"), None);
    }

    #[test]
    fn cwt_ref_at_classifies_rule_references() {
        use cwtools_rules::rules_types::CwtDefKind;
        let line = "    has_focus = <focus>";
        assert_eq!(
            cwt_ref_at(line, 18),
            Some((CwtDefKind::Type, "focus".to_string()))
        );
        assert_eq!(
            cwt_ref_at(line, 16),
            Some((CwtDefKind::Type, "focus".to_string()))
        );
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
        assert_eq!(cwt_ref_at("    alias_name[effect] = x", 12), None);
        assert_eq!(cwt_ref_at("    v = value[my_set]", 16), None);
        assert_eq!(cwt_ref_at("    has_focus = <focus>", 6), None);
    }

    #[test]
    fn at_var_at_cursor_finds_sigil_token() {
        let text = "y = @foo\n";
        assert_eq!(at_var_at_cursor(text, 0, 6), Some(("@foo".to_string(), 4)));
        assert_eq!(at_var_at_cursor(text, 0, 4), Some(("@foo".to_string(), 4)));
        assert_eq!(at_var_at_cursor(text, 0, 8), Some(("@foo".to_string(), 4)));
        assert_eq!(at_var_at_cursor("y = foo\n", 0, 5), None);
        assert_eq!(at_var_at_cursor("y = @\n", 0, 4), None);
    }

    #[test]
    fn symbol_rank_orders_exact_prefix_substring() {
        assert_eq!(symbol_rank("MY_FOCUS", "my_focus"), Some(0));
        assert_eq!(symbol_rank("my_focus_tooltip", "my_focus"), Some(1));
        assert_eq!(symbol_rank("@my_const", "my"), Some(2));
        assert_eq!(symbol_rank("unrelated", "my_focus"), None);
        assert_eq!(symbol_rank("anything", ""), Some(2));
        assert_eq!(symbol_rank("İstanbul", &"İstanbul".to_lowercase()), Some(0));
        assert_eq!(
            symbol_rank("İstanbul_x", &"İstanbul".to_lowercase()),
            Some(1)
        );
        assert_eq!(
            symbol_rank("x_İstanbul", &"İstanbul".to_lowercase()),
            Some(2)
        );
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
        assert!(!top.accepts(2, "ccc", "file:///a", 2, 0));
        assert!(!top.accepts(1, "bbb", "file:///a", 1, 0));
        assert!(top.accepts(0, "aab", "file:///a", 3, 0));
        top.push(cand(0, "aab", "file:///a", 3, 0));
        let names: Vec<_> = top.into_sorted_vec().into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["aaa", "aab"]);
    }

    #[test]
    fn name_contains_ignore_case_falls_back_for_non_ascii_names() {
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

    /// node and the request never comes back (#541); the whole point of
    #[test]
    fn document_symbols_of_a_huge_file_stay_linear() {
        const CLAUSES: usize = 100_000;
        let text = "a={}\n".repeat(CLAUSES);
        let table = StringTable::new();
        let ast = cwtools_parser::parser::parse_string(&text, &table);
        let lines = DocLines::new(&text, PositionEncodingKind::UTF16);
        let syms = build_doc_symbols(&ast.root_children, &ast.arena, &table, &lines);
        assert_eq!(syms.len(), CLAUSES);
        assert_eq!(syms[0].name, "a");
        assert_eq!(syms[0].range.start, Position::new(0, 0));
        assert_eq!(syms[0].selection_range.end, Position::new(0, 1));
        let last = &syms[CLAUSES - 1];
        assert_eq!(last.range.start, Position::new(CLAUSES as u32 - 1, 0));
        assert_eq!(
            last.selection_range.end,
            Position::new(CLAUSES as u32 - 1, 1)
        );
    }

    #[test]
    fn dedup_locations_collapses_identical() {
        // Issue #62: the same definition reached through two index paths yields
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
        let file = "file:///mod/events/a.txt";
        let locs = vec![make_location(file, 2, 0), make_location(file, 6, 0)];
        let deduped = dedup_locations(locs);
        assert_eq!(deduped.len(), 2, "distinct positions must both survive");
    }

    #[test]
    fn dedup_locations_preserves_distinct_uris() {
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
        assert_eq!(
            code_token_cols_in_line_ignore_case("x = MY_KEY y", "my_key"),
            vec![4]
        );
        assert_eq!(
            code_token_cols_in_line_ignore_case("x = my_key_extra", "my_key"),
            Vec::<u32>::new()
        );
        assert_eq!(
            code_token_cols_in_line_ignore_case("x = my_key # MY_KEY", "my_key"),
            vec![4]
        );
        assert_eq!(
            code_token_cols_in_line_ignore_case("x = \"MY_KEY\" # MY_KEY", "my_key"),
            vec![5]
        );
        assert_eq!(
            code_token_cols_in_line_ignore_case("a = my.key", "my_key"),
            Vec::<u32>::new()
        );
    }

    #[test]
    fn loc_ref_key_cols_finds_dollar_refs_inside_yml() {
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
        assert_eq!(
            loc_ref_key_cols_in_line("a $5 for $ITEM$ b", "item").len(),
            1
        );
        assert_eq!(
            loc_ref_key_cols_in_line("a $5 for $ITEM$ b", "5"),
            Vec::<u32>::new()
        );
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
        assert_eq!(loc_root("my_key_desc_extra"), "my_key_desc_extra");
    }

    #[test]
    fn dedup_locations_keeps_first_occurrence() {
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
