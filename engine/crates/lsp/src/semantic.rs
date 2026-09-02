//! Columns and lengths are in the NEGOTIATED position encoding (utf-16 by

use std::cell::Cell;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use cwtools_info::ReferenceHint;
use cwtools_parser::ast::{Arena, Child, ParsedFile, Value};
use cwtools_rules::rules_types::{NewField, Options, RuleSet, RuleType};
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::Prepared;
use cwtools_validation::position::{rules_at_pos, value_rules_for_key};

use crate::Backend;

const TOKEN_TYPES: [SemanticTokenType; 11] = [
    SemanticTokenType::COMMENT,     // 0
    SemanticTokenType::PROPERTY,    // 1 — a leaf/block key
    SemanticTokenType::OPERATOR,    // 2 — `=`, `>=`, `!=`, …
    SemanticTokenType::NUMBER,      // 3
    SemanticTokenType::STRING,      // 4 — unclassified scalar, LocRef, FileRef
    SemanticTokenType::KEYWORD,     // 5 — `yes` / `no`
    SemanticTokenType::TYPE,        // 6 — TypeRef value, or a type-declaring key
    SemanticTokenType::ENUM_MEMBER, // 7 — EnumRef value
    SemanticTokenType::VARIABLE,    // 8 — script variable read
    SemanticTokenType::NAMESPACE,   // 9 — scope name
    SemanticTokenType::FUNCTION,    // 10 — key resolved through an alias category
];

const TOKEN_MODIFIERS: [SemanticTokenModifier; 1] = [
    SemanticTokenModifier::DECLARATION, // bit 0 — the key names a type instance
];

const TY_COMMENT: u32 = 0;
const TY_PROPERTY: u32 = 1;
const TY_OPERATOR: u32 = 2;
const TY_NUMBER: u32 = 3;
const TY_STRING: u32 = 4;
const TY_KEYWORD: u32 = 5;
const TY_TYPE: u32 = 6;
const TY_ENUM_MEMBER: u32 = 7;
const TY_VARIABLE: u32 = 8;
const TY_NAMESPACE: u32 = 9;
const TY_FUNCTION: u32 = 10;

const MOD_NONE: u32 = 0;
const MOD_DECLARATION: u32 = 1 << 0;

const MAX_RULE_BOOTSTRAPS: u32 = 2000;

pub(crate) fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: TOKEN_TYPES.to_vec(),
        token_modifiers: TOKEN_MODIFIERS.to_vec(),
    }
}

/// One token at its absolute position, before delta encoding. `line` is 0-based;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AbsToken {
    pub(crate) line: u32,
    pub(crate) start: u32,
    pub(crate) length: u32,
    pub(crate) token_type: u32,
    pub(crate) modifiers: u32,
}

/// descending into its clause, which is already source order, but the encoding
pub(crate) fn encode(mut tokens: Vec<AbsToken>) -> Vec<SemanticToken> {
    tokens.retain(|t| t.length > 0);
    tokens.sort_by_key(|t| (t.line, t.start));
    let mut out = Vec::with_capacity(tokens.len());
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;
    for t in tokens {
        let delta_line = t.line - prev_line;
        let delta_start = if delta_line == 0 {
            t.start - prev_start
        } else {
            t.start
        };
        out.push(SemanticToken {
            delta_line,
            delta_start,
            length: t.length,
            token_type: t.token_type,
            token_modifiers_bitset: t.modifiers,
        });
        prev_line = t.line;
        prev_start = t.start;
    }
    out
}

type LineSpan = (u32, u32);

struct Ctx<'a> {
    arena: &'a Arena,
    table: &'a StringTable,
    lines: Vec<Vec<char>>,
    encoding: &'a PositionEncodingKind,
    rules: Option<RuleCtx<'a>>,
    span: Option<LineSpan>,
}

struct RuleCtx<'a> {
    ast: &'a ParsedFile,
    logical_path: &'a str,
    prepared: &'a Prepared<'a>,
    ruleset: &'a RuleSet,
    bootstraps_left: Cell<u32>,
}

impl RuleCtx<'_> {
    fn bootstrap(&self, children: &[Child]) -> Option<Vec<(RuleType, Options)>> {
        let left = self.bootstraps_left.get();
        if left == 0 {
            return None;
        }
        self.bootstraps_left.set(left - 1);
        block_rules_for(self.ast, self.prepared, self.logical_path, children)
    }
}

