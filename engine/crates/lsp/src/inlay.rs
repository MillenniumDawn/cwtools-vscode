//!   suppressed. This avoids the rejected per-leaf resolver path from issue #99.

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{InlayHint, InlayHintLabel, InlayHintParams, Position, Range};

use cwtools_game::scope_engine::{SCOPE_ANY, SCOPE_INVALID, ScopeId};
use cwtools_game::scope_registry::ScopeRegistry;
use cwtools_info::TypeIndex;
use cwtools_parser::ast::{Arena, Child, ParsedFile, SourceRange, Value};
use cwtools_string_table::string_table::StringTable;

use crate::lines::DocLines;
use crate::navigation::{value_col_in_line, value_start_after_eq};
use crate::{Backend, LocTextMap};

const MAX_HINTS: usize = 200;

const MAX_TITLE_CHARS: usize = 60;

impl Backend {
    pub(crate) async fn inlay_hint_impl(
        &self,
        params: InlayHintParams,
    ) -> Result<Option<Vec<InlayHint>>> {
        use std::sync::atomic::Ordering::Relaxed;
        let loc_titles = self.state.inlay_hints_loc_titles.load(Relaxed);
        let scopes = self.state.inlay_hints_scopes.load(Relaxed);
        if !loc_titles && !scopes {
            return Ok(None);
        }
        let uri = params.text_document.uri.to_string();
        if crate::paths::has_loc_ext(&uri) || crate::paths::is_cwt_file(&uri) {
            return Ok(None);
        }
        let Some(ast) = self.ast_for(&uri) else {
            return Ok(None);
        };
        let Some(text) = self.file_text_for(&uri).await else {
            return Ok(None);
        };
        let encoding = self.position_encoding();
        let ws_prefix = self.state.config.read().workspace_prefix.clone();
        let logical_path = crate::paths::logical_path_from_uri(&uri, &ws_prefix);
        let lines = DocLines::new(&text, encoding);
        let loc_hints = if loc_titles {
            // Lock order: info_service -> loc_text (documents already released).
            let info = self.state.info_service.read();
            let loc_text = self.state.loc_text.read();
            loc_title_hints(
                &ast,
                &self.state.string_table,
                &lines,
                params.range,
                &info.type_index,
                &loc_text,
            )
        } else {
            Vec::new()
        };
        let mut scope_hints = Vec::new();
        if scopes {
            let rules_guard = self.state.rules.read();
            let info_guard = self.state.info_service.read();
            let inline_guard = self.state.inline_scripts.read();
            if let (Some(ruleset), Some(registry)) = (
                rules_guard.ruleset.as_ref(),
                rules_guard.scope_registry.as_ref(),
            ) {
                let (game, scope_checks, var_checks) = {
                    let cfg = self.state.config.read();
                    (cfg.game(), cfg.scope_checks, cfg.var_checks)
                };
                let prepared = crate::validate::make_prepared(
                    ruleset,
                    &self.state.string_table,
                    game,
                    &info_guard.type_index,
                    rules_guard.modifier_keys.as_ref(),
                    None,
                    None,
                    Some(&inline_guard),
                    Some(registry),
                    scope_checks,
                    var_checks,
                );
                let (start_line, end_line) = source_line_bounds(params.range);
                let transitions = cwtools_validation::position::scope_transitions_with_limit(
                    &ast,
                    &logical_path,
                    &prepared,
                    start_line,
                    end_line,
                    MAX_HINTS,
                );
                scope_hints =
                    scope_hints_from_transitions(&transitions, &lines, registry, params.range);
            }
        }
        let hints = merge_hints(loc_hints, scope_hints);
        if hints.is_empty() {
            Ok(None)
        } else {
            Ok(Some(hints))
        }
    }
}

fn source_line_bounds(range: Range) -> (u32, u32) {
    (
        range.start.line.saturating_add(1),
        range.end.line.saturating_add(2),
    )
}

fn merge_hints(mut first: Vec<InlayHint>, mut second: Vec<InlayHint>) -> Vec<InlayHint> {
    first.append(&mut second);
    first.sort_by_key(|hint| (hint.position.line, hint.position.character));
    first.truncate(MAX_HINTS);
    first
}

pub(crate) fn loc_title_hints(
    file: &ParsedFile,
    table: &StringTable,
    lines: &DocLines,
    range: Range,
    type_index: &TypeIndex,
    loc_text: &LocTextMap,
) -> Vec<InlayHint> {
    let mut hints = Vec::new();
    let cx = Ctx {
        arena: &file.arena,
        table,
        lines,
        range,
        type_index,
        loc_text,
    };
    collect_hints(&file.root_children, &cx, &mut hints);
    hints
}

struct Ctx<'a> {
    arena: &'a Arena,
    table: &'a StringTable,
    lines: &'a DocLines<'a>,
    range: Range,
    type_index: &'a TypeIndex,
    loc_text: &'a LocTextMap,
}

impl Ctx<'_> {
    fn line(&self, line0: u32) -> &str {
        self.lines.line(line0)
    }

    fn line_in_range(&self, line0: u32) -> bool {
        line0 >= self.range.start.line && line0 <= self.range.end.line
    }

    fn position_in_range(&self, position: Position) -> bool {
        position_in_half_open_range(position, self.range)
    }
}

fn position_in_half_open_range(position: Position, range: Range) -> bool {
    let start = range.start;
    let end = range.end;
    (position.line > start.line
        || (position.line == start.line && position.character >= start.character))
        && (position.line < end.line
            || (position.line == end.line && position.character < end.character))
}

fn collect_hints(children: &[Child], cx: &Ctx<'_>, out: &mut Vec<InlayHint>) {
    for child in children {
        if out.len() >= MAX_HINTS {
            return;
        }
        match child {
            Child::Leaf(idx) => {
                let leaf = &cx.arena.leaves[*idx as usize];
                if let Value::Clause(inner) = &leaf.value {
                    let start0 = leaf.pos.start.line.saturating_sub(1);
                    let end0 = leaf.pos.end.line.saturating_sub(1);
                    if end0 >= cx.range.start.line && start0 <= cx.range.end.line {
                        collect_hints(inner, cx, out);
                    }
                    continue;
                }
                let line0 = leaf.pos.start.line.saturating_sub(1);
                if !cx.line_in_range(line0) {
                    continue;
                }
                if let Some(hint) = hint_for_value(
                    &leaf.value,
                    cx,
                    line0,
                    Anchor::Keyed(leaf.pos.start.col as u32),
                ) && cx.position_in_range(hint.position)
                {
                    out.push(hint);
                }
            }
            Child::LeafValue(idx) => {
                let lv = &cx.arena.leaf_values[*idx as usize];
                let line0 = lv.pos.start.line.saturating_sub(1);
                if !cx.line_in_range(line0) {
                    continue;
                }
                if let Some(hint) =
                    hint_for_value(&lv.value, cx, line0, Anchor::Bare(lv.pos.start.col as u32))
                    && cx.position_in_range(hint.position)
                {
                    out.push(hint);
                }
            }
            Child::Comment(_) => {}
        }
    }
}

fn scope_hints_from_transitions(
    transitions: &[cwtools_validation::position::ScopeTransition],
    lines: &DocLines,
    registry: &ScopeRegistry,
    request_range: Range,
) -> Vec<InlayHint> {
    transitions
        .iter()
        .filter_map(|transition| {
            let hint = scope_hint_for_block(
                registry,
                transition.ambient,
                transition.resolved,
                transition.range,
                lines,
            )?;
            position_in_half_open_range(hint.position, request_range).then_some(hint)
        })
        .collect()
}

pub(crate) fn scope_hint_for_block(
    registry: &ScopeRegistry,
    ambient_scope: ScopeId,
    resolved_scope: ScopeId,
    range: SourceRange,
    lines: &DocLines,
) -> Option<InlayHint> {
    if resolved_scope == ambient_scope {
        return None;
    }
    if !is_real_scope(registry, resolved_scope) {
        return None;
    }
    let line0 = range.start.line.saturating_sub(1);
    let key_col = range.start.col as u32;
    let line = lines.line(line0);
    let after_eq = value_start_after_eq(line, key_col).unwrap_or(key_col);
    let col = skip_whitespace(line, after_eq).unwrap_or(after_eq);
    let position = lines.position(line0, col);
    let label = format!("\u{2192} {}", registry.name_of(resolved_scope));
    Some(InlayHint {
        position,
        label: InlayHintLabel::String(label),
        kind: None,
        text_edits: None,
        tooltip: None,
        padding_left: Some(true),
        padding_right: None,
        data: None,
    })
}