impl Ctx<'_> {
    fn worth_entering(&self, child: &Child) -> bool {
        let Some((first, last)) = self.span else {
            return true;
        };
        let pos = match child {
            Child::Leaf(i) => self.arena.leaves[*i as usize].pos,
            Child::LeafValue(i) => self.arena.leaf_values[*i as usize].pos,
            Child::Comment(i) => self.arena.comments[*i as usize].pos,
        };
        pos.start.line <= last && pos.end.line >= first
    }

    fn line(&self, line0: u32) -> &[char] {
        self.lines.get(line0 as usize).map_or(&[], |l| l.as_slice())
    }

    /// parser char columns into the negotiated encoding. `None` outside the
    fn token(
        &self,
        line0: u32,
        start_col: usize,
        end_col: usize,
        token_type: u32,
        modifiers: u32,
    ) -> Option<AbsToken> {
        if let Some((first, last)) = self.span
            && !(first..=last).contains(&(line0 + 1))
        {
            return None;
        }
        let line = self.line(line0);
        if start_col >= end_col || end_col > line.len() {
            return None;
        }
        Some(AbsToken {
            line: line0,
            start: encoded_len(&line[..start_col], self.encoding),
            length: encoded_len(&line[start_col..end_col], self.encoding),
            token_type,
            modifiers,
        })
    }
}

/// Length of `chars` in the negotiated encoding — the same conversion
fn encoded_len(chars: &[char], encoding: &PositionEncodingKind) -> u32 {
    if encoding == &PositionEncodingKind::UTF32 {
        chars.len() as u32
    } else {
        chars.iter().map(|c| c.len_utf16() as u32).sum()
    }
}