fn is_real_scope(registry: &ScopeRegistry, scope: ScopeId) -> bool {
    scope != SCOPE_ANY && scope != SCOPE_INVALID && registry.by_id.contains_key(&scope)
}

fn skip_whitespace(line: &str, from: u32) -> Option<u32> {
    let from = from as usize;
    line.chars()
        .enumerate()
        .skip(from)
        .find(|(_, c)| !c.is_whitespace())
        .map(|(i, _)| i as u32)
}

enum Anchor {
    Keyed(u32),
    Bare(u32),
}

fn hint_for_value(value: &Value, cx: &Ctx<'_>, line0: u32, anchor: Anchor) -> Option<InlayHint> {
    let id = match value {
        Value::String(t) | Value::QString(t) => t.normal,
        _ => return None,
    };
    cx.table
        .with_string(id, |s| {
            let name = s.trim_matches('"');
            if name.is_empty() || !cx.type_index.is_any_instance(name) {
                return None;
            }
            let name_lc = name.to_ascii_lowercase();
            let title = truncate_title(cx.loc_text.get(name_lc.as_str())?.first()?.1.as_str());

            let line = cx.line(line0);
            let from = match anchor {
                Anchor::Keyed(key_col) => value_start_after_eq(line, key_col).unwrap_or(key_col),
                Anchor::Bare(col) => col,
            };
            let col = value_col_in_line(line, name, from)?;
            let end_col = col + name.chars().count() as u32;
            let position = cx.lines.position(line0, end_col);

            Some(InlayHint {
                position,
                label: InlayHintLabel::String(title),
                kind: None,
                text_edits: None,
                tooltip: None,
                padding_left: Some(true),
                padding_right: None,
                data: None,
            })
        })
        .flatten()
}

fn truncate_title(title: &str) -> String {
    if title.chars().count() <= MAX_TITLE_CHARS {
        return title.to_string();
    }
    let mut s: String = title.chars().take(MAX_TITLE_CHARS).collect();
    s.push('…');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_lsp::lsp_types::PositionEncodingKind;

    use cwtools_game::scope_registry::ScopeDefOwned;
    use cwtools_info::{SourceLocation, TypeInstance};
    use cwtools_localization::Lang;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tower_lsp::lsp_types::Position;

    fn idx_with(type_name: &str, names: &[&str]) -> TypeIndex {
        let mut idx = TypeIndex::new();
        let mut per_type = HashMap::new();
        per_type.insert(
            type_name.to_string(),
            names
                .iter()
                .map(|n| TypeInstance {
                    name: n.to_string(),
                    location: SourceLocation {
                        line: 1,
                        col: 0,
                        end: (1, 0),
                    },
                    primary_loc_key: None,
                    required_loc_keys: Vec::new(),
                })
                .collect(),
        );
        idx.merge("file:///mod/x.txt", per_type);
        idx
    }

    fn loc(pairs: &[(&str, &str)]) -> LocTextMap {
        pairs
            .iter()
            .map(|(k, v)| (Arc::from(*k), vec![(Lang::English, v.to_string())]))
            .collect()
    }

    fn hints_for(text: &str, idx: &TypeIndex, loc: &LocTextMap) -> Vec<InlayHint> {
        let table = StringTable::new();
        let ast = cwtools_parser::parser::parse_string(text, &table);
        let range = Range::new(Position::new(0, 0), Position::new(1000, 0));
        let lines = DocLines::new(text, PositionEncodingKind::UTF16);
        loc_title_hints(&ast, &table, &lines, range, idx, loc)
    }

    fn label(h: &InlayHint) -> &str {
        match &h.label {
            InlayHintLabel::String(s) => s,
            _ => panic!("expected a string label"),
        }
    }

    #[test]
    fn value_with_title_gets_a_hint_after_the_value() {
        let text = "c = {\n    add_ideas = my_idea\n}\n";
        let idx = idx_with("idea", &["my_idea"]);
        let loc = loc(&[("my_idea", "My Idea")]);
        let hints = hints_for(text, &idx, &loc);
        assert_eq!(hints.len(), 1, "one titled id -> one hint");
        assert_eq!(label(&hints[0]), "My Idea");
        assert_eq!(hints[0].padding_left, Some(true));
        assert_eq!(hints[0].position, Position::new(1, 23));
    }

    #[test]
    fn bare_leaf_value_in_a_list_gets_a_hint() {
        let text = "list = {\n    my_idea\n}\n";
        let idx = idx_with("idea", &["my_idea"]);
        let loc = loc(&[("my_idea", "My Idea")]);
        let hints = hints_for(text, &idx, &loc);
        assert_eq!(hints.len(), 1);
        assert_eq!(label(&hints[0]), "My Idea");
        assert_eq!(hints[0].position, Position::new(1, 11));
    }

    #[test]
    fn known_id_without_a_title_gets_no_hint() {
        let text = "c = {\n    add_ideas = my_idea\n}\n";
        let idx = idx_with("idea", &["my_idea"]);
        let loc = loc(&[]);
        assert!(hints_for(text, &idx, &loc).is_empty());
    }

    #[test]
    fn loc_key_that_is_not_a_known_instance_gets_no_hint() {
        let text = "c = {\n    set_country_flag = my_idea\n}\n";
        let idx = TypeIndex::new();
        let loc = loc(&[("my_idea", "My Idea")]);
        assert!(hints_for(text, &idx, &loc).is_empty());
    }

    #[test]
    fn quoted_value_resolves_the_inner_token() {
        let text = "c = {\n    name = \"my_idea\"\n}\n";
        let idx = idx_with("idea", &["my_idea"]);
        let loc = loc(&[("my_idea", "My Idea")]);
        let hints = hints_for(text, &idx, &loc);
        assert_eq!(hints.len(), 1);
        assert_eq!(label(&hints[0]), "My Idea");
    }

    #[test]
    fn case_insensitive_id_and_key() {
        let text = "c = {\n    add_ideas = MY_IDEA\n}\n";
        let idx = idx_with("idea", &["my_idea"]);
        let loc = loc(&[("my_idea", "My Idea")]);
        let hints = hints_for(text, &idx, &loc);
        assert_eq!(hints.len(), 1);
        assert_eq!(label(&hints[0]), "My Idea");
    }

    #[test]
    fn out_of_range_leaves_are_skipped() {
        let text = "a = my_idea\nb = my_idea\nc = my_idea\n";
        let idx = idx_with("idea", &["my_idea"]);
        let loc = loc(&[("my_idea", "My Idea")]);
        let table = StringTable::new();
        let ast = cwtools_parser::parser::parse_string(text, &table);
        let range = Range::new(Position::new(1, 0), Position::new(1, 100));
        let lines = DocLines::new(text, PositionEncodingKind::UTF16);
        let hints = loc_title_hints(&ast, &table, &lines, range, &idx, &loc);
        assert_eq!(hints.len(), 1, "only the in-range leaf is hinted");
        assert_eq!(hints[0].position.line, 1);
    }

    #[test]
    fn hint_count_is_capped() {
        let n = MAX_HINTS + 50;
        let mut text = String::from("list = {\n");
        for _ in 0..n {
            text.push_str("    my_idea\n");
        }
        text.push_str("}\n");
        let idx = idx_with("idea", &["my_idea"]);
        let loc = loc(&[("my_idea", "My Idea")]);
        let hints = hints_for(&text, &idx, &loc);
        assert_eq!(hints.len(), MAX_HINTS);
    }

    #[test]
    fn long_title_is_truncated() {
        let long = "x".repeat(MAX_TITLE_CHARS + 10);
        let text = "c = {\n    add_ideas = my_idea\n}\n";
        let idx = idx_with("idea", &["my_idea"]);
        let loc = loc(&[("my_idea", long.as_str())]);
        let hints = hints_for(text, &idx, &loc);
        assert_eq!(hints.len(), 1);
        let rendered = label(&hints[0]);
        assert!(rendered.ends_with('…'));
        assert_eq!(rendered.chars().count(), MAX_TITLE_CHARS + 1);
    }

    fn scope_registry_for_tests() -> (ScopeRegistry, ScopeId, ScopeId) {
        let mut registry = ScopeRegistry::default();
        let country = ScopeId(100);
        let character = ScopeId(101);
        registry.by_id.insert(
            country,
            ScopeDefOwned {
                name: "Country".to_string(),
                aliases: vec!["country".to_string()],
                subscope_of: Vec::new(),
            },
        );
        registry.by_id.insert(
            character,
            ScopeDefOwned {
                name: "Character".to_string(),
                aliases: vec!["character".to_string()],
                subscope_of: vec![country],
            },
        );
        (registry, country, character)
    }

    #[test]
    fn scope_hint_uses_block_brace_position_and_resolved_name() {
        let text = "owner = {\n}\n";
        let table = StringTable::new();
        let ast = cwtools_parser::parser::parse_string(text, &table);
        let Child::Leaf(idx) = ast.root_children[0] else {
            panic!("expected a keyed clause");
        };
        let (registry, country, character) = scope_registry_for_tests();
        let lines = DocLines::new(text, PositionEncodingKind::UTF16);
        let hint = scope_hint_for_block(
            &registry,
            country,
            character,
            ast.arena.leaves[idx as usize].pos,
            &lines,
        )
        .expect("different real scopes produce a hint");
        assert_eq!(label(&hint), "→ character");
        assert_eq!(hint.position, Position::new(0, 8));
        assert_eq!(hint.padding_left, Some(true));
        assert_eq!(hint.padding_right, None);
        assert!(hint.tooltip.is_none());
        assert!(hint.text_edits.is_none());
    }

    #[test]
    fn scope_hint_range_is_half_open() {
        let text = "owner = { }\n";
        let table = StringTable::new();
        let ast = cwtools_parser::parser::parse_string(text, &table);
        let Child::Leaf(idx) = ast.root_children[0] else {
            panic!("expected a keyed clause");
        };
        let (registry, country, character) = scope_registry_for_tests();
        let range = ast.arena.leaves[idx as usize].pos;
        let lines = DocLines::new(text, PositionEncodingKind::UTF16);
        let hint = scope_hint_for_block(&registry, country, character, range, &lines)
            .expect("scope hint should render");
        assert!(position_in_half_open_range(
            hint.position,
            Range::new(Position::new(0, 0), Position::new(0, 9),)
        ));
        assert!(!position_in_half_open_range(
            hint.position,
            Range::new(Position::new(0, 0), Position::new(0, 8),)
        ));
    }

    #[test]
    fn scope_hint_suppresses_ambient_and_placeholder_scopes() {
        let text = "owner = { }\n";
        let table = StringTable::new();
        let ast = cwtools_parser::parser::parse_string(text, &table);
        let Child::Leaf(idx) = ast.root_children[0] else {
            panic!("expected a keyed clause");
        };
        let (registry, country, character) = scope_registry_for_tests();
        let leaf = &ast.arena.leaves[idx as usize];
        let lines = DocLines::new(text, PositionEncodingKind::UTF16);
        assert!(scope_hint_for_block(&registry, country, country, leaf.pos, &lines).is_none());
        assert!(scope_hint_for_block(&registry, country, SCOPE_ANY, leaf.pos, &lines).is_none());
        assert!(scope_hint_for_block(&registry, country, ScopeId(999), leaf.pos, &lines).is_none());
        assert!(is_real_scope(&registry, character));
    }

    #[test]
    fn numeric_and_bool_values_are_ignored() {
        let text = "c = {\n    cost = 5\n    flag = yes\n}\n";
        let mut idx = idx_with("idea", &["5"]);
        {
            let mut pt = HashMap::new();
            pt.insert(
                "idea".to_string(),
                vec![TypeInstance {
                    name: "true".to_string(),
                    location: SourceLocation {
                        line: 1,
                        col: 0,
                        end: (1, 0),
                    },
                    primary_loc_key: None,
                    required_loc_keys: Vec::new(),
                }],
            );
            idx.merge("file:///mod/y.txt", pt);
        }
        let loc = loc(&[("5", "Five"), ("true", "True")]);
        assert!(hints_for(text, &idx, &loc).is_empty());
    }
}