pub(crate) fn semantic_tokens(
    file: &ParsedFile,
    table: &StringTable,
    text: &str,
    encoding: &PositionEncodingKind,
    rules: Option<(&Prepared<'_>, &str)>,
    span: Option<LineSpan>,
) -> Vec<AbsToken> {
    let cx = Ctx {
        arena: &file.arena,
        table,
        lines: text.lines().map(|l| l.chars().collect()).collect(),
        encoding,
        rules: rules.map(|(prepared, logical_path)| RuleCtx {
            ast: file,
            logical_path,
            prepared,
            ruleset: prepared.ruleset,
            bootstraps_left: Cell::new(MAX_RULE_BOOTSTRAPS),
        }),
        span,
    };
    let mut out = Vec::new();
    let root_rules = cx
        .rules
        .as_ref()
        .and_then(|r| r.bootstrap(&file.root_children));
    collect(&file.root_children, &cx, root_rules.as_deref(), &mut out);
    out
}

pub(crate) fn block_rules_for(
    ast: &ParsedFile,
    prepared: &Prepared<'_>,
    logical_path: &str,
    children: &[Child],
) -> Option<Vec<(RuleType, Options)>> {
    let (line, col) = first_leaf_key_pos(children, &ast.arena)?;
    rules_at_pos(ast, logical_path, prepared, line, col, false).map(|rctx| rctx.child_rules)
}

fn first_leaf_key_pos(children: &[Child], arena: &Arena) -> Option<(u32, u16)> {
    children
        .iter()
        .find_map(|c| match c {
            Child::Leaf(i) => {
                let p = arena.leaves[*i as usize].pos.start;
                Some((p.line, p.col))
            }
            _ => None,
        })
        .or_else(|| {
            children.iter().find_map(|c| match c {
                Child::LeafValue(i) => {
                    let p = arena.leaf_values[*i as usize].pos.start;
                    Some((p.line, p.col))
                }
                _ => None,
            })
        })
}

fn collect(
    children: &[Child],
    cx: &Ctx<'_>,
    block_rules: Option<&[(RuleType, Options)]>,
    out: &mut Vec<AbsToken>,
) {
    for child in children {
        if !cx.worth_entering(child) {
            continue;
        }
        match child {
            Child::Comment(idx) => {
                let c = &cx.arena.comments[*idx as usize];
                let line0 = c.pos.start.line.saturating_sub(1);
                if let Some(t) = cx.token(
                    line0,
                    c.pos.start.col as usize,
                    c.pos.end.col as usize,
                    TY_COMMENT,
                    MOD_NONE,
                ) {
                    out.push(t);
                }
            }
            Child::Leaf(idx) => collect_leaf(*idx, cx, block_rules, out),
            Child::LeafValue(idx) => {
                let lv = &cx.arena.leaf_values[*idx as usize];
                let line0 = lv.pos.start.line.saturating_sub(1);
                if let Value::Clause(inner) = &lv.value {
                    let inner_rules = block_rules.map(valueclause_bodies);
                    collect(inner, cx, inner_rules.as_deref(), out);
                    continue;
                }
                let start = lv.pos.start.col as usize;
                let Some((s, e)) = value_token_span(cx.line(line0), start) else {
                    continue;
                };
                let ty = leaf_value_token_type(&lv.value, cx, block_rules);
                if let Some(t) = cx.token(line0, s, e, ty, MOD_NONE) {
                    out.push(t);
                }
            }
        }
    }
}

fn collect_leaf(
    idx: u32,
    cx: &Ctx<'_>,
    block_rules: Option<&[(RuleType, Options)]>,
    out: &mut Vec<AbsToken>,
) {
    let leaf = &cx.arena.leaves[idx as usize];
    let line0 = leaf.pos.start.line.saturating_sub(1);
    let line = cx.line(line0);
    let raw_key = cx.table.get_string(leaf.key.normal).unwrap_or_default();
    let key_col = leaf.pos.start.col as usize;
    let key_len = raw_key.chars().count();
    let key = raw_key.trim_matches('"');

    let matched = match (cx.rules.as_ref(), block_rules) {
        (Some(r), Some(rules)) => value_rules_for_key(r.ruleset, r.prepared.type_index, rules, key),
        _ => Vec::new(),
    };

    let (key_ty, key_mods) = key_token_class(cx, block_rules, &matched, key);
    if let Some(t) = cx.token(line0, key_col, key_col + key_len, key_ty, key_mods) {
        out.push(t);
    }

    let op = leaf.op.as_str();
    if let Some(op_col) = find_token_col(line, op, key_col + key_len)
        && let Some(t) = cx.token(
            line0,
            op_col,
            op_col + op.chars().count(),
            TY_OPERATOR,
            MOD_NONE,
        )
    {
        out.push(t);
    }

    if let Value::Clause(inner) = &leaf.value {
        let inner_rules = match block_rules {
            None => cx.rules.as_ref().and_then(|r| r.bootstrap(inner)),
            Some(_) => Some(node_bodies(&matched)),
        };
        collect(inner, cx, inner_rules.as_deref(), out);
        return;
    }

    let from = find_token_col(line, op, key_col + key_len)
        .map(|c| c + op.chars().count())
        .unwrap_or(key_col + key_len);
    let Some((s, e)) = value_token_span(line, from) else {
        return;
    };
    let ty = value_token_type(&leaf.value, &matched, cx);
    if let Some(t) = cx.token(line0, s, e, ty, MOD_NONE) {
        out.push(t);
    }
}

fn key_token_class(
    cx: &Ctx<'_>,
    block_rules: Option<&[(RuleType, Options)]>,
    matched: &[&(RuleType, Options)],
    key: &str,
) -> (u32, u32) {
    let (Some(r), Some(rules)) = (cx.rules.as_ref(), block_rules) else {
        return (TY_PROPERTY, MOD_NONE);
    };
    if cwtools_validation::position::alias_category_for_key(
        r.ruleset,
        r.prepared.type_index,
        rules,
        key,
    )
    .is_some()
    {
        return (TY_FUNCTION, MOD_NONE);
    }
    let declares_type = matched.iter().any(|(rt, _)| {
        matches!(
            rt,
            RuleType::LeafRule {
                left: NewField::TypeField(_),
                ..
            } | RuleType::NodeRule {
                left: NewField::TypeField(_),
                ..
            }
        )
    });
    if declares_type {
        (TY_TYPE, MOD_DECLARATION)
    } else {
        (TY_PROPERTY, MOD_NONE)
    }
}

fn value_token_type(value: &Value, matched: &[&(RuleType, Options)], cx: &Ctx<'_>) -> u32 {
    match value {
        Value::Int(_) | Value::Float(_) => TY_NUMBER,
        Value::Bool(_) => TY_KEYWORD,
        Value::Clause(_) => TY_STRING,
        Value::String(t) | Value::QString(t) => {
            let Some(r) = cx.rules.as_ref() else {
                return TY_STRING;
            };
            let text = cx.table.get_string(t.normal).unwrap_or_default();
            let text = text.trim_matches('"');
            matched
                .iter()
                .find_map(|(rt, _)| {
                    hint_token_type(&crate::hint_from_rule_right(rt, text, r.ruleset))
                })
                .unwrap_or(TY_STRING)
        }
    }
}

fn leaf_value_token_type(
    value: &Value,
    cx: &Ctx<'_>,
    block_rules: Option<&[(RuleType, Options)]>,
) -> u32 {
    match value {
        Value::Int(_) | Value::Float(_) => TY_NUMBER,
        Value::Bool(_) => TY_KEYWORD,
        _ => {
            let Some(rules) = block_rules else {
                return TY_STRING;
            };
            let leaf_values: Vec<&(RuleType, Options)> = rules
                .iter()
                .filter(|(rt, _)| matches!(rt, RuleType::LeafValueRule { .. }))
                .collect();
            value_token_type(value, &leaf_values, cx)
        }
    }
}

fn hint_token_type(hint: &ReferenceHint) -> Option<u32> {
    match hint {
        ReferenceHint::TypeRef { .. } => Some(TY_TYPE),
        ReferenceHint::EnumRef { .. } => Some(TY_ENUM_MEMBER),
        ReferenceHint::Variable { .. } => Some(TY_VARIABLE),
        ReferenceHint::ScopeName { .. } => Some(TY_NAMESPACE),
        ReferenceHint::LocRef { .. } | ReferenceHint::FileRef { .. } => None,
        ReferenceHint::Unknown => None,
    }
}

pub(crate) fn node_bodies(matched: &[&(RuleType, Options)]) -> Vec<(RuleType, Options)> {
    let mut out = Vec::new();
    for (rt, _) in matched.iter().copied() {
        if let RuleType::NodeRule { rules, .. } = rt {
            for r in rules.iter() {
                match &r.0 {
                    RuleType::SubtypeRule { rules: inner, .. } => out.extend(inner.iter().cloned()),
                    _ => out.push(r.clone()),
                }
            }
        }
    }
    out
}

pub(crate) fn valueclause_bodies(rules: &[(RuleType, Options)]) -> Vec<(RuleType, Options)> {
    rules
        .iter()
        .filter_map(|(rt, _)| match rt {
            RuleType::ValueClauseRule { rules } => Some(rules.iter().cloned()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn find_token_col(line: &[char], needle: &str, from: usize) -> Option<usize> {
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() || line.len() < needle.len() {
        return None;
    }
    (from..=line.len() - needle.len()).find(|&i| line[i..i + needle.len()] == needle[..])
}

fn value_token_span(line: &[char], from: usize) -> Option<(usize, usize)> {
    let mut i = from.min(line.len());
    while i < line.len() && line[i].is_whitespace() {
        i += 1;
    }
    if i >= line.len() || line[i] == '#' || line[i] == '{' || line[i] == '}' {
        return None;
    }
    let start = i;
    if line[i] == '"' {
        i += 1;
        while i < line.len() && line[i] != '"' {
            i += 1;
        }
        if i < line.len() {
            i += 1;
        }
    } else {
        while i < line.len() && !line[i].is_whitespace() && !matches!(line[i], '#' | '{' | '}') {
            i += 1;
        }
    }
    Some((start, i))
}

fn compute_semantic_delta(
    prev: &[SemanticToken],
    next: &[SemanticToken],
) -> Vec<SemanticTokensEdit> {
    if prev == next {
        return Vec::new();
    }
    let mut prefix = 0usize;
    while prefix < prev.len() && prefix < next.len() && prev[prefix] == next[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < prev.len() - prefix
        && suffix < next.len() - prefix
        && prev[prev.len() - 1 - suffix] == next[next.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let start = (prefix * 5) as u32;
    let delete_count = ((prev.len() - prefix - suffix) * 5) as u32;
    let data = next[prefix..next.len() - suffix].to_vec();
    let data_opt = if data.is_empty() { None } else { Some(data) };
    vec![SemanticTokensEdit {
        start,
        delete_count,
        data: data_opt,
    }]
}

impl Backend {
    #[tracing::instrument(skip_all)]
    pub(crate) async fn semantic_tokens_full_impl(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri.to_string();
        if let Some(text) = self.file_text_for(&uri).await {
            let hash = cwtools_cache::workspace::content_hash(&text);
            // scrutinee would still be alive inside the block (#334).
            let cached = self.state.semantic_tokens_cache.lock().get(&uri).cloned();
            if let Some(entry) = cached
                && entry.hash == hash
            {
                let result_id = self
                    .state
                    .semantic_tokens_seq
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    .to_string();
                let data = entry.data.clone();
                self.state.semantic_tokens_cache.lock().insert(
                    uri.clone(),
                    crate::SemanticCacheEntry {
                        result_id: result_id.clone(),
                        data: data.clone(),
                        hash,
                    },
                );
                return Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
                    result_id: Some(result_id),
                    data,
                })));
            }
        }
        let Some(data) = self.tokens_for(&uri, None).await else {
            return Ok(None);
        };
        let hash = self
            .file_text_for(&uri)
            .await
            .map(|t| cwtools_cache::workspace::content_hash(&t))
            .unwrap_or(0);
        let result_id = self
            .state
            .semantic_tokens_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .to_string();
        self.state.semantic_tokens_cache.lock().insert(
            uri.clone(),
            crate::SemanticCacheEntry {
                result_id: result_id.clone(),
                data: data.clone(),
                hash,
            },
        );
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: Some(result_id),
            data,
        })))
    }

    #[tracing::instrument(skip_all)]
    pub(crate) async fn semantic_tokens_full_delta_impl(
        &self,
        params: SemanticTokensDeltaParams,
    ) -> Result<Option<SemanticTokensFullDeltaResult>> {
        let uri = params.text_document.uri.to_string();
        if let Some(text) = self.file_text_for(&uri).await {
            let hash = cwtools_cache::workspace::content_hash(&text);
            let cached = self.state.semantic_tokens_cache.lock().get(&uri).cloned();
            if let Some(entry) = cached
                && entry.hash == hash
                && entry.result_id == params.previous_result_id
            {
                let result_id = self
                    .state
                    .semantic_tokens_seq
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    .to_string();
                self.state.semantic_tokens_cache.lock().insert(
                    uri.clone(),
                    crate::SemanticCacheEntry {
                        result_id: result_id.clone(),
                        data: entry.data.clone(),
                        hash,
                    },
                );
                return Ok(Some(SemanticTokensFullDeltaResult::TokensDelta(
                    SemanticTokensDelta {
                        result_id: Some(result_id),
                        edits: Vec::new(),
                    },
                )));
            }
        }
        let Some(new_data) = self.tokens_for(&uri, None).await else {
            return Ok(None);
        };
        let hash = self
            .file_text_for(&uri)
            .await
            .map(|t| cwtools_cache::workspace::content_hash(&t))
            .unwrap_or(0);
        let previous_id = params.previous_result_id;
        let cached = self.state.semantic_tokens_cache.lock().get(&uri).cloned();
        if let Some(entry) = cached
            && entry.result_id == previous_id
        {
            let edits = compute_semantic_delta(&entry.data, &new_data);
            let result_id = self
                .state
                .semantic_tokens_seq
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                .to_string();
            self.state.semantic_tokens_cache.lock().insert(
                uri.clone(),
                crate::SemanticCacheEntry {
                    result_id: result_id.clone(),
                    data: new_data.clone(),
                    hash,
                },
            );
            return Ok(Some(SemanticTokensFullDeltaResult::TokensDelta(
                SemanticTokensDelta {
                    result_id: Some(result_id),
                    edits,
                },
            )));
        }
        let result_id = self
            .state
            .semantic_tokens_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .to_string();
        self.state.semantic_tokens_cache.lock().insert(
            uri.clone(),
            crate::SemanticCacheEntry {
                result_id: result_id.clone(),
                data: new_data.clone(),
                hash,
            },
        );
        Ok(Some(SemanticTokensFullDeltaResult::Tokens(
            SemanticTokens {
                result_id: Some(result_id),
                data: new_data,
            },
        )))
    }

    #[tracing::instrument(skip_all)]
    pub(crate) async fn semantic_tokens_range_impl(
        &self,
        params: SemanticTokensRangeParams,
    ) -> Result<Option<SemanticTokensRangeResult>> {
        let uri = params.text_document.uri.to_string();
        let end = params.range.end;
        let last = if end.character == 0 {
            end.line.saturating_sub(1)
        } else {
            end.line
        };
        let span = (params.range.start.line + 1, last + 1);
        Ok(self.tokens_for(&uri, Some(span)).await.map(|data| {
            SemanticTokensRangeResult::Tokens(SemanticTokens {
                result_id: None,
                data,
            })
        }))
    }

    async fn tokens_for(&self, uri: &str, span: Option<LineSpan>) -> Option<Vec<SemanticToken>> {
        if crate::paths::has_loc_ext(uri) || crate::paths::is_cwt_file(uri) {
            return None;
        }
        let ast = self.ast_for(uri)?;
        let text = self.file_text_for(uri).await?;
        let (game, scope_checks, var_checks, encoding, ws_prefix) = {
            let cfg = self.state.config.read();
            (
                cfg.game(),
                cfg.scope_checks,
                cfg.var_checks,
                cfg.position_encoding.clone(),
                cfg.workspace_prefix.clone(),
            )
        };
        let logical_path = crate::paths::logical_path_from_uri(uri, &ws_prefix);
        let (ruleset, modifier_keys, scope_registry) = {
            let rules = self.state.rules.read();
            (
                rules.ruleset.clone(),
                rules.modifier_keys.clone(),
                rules.scope_registry.clone(),
            )
        };

        let tokens = match ruleset {
            Some(ruleset) => {
                let info = self.state.info_service.read();
                let inline_guard = self.state.inline_scripts.read();
                let prepared = crate::validate::make_prepared(
                    &ruleset,
                    &self.state.string_table,
                    game,
                    &info.type_index,
                    &modifier_keys,
                    None,
                    None,
                    Some(&inline_guard),
                    scope_registry.as_ref(),
                    scope_checks,
                    var_checks,
                );
                semantic_tokens(
                    &ast,
                    &self.state.string_table,
                    &text,
                    &encoding,
                    Some((&prepared, logical_path.as_str())),
                    span,
                )
            }
            None => semantic_tokens(&ast, &self.state.string_table, &text, &encoding, None, span),
        };

        Some(encode(tokens))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens_for(text: &str) -> Vec<AbsToken> {
        let table = StringTable::new();
        let ast = cwtools_parser::parser::parse_string(text, &table);
        semantic_tokens(&ast, &table, text, &PositionEncodingKind::UTF16, None, None)
    }

    fn tokens_in_span(text: &str, span: LineSpan) -> Vec<AbsToken> {
        let table = StringTable::new();
        let ast = cwtools_parser::parser::parse_string(text, &table);
        semantic_tokens(
            &ast,
            &table,
            text,
            &PositionEncodingKind::UTF16,
            None,
            Some(span),
        )
    }

    fn at(tokens: &[AbsToken], line: u32, start: u32) -> AbsToken {
        *tokens
            .iter()
            .find(|t| t.line == line && t.start == start)
            .unwrap_or_else(|| panic!("no token at {line}:{start} in {tokens:#?}"))
    }

    fn abs(line: u32, start: u32, length: u32, token_type: u32) -> AbsToken {
        AbsToken {
            line,
            start,
            length,
            token_type,
            modifiers: MOD_NONE,
        }
    }

    #[test]
    fn encode_first_token_is_absolute() {
        let out = encode(vec![abs(3, 7, 4, TY_PROPERTY)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].delta_line, 3, "first line delta is from line 0");
        assert_eq!(out[0].delta_start, 7, "first start is absolute");
        assert_eq!(out[0].length, 4);
        assert_eq!(out[0].token_type, TY_PROPERTY);
    }

    #[test]
    fn encode_same_line_deltas_start_relative_to_previous() {
        let out = encode(vec![
            abs(0, 0, 3, TY_PROPERTY),
            abs(0, 4, 1, TY_OPERATOR),
            abs(0, 6, 3, TY_STRING),
        ]);
        let deltas: Vec<(u32, u32, u32)> = out
            .iter()
            .map(|t| (t.delta_line, t.delta_start, t.length))
            .collect();
        assert_eq!(deltas, vec![(0, 0, 3), (0, 4, 1), (0, 2, 3)]);
    }

    #[test]
    fn encode_new_line_resets_start_to_absolute() {
        let out = encode(vec![abs(0, 10, 2, TY_PROPERTY), abs(2, 4, 2, TY_PROPERTY)]);
        assert_eq!((out[1].delta_line, out[1].delta_start), (2, 4));
    }

    #[test]
    fn encode_sorts_out_of_order_input() {
        let sorted = encode(vec![
            abs(0, 0, 1, TY_PROPERTY),
            abs(1, 0, 1, TY_PROPERTY),
            abs(1, 5, 1, TY_PROPERTY),
        ]);
        let shuffled = encode(vec![
            abs(1, 5, 1, TY_PROPERTY),
            abs(0, 0, 1, TY_PROPERTY),
            abs(1, 0, 1, TY_PROPERTY),
        ]);
        assert_eq!(sorted, shuffled);
    }

    #[test]
    fn encode_round_trips_back_to_absolute_positions() {
        let input = vec![
            abs(0, 0, 3, TY_PROPERTY),
            abs(0, 4, 1, TY_OPERATOR),
            abs(0, 6, 3, TY_STRING),
            abs(1, 2, 5, TY_COMMENT),
            abs(9, 0, 1, TY_NUMBER),
        ];
        let encoded = encode(input.clone());
        let mut line = 0u32;
        let mut start = 0u32;
        let decoded: Vec<AbsToken> = encoded
            .iter()
            .map(|t| {
                line += t.delta_line;
                start = if t.delta_line == 0 {
                    start + t.delta_start
                } else {
                    t.delta_start
                };
                AbsToken {
                    line,
                    start,
                    length: t.length,
                    token_type: t.token_type,
                    modifiers: t.token_modifiers_bitset,
                }
            })
            .collect();
        assert_eq!(decoded, input);
    }

    #[test]
    fn encode_drops_zero_length_tokens() {
        let out = encode(vec![abs(0, 0, 0, TY_PROPERTY), abs(0, 4, 2, TY_STRING)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].delta_start, 4, "the dropped token leaves no gap");
    }

    #[test]
    fn encode_carries_the_modifier_bitset() {
        let out = encode(vec![AbsToken {
            line: 0,
            start: 0,
            length: 3,
            token_type: TY_TYPE,
            modifiers: MOD_DECLARATION,
        }]);
        assert_eq!(out[0].token_modifiers_bitset, 1);
    }

    #[test]
    fn encode_of_nothing_is_empty() {
        assert!(encode(Vec::new()).is_empty());
    }

    #[test]
    fn key_operator_and_value_are_separate_tokens() {
        let tokens = tokens_for("cost = 5\n");
        assert_eq!(at(&tokens, 0, 0).token_type, TY_PROPERTY);
        assert_eq!(at(&tokens, 0, 0).length, 4);
        assert_eq!(at(&tokens, 0, 5).token_type, TY_OPERATOR);
        assert_eq!(at(&tokens, 0, 5).length, 1);
        assert_eq!(at(&tokens, 0, 7).token_type, TY_NUMBER);
        assert_eq!(at(&tokens, 0, 7).length, 1);
    }

    #[test]
    fn comparison_operators_are_located_whole() {
        let tokens = tokens_for("threat >= 0.5\n");
        let op = at(&tokens, 0, 7);
        assert_eq!(op.token_type, TY_OPERATOR);
        assert_eq!(op.length, 2);
        assert_eq!(at(&tokens, 0, 10).token_type, TY_NUMBER);
    }

    #[test]
    fn booleans_are_keywords_and_quoted_values_keep_their_quotes() {
        let tokens = tokens_for("a = yes\nb = \"hi there\"\n");
        assert_eq!(at(&tokens, 0, 4).token_type, TY_KEYWORD);
        let quoted = at(&tokens, 1, 4);
        assert_eq!(quoted.token_type, TY_STRING);
        assert_eq!(quoted.length, 10, "the span covers both quotes");
    }

    #[test]
    fn comments_are_tokenised_leading_and_trailing() {
        let tokens = tokens_for("# lead\na = b # trail\n");
        assert_eq!(at(&tokens, 0, 0).token_type, TY_COMMENT);
        assert_eq!(at(&tokens, 0, 0).length, 6);
        let trail = at(&tokens, 1, 6);
        assert_eq!(trail.token_type, TY_COMMENT);
        assert_eq!(trail.length, 7);
        assert_eq!(at(&tokens, 1, 4).length, 1);
    }

    #[test]
    fn nested_blocks_and_bare_list_values_are_tokenised() {
        let text = "focus = {\n    id = my_focus\n    list = { a 2 }\n}\n";
        let tokens = tokens_for(text);
        assert_eq!(at(&tokens, 0, 0).token_type, TY_PROPERTY, "outer key");
        assert_eq!(at(&tokens, 1, 4).token_type, TY_PROPERTY, "nested key");
        assert_eq!(at(&tokens, 1, 9).token_type, TY_STRING, "nested value");
        assert_eq!(at(&tokens, 2, 13).token_type, TY_STRING, "bare string");
        assert_eq!(at(&tokens, 2, 15).token_type, TY_NUMBER, "bare number");
    }

    #[test]
    fn a_clause_key_emits_no_value_token() {
        let tokens = tokens_for("focus = {\n    x = 1\n}\n");
        assert!(
            !tokens.iter().any(|t| t.line == 0 && t.start > 7),
            "nothing past the `=` on the clause line: {tokens:#?}"
        );
    }

    #[test]
    fn encoded_stream_of_a_real_file_decodes_to_the_source_columns() {
        let text = "a = 1\nbb = 22\n";
        let encoded = encode(tokens_for(text));
        let mut line = 0u32;
        let mut start = 0u32;
        let mut spans = Vec::new();
        for t in &encoded {
            line += t.delta_line;
            start = if t.delta_line == 0 {
                start + t.delta_start
            } else {
                t.delta_start
            };
            spans.push((line, start, t.length));
        }
        assert_eq!(
            spans,
            vec![
                (0, 0, 1), // a
                (0, 2, 1), // =
                (0, 4, 1), // 1
                (1, 0, 2), // bb
                (1, 3, 1), // =
                (1, 5, 2), // 22
            ]
        );
    }

    const THREE_ENTITIES: &str = "a = {\n  x = 1\n}\nb = {\n  y = 2\n}\nc = {\n  z = 3\n}\n";

    #[test]
    fn a_span_keeps_only_the_entities_it_covers() {
        let tokens = tokens_in_span(THREE_ENTITIES, (4, 6));
        assert!(
            tokens.iter().all(|t| (3..=5).contains(&t.line)),
            "tokens outside the span leaked: {tokens:#?}"
        );
        assert_eq!(tokens.len(), 5, "{tokens:#?}");
    }

    #[test]
    fn a_span_produces_the_same_tokens_full_would() {
        let full = tokens_for(THREE_ENTITIES);
        let ranged = tokens_in_span(THREE_ENTITIES, (4, 6));
        let expected: Vec<AbsToken> = full
            .into_iter()
            .filter(|t| (3..=5).contains(&t.line))
            .collect();
        assert_eq!(ranged, expected);
    }

    #[test]
    fn a_span_inside_an_entity_still_gets_its_lines() {
        let tokens = tokens_in_span(THREE_ENTITIES, (5, 5));
        assert!(
            tokens.iter().all(|t| t.line == 4),
            "only the requested line: {tokens:#?}"
        );
        assert_eq!(tokens.len(), 3, "{tokens:#?}");
    }

    // ── Position encoding ────────────────────────────────────────────────────

    #[test]
    fn utf16_columns_and_lengths_count_code_units() {
        // 😀 is one char to the parser but two UTF-16 code units, so every
        let text = "a = 😀\n";
        let table = StringTable::new();
        let ast = cwtools_parser::parser::parse_string(text, &table);
        let tokens = semantic_tokens(&ast, &table, text, &PositionEncodingKind::UTF16, None, None);
        let value = tokens.iter().find(|t| t.token_type == TY_STRING).unwrap();
        assert_eq!((value.start, value.length), (4, 2));
    }

    #[test]
    fn utf32_columns_and_lengths_count_scalars() {
        let text = "a = 😀\n";
        let table = StringTable::new();
        let ast = cwtools_parser::parser::parse_string(text, &table);
        let tokens = semantic_tokens(&ast, &table, text, &PositionEncodingKind::UTF32, None, None);
        let value = tokens.iter().find(|t| t.token_type == TY_STRING).unwrap();
        assert_eq!((value.start, value.length), (4, 1));
    }

    #[test]
    fn a_non_bmp_char_before_a_token_shifts_its_utf16_column() {
        let text = "😀 = 1\n";
        let table = StringTable::new();
        let ast = cwtools_parser::parser::parse_string(text, &table);
        let tokens = semantic_tokens(&ast, &table, text, &PositionEncodingKind::UTF16, None, None);
        let number = tokens.iter().find(|t| t.token_type == TY_NUMBER).unwrap();
        assert_eq!(number.start, 5, "char col 4 is UTF-16 col 5");
    }

    const PAST_EQ: usize = 3;

    #[test]
    fn value_span_skips_whitespace_and_stops_at_a_comment() {
        let line: Vec<char> = "a =   b # c".chars().collect();
        assert_eq!(value_token_span(&line, PAST_EQ), Some((6, 7)));
    }

    #[test]
    fn value_span_is_none_at_a_clause_or_end_of_line() {
        let brace: Vec<char> = "a = {".chars().collect();
        assert_eq!(value_token_span(&brace, PAST_EQ), None);
        let bare: Vec<char> = "a =".chars().collect();
        assert_eq!(value_token_span(&bare, PAST_EQ), None);
        let comment: Vec<char> = "a = # x".chars().collect();
        assert_eq!(value_token_span(&comment, PAST_EQ), None);
    }

    #[test]
    fn value_span_covers_a_quoted_string_including_spaces() {
        let line: Vec<char> = r#"a = "two words" # c"#.chars().collect();
        assert_eq!(value_token_span(&line, PAST_EQ), Some((4, 15)));
    }

    #[test]
    fn operator_lookup_starts_after_the_key() {
        let line: Vec<char> = "a = b".chars().collect();
        assert_eq!(find_token_col(&line, "=", 1), Some(2));
        assert_eq!(find_token_col(&line, "!=", 1), None);
    }

    const DOC_URI: &str = if cfg!(windows) {
        "file:///C:/ws/common/ideas/00_ideas.txt"
    } else {
        "file:///ws/common/ideas/00_ideas.txt"
    };

    fn backend_with_open_doc() -> Backend {
        let state = std::sync::Arc::new(crate::state::DocumentState::new());
        let captured = std::sync::Arc::new(parking_lot::Mutex::new(None));
        let slot = captured.clone();
        let server_state = state.clone();
        let (_service, _socket) = tower_lsp::LspService::new(move |client| {
            *slot.lock() = Some(client.clone());
            Backend {
                client,
                state: server_state.clone(),
            }
        });
        let client = captured.lock().take().unwrap();
        state
            .documents
            .lock()
            .open(
                DOC_URI.to_string(),
                crate::state::ParsedDoc {
                    version: 1,
                    text: std::sync::Arc::from("idea = { cost = 1 }"),
                    ast: None,
                    ast_version: None,
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .unwrap();
        Backend { client, state }
    }

    fn full_params() -> SemanticTokensParams {
        SemanticTokensParams {
            text_document: TextDocumentIdentifier {
                uri: Url::parse(DOC_URI).unwrap(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    /// A self-deadlock wedges the thread that hit it, so the check cannot ride
    fn must_finish(name: &str, f: impl FnOnce() + Send + 'static) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            f();
            let _ = tx.send(());
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_secs(30)).is_ok(),
            "{name} deadlocked on the semantic-token cache mutex"
        );
    }

    // went silent mid-scan with no error and no further output (#334).
    #[test]
    fn repeat_full_request_does_not_deadlock_on_the_cache() {
        must_finish("semantic_tokens_full", || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let backend = backend_with_open_doc();
                backend
                    .semantic_tokens_full_impl(full_params())
                    .await
                    .unwrap();
                backend
                    .semantic_tokens_full_impl(full_params())
                    .await
                    .unwrap();
            });
        });
    }

    #[test]
    fn repeat_delta_request_does_not_deadlock_on_the_cache() {
        must_finish("semantic_tokens_full_delta", || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let backend = backend_with_open_doc();
                let first = backend
                    .semantic_tokens_full_impl(full_params())
                    .await
                    .unwrap();
                let previous_result_id = match first {
                    Some(SemanticTokensResult::Tokens(tokens)) => tokens.result_id.unwrap(),
                    other => panic!("expected full tokens, got {other:?}"),
                };
                backend
                    .semantic_tokens_full_delta_impl(SemanticTokensDeltaParams {
                        text_document: TextDocumentIdentifier {
                            uri: Url::parse(DOC_URI).unwrap(),
                        },
                        previous_result_id,
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                    })
                    .await
                    .unwrap();
            });
        });
    }
}
