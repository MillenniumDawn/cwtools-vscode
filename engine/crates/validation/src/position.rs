//! Position-targeted rule resolution for editor features (completion, hover,
//! goto-definition).
//!
//! [`rules_at_pos`] mirrors the validator's descent (`validate_prepared` →
//! `validate_with_type` → `validate_children`) but follows only the branch that
//! contains the cursor and returns the applicable rules instead of emitting
//! errors. It shares the validator's matching machinery (`matching_candidates`,
//! `alias_overloads`, `merged_rules_for_type`, `flatten_nested_subtype_rules`)
//! so the two can't disagree about what a key resolves to. The entry walk over
//! root children intentionally mirrors `validate_prepared` (lib.rs) — keep the
//! two in step when changing either.

use cwtools_game::scope_engine::{ScopeContext, ScopeId};
use cwtools_parser::ast::{Child, ParsedFile, SourcePos, SourceRange, Value};
use cwtools_rules::rules_types::*;

use crate::common::{leaf_value_to_string, unquote_key};
use crate::ctx::{AliasBranchBudget, ValidationCtx};
use crate::resolve::{
    DispatchInput, PathCandidate, ResolvedType, find_rules_by_name, find_type_from_candidates,
    grandchild_candidates_for_wrapper, path_candidates_for_file, refine_grandchild_type,
    resolve_root_child, type_has_content,
};
use crate::rule_core::{
    alias_overloads, alias_overloads_with_confidence, flatten_nested_subtype_rules,
    matching_candidates, merged_rules_for_type, rule_matches_leaf_key,
};
use crate::scope::{enter_block_scope, seed_root_scope};
use crate::{Prepared, initial_scope_context};
use std::cell::RefCell;
use std::sync::Arc;

/// A scope-changing block discovered by [`scope_transitions`]. Positions use
/// parser coordinates: lines are 1-based and columns are 0-based.
#[derive(Debug, Clone, Copy)]
pub struct ScopeTransition {
    pub range: SourceRange,
    pub ambient: ScopeId,
    pub resolved: ScopeId,
}

/// The leaf under the cursor, when the cursor sits on a `key = value` line
/// rather than at a block insert position.
#[derive(Debug, Clone)]
pub struct LeafAtPos {
    pub key: String,
    /// Raw value text; empty for clause values.
    pub value: String,
    /// True when the cursor is on the value side of the `=`.
    pub in_value: bool,
    pub line: u32,
    pub col: u16,
}

/// The rules applicable at a cursor position.
#[derive(Debug, Clone, Default)]
pub struct RuleContext {
    /// Rules for NEW keys in the innermost block containing the cursor
    /// (subtype-merged and nested-subtype-flattened; `AliasField` lefts are NOT
    /// pre-expanded — completion enumerates the category's aliases itself).
    pub child_rules: Vec<(RuleType, Options)>,
    /// When the cursor is on a leaf: every matched rule for that leaf.
    /// `AliasField` matches are expanded to their alias-body overloads, so
    /// `has_completed_focus = X` yields the `LeafRule` whose right side is
    /// `TypeField("focus")`.
    pub value_rules: Vec<(RuleType, Options)>,
    pub leaf: Option<LeafAtPos>,
    /// Scope context at the cursor (None when no game/registry).
    pub scope: Option<ScopeContext>,
}

fn pos_in_range(line: u32, col: u16, range: &SourceRange) -> bool {
    let target = SourcePos { line, col };
    let (s, e) = (&range.start, &range.end);
    if target.line < s.line || target.line > e.line {
        return false;
    }
    if target.line == s.line && target.col < s.col {
        return false;
    }
    if target.line == e.line && target.col > e.col {
        return false;
    }
    true
}

/// Resolve the rules applicable at `(line, col)` (parser coordinates: `line` is
/// 1-based, `col` is 0-based).
///
/// Returns `None` when the position is outside any known entity — at the file
/// top level, in a file no type covers, or under an index-only type with no
/// rule body. Callers fall back to their generic behavior (e.g. root-type
/// snippets) in that case.
///
/// `for_completion` opts into the subtype union in `merged_rules_for_type`:
/// completion offers every subtype's fields, while hover/goto pass `false` to
/// mirror validation exactly.
#[tracing::instrument(skip_all, fields(line, col))]
pub fn rules_at_pos(
    ast: &ParsedFile,
    file_path: &str,
    prepared: &Prepared,
    line: u32,
    col: u16,
    for_completion: bool,
) -> Option<RuleContext> {
    let ruleset = prepared.ruleset;
    let table = prepared.table;
    let mut scope_context = initial_scope_context(file_path, prepared.registry);
    // The resolver discards the diagnostics it produces, but the shared context
    // still wants the run's shared path.
    let file_arc: crate::FilePath = std::sync::Arc::from(file_path);
    let alias_branch_budget = std::cell::RefCell::new(AliasBranchBudget::default());
    let inline_stack = std::cell::RefCell::new(Vec::new());
    let ctx = ValidationCtx {
        ast,
        ruleset,
        table,
        file_path: &file_arc,
        game: prepared.game,
        type_index: prepared.type_index,
        modifier_keys: prepared.modifier_keys,
        loc_index: prepared.loc_index,
        extra_loc_keys: prepared.extra_loc_keys,
        inline_scripts: prepared.inline_scripts,
        scope_checks: prepared.scope_checks,
        var_checks: prepared.var_checks,
        loop_vars: std::cell::RefCell::new(Vec::new()),
        alias_branch_budget: &alias_branch_budget,
        inline_stack: &inline_stack,
        alias_memo: std::cell::RefCell::new(crate::ctx::AliasMemo::default()),
        // The resolver is a read-only navigation walk; it never contributes to
        // the project-wide unused check.
        type_uses: None,
    };

    // Path candidates depend only on the file path, so compute them once and
    // reuse below for both the type_per_file check and the root-child dispatch,
    // rather than rescanning `ruleset.types` twice.
    let file_path_lower = file_path.to_lowercase();
    let path_candidates = path_candidates_for_file(&file_path_lower, ruleset);

    // type_per_file: the whole file is one instance; root children are its body.
    let path_type = find_type_from_candidates(&path_candidates, None);
    if let Some(td) = path_type
        && td.type_per_file
    {
        let inner_rules = find_rules_by_name(&td.name, ruleset);
        if !type_has_content(td, inner_rules) {
            return None;
        }
        return Some(enter_entity(
            &ctx,
            td,
            &ast.root_children,
            inner_rules,
            None,
            &mut scope_context,
            line,
            col,
            for_completion,
        ));
    }

    // Find the root child containing the position.
    let child = ast.root_children.iter().find(|c| match c {
        Child::Leaf(idx) => pos_in_range(line, col, &ast.arena.leaves[*idx as usize].pos),
        Child::LeafValue(idx) => pos_in_range(line, col, &ast.arena.leaf_values[*idx as usize].pos),
        _ => false,
    })?;

    let Child::Leaf(leaf_idx) = child else {
        return None;
    };
    let leaf = &ast.arena.leaves[*leaf_idx as usize];
    let Value::Clause(children) = &leaf.value else {
        return None;
    };
    let root_key = table.get_string(leaf.key.normal).unwrap_or_default();
    // Cursor on the root key itself (`my_focus| = { ... }`): top-level context,
    // not inside the entity. Columns are char counts (see parser), so measure the
    // key in chars, not bytes.
    if line == leaf.pos.start.line
        && (col as usize) <= leaf.pos.start.col as usize + root_key.chars().count()
    {
        return None;
    }

    // Resolve which type owns this root node (exact root-key match, then path
    // fallback) via the shared dispatch, then descend toward the cursor.
    // Navigation opts into the content-bearing fallback (`allow_content_fallback`)
    // so the cursor can still descend through a rule-less skip wrapper whose body
    // lives in a sibling base type (e.g. `on_actions` -> `on_action`).
    let dispatch = DispatchInput {
        ruleset,
        file_path,
        path_candidates: &path_candidates,
        allow_content_fallback: true,
    };
    match resolve_root_child(&dispatch, &root_key) {
        ResolvedType::Entity {
            type_def,
            inner_rules,
        } => Some(enter_entity(
            &ctx,
            type_def,
            children,
            inner_rules,
            Some(&root_key),
            &mut scope_context,
            line,
            col,
            for_completion,
        )),
        ResolvedType::Wrapper {
            type_def,
            inner_rules,
            skip_tail,
        } => descend_wrapper(
            &ctx,
            children,
            &path_candidates,
            type_def,
            &root_key,
            inner_rules,
            skip_tail,
            &mut scope_context,
            line,
            col,
            for_completion,
        ),
        ResolvedType::None => None,
    }
}

/// Collect scope transitions in one validator-shaped downward walk. Unlike
/// `rules_at_pos`, this visits each block once and threads one mutable
/// `ScopeContext` through the tree, so a range request does not rebuild a
/// `Prepared` context for every candidate leaf.
#[allow(clippy::too_many_arguments)]
pub fn scope_transitions(
    ast: &ParsedFile,
    file_path: &str,
    prepared: &Prepared<'_>,
    start_line: u32,
    end_line: u32,
) -> Vec<ScopeTransition> {
    scope_transitions_with_limit(ast, file_path, prepared, start_line, end_line, usize::MAX)
}

pub fn scope_transitions_with_limit(
    ast: &ParsedFile,
    file_path: &str,
    prepared: &Prepared<'_>,
    start_line: u32,
    end_line: u32,
    max_transitions: usize,
) -> Vec<ScopeTransition> {
    if max_transitions == 0 {
        return Vec::new();
    }
    let Some(mut scope_context) = initial_scope_context(file_path, prepared.registry) else {
        return Vec::new();
    };
    let file_arc: crate::FilePath = Arc::from(file_path);
    let alias_branch_budget = RefCell::new(AliasBranchBudget::default());
    let inline_stack = RefCell::new(Vec::new());
    let ctx = ValidationCtx {
        ast,
        ruleset: prepared.ruleset,
        table: prepared.table,
        file_path: &file_arc,
        game: prepared.game,
        type_index: prepared.type_index,
        modifier_keys: prepared.modifier_keys,
        loc_index: prepared.loc_index,
        extra_loc_keys: prepared.extra_loc_keys,
        inline_scripts: prepared.inline_scripts,
        scope_checks: prepared.scope_checks,
        var_checks: prepared.var_checks,
        loop_vars: RefCell::new(Vec::new()),
        alias_branch_budget: &alias_branch_budget,
        inline_stack: &inline_stack,
        alias_memo: RefCell::new(crate::ctx::AliasMemo::default()),
        type_uses: None,
    };
    let path_candidates = path_candidates_for_file(&file_path.to_lowercase(), prepared.ruleset);
    let mut out = Vec::new();

    if let Some(type_def) = find_type_from_candidates(&path_candidates, None)
        && type_def.type_per_file
    {
        let inner_rules = find_rules_by_name(&type_def.name, prepared.ruleset);
        collect_entity_scope(
            &ctx,
            type_def,
            &ast.root_children,
            inner_rules,
            None,
            None,
            &mut scope_context,
            start_line,
            end_line,
            max_transitions,
            &mut out,
        );
        return out;
    }

    let dispatch = DispatchInput {
        ruleset: prepared.ruleset,
        file_path,
        path_candidates: &path_candidates,
        allow_content_fallback: false,
    };
    for child in &ast.root_children {
        if out.len() >= max_transitions {
            break;
        }
        let Child::Leaf(idx) = child else { continue };
        let leaf = &ast.arena.leaves[*idx as usize];
        let Value::Clause(children) = &leaf.value else {
            continue;
        };
        let key = prepared
            .table
            .get_string(leaf.key.normal)
            .unwrap_or_default();
        match resolve_root_child(&dispatch, &key) {
            ResolvedType::Entity {
                type_def,
                inner_rules,
            } => collect_entity_scope(
                &ctx,
                type_def,
                children,
                inner_rules,
                Some(&key),
                Some(leaf.pos),
                &mut scope_context,
                start_line,
                end_line,
                max_transitions,
                &mut out,
            ),
            ResolvedType::Wrapper {
                type_def,
                inner_rules,
                skip_tail,
            } => collect_wrapper_scope(
                &ctx,
                children,
                &path_candidates,
                type_def,
                &key,
                inner_rules,
                skip_tail,
                &mut scope_context,
                start_line,
                end_line,
                max_transitions,
                &mut out,
            ),
            ResolvedType::None => {}
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn collect_entity_scope(
    ctx: &ValidationCtx<'_>,
    type_def: &TypeDefinition,
    children: &[Child],
    inner_rules: &[(RuleType, Options)],
    node_key: Option<&str>,
    node_range: Option<SourceRange>,
    scope_context: &mut ScopeContext,
    start_line: u32,
    end_line: u32,
    max_transitions: usize,
    out: &mut Vec<ScopeTransition>,
) {
    if node_range.is_some_and(|range| range.end.line < start_line) {
        return;
    }
    let (merged, matched, push_scope) =
        merged_rules_for_type(ctx, type_def, children, inner_rules, node_key, false);
    if matched.is_empty() && merged.is_empty() && node_range.is_some() {
        return;
    }
    let saved = scope_context.save();
    let ambient = scope_context.current();
    seed_root_scope(
        scope_context,
        type_def,
        push_scope,
        node_key,
        ctx.ruleset,
        ctx.game,
    );
    let resolved = scope_context.current();
    if let Some(range) = node_range
        && range.start.line >= start_line
        && range.start.line <= end_line
        && resolved != ambient
        && out.len() < max_transitions
    {
        out.push(ScopeTransition {
            range,
            ambient,
            resolved,
        });
    }
    collect_scope_children(
        ctx,
        children,
        merged.as_ref(),
        scope_context,
        start_line,
        end_line,
        max_transitions,
        out,
    );
    scope_context.restore(saved);
}

#[allow(clippy::too_many_arguments)]
fn collect_wrapper_scope(
    ctx: &ValidationCtx<'_>,
    grandchildren: &[Child],
    path_candidates: &[PathCandidate<'_>],
    type_def: &TypeDefinition,
    wrapper_root_key: &str,
    inner_rules: &[(RuleType, Options)],
    skip_tail: &[SkipRootKey],
    scope_context: &mut ScopeContext,
    start_line: u32,
    end_line: u32,
    max_transitions: usize,
    out: &mut Vec<ScopeTransition>,
) {
    let candidates = grandchild_candidates_for_wrapper(path_candidates, wrapper_root_key);
    for child in grandchildren {
        if out.len() >= max_transitions {
            return;
        }
        let (key, children, range) = match child {
            Child::Leaf(idx) => {
                let leaf = &ctx.ast.arena.leaves[*idx as usize];
                let Value::Clause(children) = &leaf.value else {
                    continue;
                };
                (
                    ctx.table.get_string(leaf.key.normal).unwrap_or_default(),
                    children.as_slice(),
                    leaf.pos,
                )
            }
            _ => continue,
        };
        if range.end.line < start_line {
            continue;
        }
        if let [next, rest @ ..] = skip_tail {
            if cwtools_index::skip_root_key_matches(next, &key) {
                collect_wrapper_scope(
                    ctx,
                    children,
                    path_candidates,
                    type_def,
                    &key,
                    inner_rules,
                    rest,
                    scope_context,
                    start_line,
                    end_line,
                    max_transitions,
                    out,
                );
            }
            continue;
        }
        let Some((child_type, child_rules)) =
            refine_grandchild_type(&candidates, &key, type_def, inner_rules, ctx.ruleset)
        else {
            continue;
        };
        collect_entity_scope(
            ctx,
            child_type,
            children,
            child_rules,
            Some(&key),
            Some(range),
            scope_context,
            start_line,
            end_line,
            max_transitions,
            out,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_scope_children(
    ctx: &ValidationCtx<'_>,
    children: &[Child],
    rules: &[(RuleType, Options)],
    scope_context: &mut ScopeContext,
    start_line: u32,
    end_line: u32,
    max_transitions: usize,
    out: &mut Vec<ScopeTransition>,
) {
    let flattened;
    let rules = if rules
        .iter()
        .any(|(rule, _)| matches!(rule, RuleType::SubtypeRule { .. }))
    {
        flattened = flatten_nested_subtype_rules(rules);
        flattened.as_slice()
    } else {
        rules
    };
    for child in children {
        if out.len() >= max_transitions {
            return;
        }
        let Child::Leaf(idx) = child else {
            if let Child::LeafValue(idx) = child {
                let lv = &ctx.ast.arena.leaf_values[*idx as usize];
                if let Value::Clause(inner) = &lv.value {
                    let next = valueclause_bodies(rules);
                    if !next.is_empty() {
                        collect_scope_children(
                            ctx,
                            inner,
                            &next,
                            scope_context,
                            start_line,
                            end_line,
                            max_transitions,
                            out,
                        );
                    }
                }
            }
            continue;
        };
        let leaf = &ctx.ast.arena.leaves[*idx as usize];
        if leaf.pos.start.line > end_line {
            return;
        }
        let Value::Clause(inner) = &leaf.value else {
            continue;
        };
        let raw_key = ctx.table.get_string(leaf.key.normal).unwrap_or_default();
        let key = unquote_key(&raw_key);
        let candidates = matching_candidates(
            rules,
            key,
            ctx.ruleset,
            ctx.type_index,
            rule_matches_leaf_key,
        );
        let mut next = Vec::new();
        let mut scope_options: Vec<(&Options, bool)> = Vec::new();
        let mut math_expression = false;
        let mut ambiguous_body = false;
        for (rule, opts) in candidates {
            match rule {
                RuleType::NodeRule {
                    left: NewField::AliasField(category),
                    ..
                }
                | RuleType::LeafRule {
                    left: NewField::AliasField(category),
                    ..
                } => {
                    let aliases = alias_overloads_with_confidence(
                        ctx.ruleset,
                        ctx.type_index,
                        category,
                        key,
                        false,
                    );
                    if aliases.len() > 1
                        && !ctx.reserve_alias_branches(
                            aliases.len(),
                            leaf.pos.start,
                            Some(leaf.pos.end),
                        )
                    {
                        return;
                    }
                    let mut first_node_alias = None;
                    for (alias, confident) in aliases {
                        if !confident {
                            continue;
                        }
                        if let RuleType::NodeRule { rules: body, .. } = &alias.0 {
                            if first_node_alias
                                .as_ref()
                                .is_some_and(|first| *first != alias.0)
                            {
                                ambiguous_body = true;
                            } else {
                                first_node_alias = Some(alias.0.clone());
                            }
                            next.extend(body.iter().cloned());
                            scope_options.push((&alias.1, true));
                        }
                    }
                }
                RuleType::NodeRule { rules: body, .. } => {
                    next.extend(body.iter().cloned());
                    scope_options.push((opts, false));
                }
                rule if crate::rule_core::rule_right_is_math_expr(rule) => {
                    next.extend(crate::rule_core::math_clause_rules().iter().cloned());
                    math_expression = true;
                }
                _ => {}
            }
        }
        if math_expression {
            let math_rules = crate::rule_core::math_clause_rules();
            collect_scope_children(
                ctx,
                inner,
                math_rules,
                scope_context,
                start_line,
                end_line,
                max_transitions,
                out,
            );
            continue;
        }
        if scope_options.is_empty() {
            continue;
        }
        let saved = scope_context.save();
        let ambient = scope_context.current();
        let mut resolved_context: Option<ScopeContext> = None;
        let mut conflict = false;
        for (opts, numeric_state_ok) in &scope_options {
            scope_context.restore(saved.clone());
            enter_block_scope(
                scope_context,
                key,
                opts,
                ctx.game,
                *numeric_state_ok,
                ctx.type_index,
            );
            let candidate = scope_context.clone();
            if let Some(previous) = &resolved_context {
                conflict |= previous != &candidate;
            } else {
                resolved_context = Some(candidate);
            }
        }
        if conflict {
            scope_context.restore(saved);
            continue;
        }
        let Some(resolved_context) = resolved_context else {
            scope_context.restore(saved);
            continue;
        };
        let resolved = resolved_context.current();
        *scope_context = resolved_context;
        if leaf.pos.start.line >= start_line
            && leaf.pos.start.line <= end_line
            && resolved != ambient
        {
            out.push(ScopeTransition {
                range: leaf.pos,
                ambient,
                resolved,
            });
        }
        if !ambiguous_body && !next.is_empty() && leaf.pos.end.line >= start_line {
            collect_scope_children(
                ctx,
                inner,
                &next,
                scope_context,
                start_line,
                end_line,
                max_transitions,
                out,
            );
        }
        scope_context.restore(saved);
    }
}

/// Descend through a skip_root_key wrapper to the grandchild containing the
/// position — mirrors `validate_wrapper_grandchildren`.
#[allow(clippy::too_many_arguments)]
fn descend_wrapper(
    ctx: &ValidationCtx,
    grandchildren: &[Child],
    path_candidates: &[PathCandidate],
    type_def: &TypeDefinition,
    wrapper_root_key: &str,
    inner_rules: &[(RuleType, Options)],
    skip_tail: &[SkipRootKey],
    scope_context: &mut Option<ScopeContext>,
    line: u32,
    col: u16,
    for_completion: bool,
) -> Option<RuleContext> {
    let candidates = grandchild_candidates_for_wrapper(path_candidates, wrapper_root_key);
    for grandchild in grandchildren {
        let Child::Leaf(gc_idx) = grandchild else {
            continue;
        };
        let gc_leaf = &ctx.ast.arena.leaves[*gc_idx as usize];
        if !pos_in_range(line, col, &gc_leaf.pos) {
            continue;
        }
        let Value::Clause(gc_children) = &gc_leaf.value else {
            return None;
        };
        let gc_key = ctx.table.get_string(gc_leaf.key.normal).unwrap_or_default();
        // Cursor on the instance key itself: treat as outside the entity.
        if line == gc_leaf.pos.start.line
            && (col as usize) <= gc_leaf.pos.start.col as usize + gc_key.chars().count()
        {
            return None;
        }

        if let [next_level, deeper_tail @ ..] = skip_tail {
            if cwtools_index::skip_root_key_matches(next_level, &gc_key) {
                return descend_wrapper(
                    ctx,
                    gc_children,
                    path_candidates,
                    type_def,
                    &gc_key,
                    inner_rules,
                    deeper_tail,
                    scope_context,
                    line,
                    col,
                    for_completion,
                );
            }
            return None;
        }

        // At the instance level: refine the type per grandchild key, as the
        // validator does.
        let (gc_type_def, gc_rules) =
            refine_grandchild_type(&candidates, &gc_key, type_def, inner_rules, ctx.ruleset)?;
        return Some(enter_entity(
            ctx,
            gc_type_def,
            gc_children,
            gc_rules,
            Some(&gc_key),
            scope_context,
            line,
            col,
            for_completion,
        ));
    }
    None
}

/// Resolve subtypes + seed the root scope for an entity, then descend to the
/// innermost block containing the position — mirrors `validate_with_type`.
#[allow(clippy::too_many_arguments)]
fn enter_entity(
    ctx: &ValidationCtx,
    type_def: &TypeDefinition,
    children: &[Child],
    inner_rules: &[(RuleType, Options)],
    node_key: Option<&str>,
    scope_context: &mut Option<ScopeContext>,
    line: u32,
    col: u16,
    for_completion: bool,
) -> RuleContext {
    let (merged, _matched, push_scope) = merged_rules_for_type(
        ctx,
        type_def,
        children,
        inner_rules,
        node_key,
        for_completion,
    );
    if let Some(sc) = scope_context.as_mut() {
        seed_root_scope(sc, type_def, push_scope, node_key, ctx.ruleset, ctx.game);
    }
    descend(ctx, children, merged.as_ref(), scope_context, line, col)
}

/// Walk one block level: find the child containing the position and either
/// recurse into the matched rule bodies or report the leaf/insert context.
/// Mirrors `validate_children`'s matching (without cardinality or errors).
fn descend(
    ctx: &ValidationCtx,
    children: &[Child],
    rules: &[(RuleType, Options)],
    scope_context: &mut Option<ScopeContext>,
    line: u32,
    col: u16,
) -> RuleContext {
    // Nested `subtype[x] = { ... }` blocks carry their fields inside SubtypeRule
    // entries; union all branches like the validator does at depth.
    let flattened;
    let rules: &[(RuleType, Options)] = if rules
        .iter()
        .any(|(rt, _)| matches!(rt, RuleType::SubtypeRule { .. }))
    {
        flattened = flatten_nested_subtype_rules(rules);
        &flattened
    } else {
        rules
    };

    for child in children {
        match child {
            Child::Leaf(idx) => {
                let leaf = &ctx.ast.arena.leaves[*idx as usize];
                if !pos_in_range(line, col, &leaf.pos) {
                    continue;
                }
                let raw_key = ctx.table.get_string(leaf.key.normal).unwrap_or_default();
                let key = unquote_key(&raw_key).to_string();
                // on_key spans the source key (quotes included), measured in chars
                // since columns are char counts; `key` is unquoted and may be shorter.
                let on_key = line == leaf.pos.start.line
                    && (col as usize) <= leaf.pos.start.col as usize + raw_key.chars().count();

                if let Value::Clause(clause_children) = &leaf.value {
                    if on_key {
                        // Editing the key of an existing block: sibling context.
                        return leaf_context(
                            ctx,
                            rules,
                            scope_context,
                            leaf,
                            &key,
                            String::new(),
                            false,
                        );
                    }
                    // The parser extends a clause leaf's range past `}` to absorb
                    // trailing whitespace. If the cursor is past all children's end
                    // lines, we're in that trailing whitespace — not inside the block.
                    // Skip and let the parent's insert-position handler supply the
                    // correct context instead of leaking this block's rules.
                    if !clause_children.is_empty() {
                        let max_child_end = clause_children
                            .iter()
                            .filter_map(|ch| match ch {
                                Child::Leaf(i) => {
                                    Some(ctx.ast.arena.leaves[*i as usize].pos.end.line)
                                }
                                Child::LeafValue(i) => {
                                    Some(ctx.ast.arena.leaf_values[*i as usize].pos.end.line)
                                }
                                _ => None,
                            })
                            .max();
                        if max_child_end.is_some_and(|max| line > max) {
                            continue;
                        }
                    }
                    // Descend into every matching rule body (disjunction → union).
                    let candidates = matching_candidates(
                        rules,
                        &key,
                        ctx.ruleset,
                        ctx.type_index,
                        rule_matches_leaf_key,
                    );
                    let mut next: Vec<(RuleType, Options)> = Vec::new();
                    let mut entered: Option<&Options> = None;
                    // Whether `entered` was first set via an effect/trigger alias
                    // (a real scope block) vs an explicit field rule (`int = {}`
                    // weight). Mirrors the validator's two enter_block_scope sites
                    // so a numeric key resolves to state only for genuine scope
                    // blocks (`129 = {}`), not random_list weights.
                    let mut entered_via_alias = false;
                    for (rule_type, opts) in &candidates {
                        match rule_type {
                            RuleType::NodeRule {
                                left: NewField::AliasField(cat),
                                ..
                            }
                            | RuleType::LeafRule {
                                left: NewField::AliasField(cat),
                                ..
                            } => {
                                for (ort, oopts) in
                                    alias_overloads(ctx.ruleset, ctx.type_index, cat, &key)
                                {
                                    if let RuleType::NodeRule { rules: body, .. } = ort {
                                        next.extend(body.iter().cloned());
                                        if entered.is_none() {
                                            entered_via_alias = true;
                                        }
                                        entered.get_or_insert(oopts);
                                    }
                                }
                            }
                            RuleType::NodeRule { rules: body, .. } => {
                                next.extend(body.iter().cloned());
                                entered.get_or_insert(opts);
                            }
                            // `value_set[variable] = math_expr` (and `value =
                            // math_expr`): a `{block}` math expression. Descend
                            // into the synthesized math-clause rules so completion
                            // offers `value`, the `mathexpr` operators, and
                            // variable operands inside the block.
                            rt if crate::rule_core::rule_right_is_math_expr(rt) => {
                                next.extend(crate::rule_core::math_clause_rules().iter().cloned());
                                entered.get_or_insert(opts);
                            }
                            _ => {}
                        }
                    }
                    if next.is_empty() {
                        // Unknown block or leaf-only matches: no rule context below
                        // here. Empty child_rules (not the parent's) — suggestions
                        // from the parent level would be wrong inside this block.
                        return RuleContext {
                            scope: scope_context.clone(),
                            ..Default::default()
                        };
                    }
                    if let (Some(sc), Some(opts)) = (scope_context.as_mut(), entered) {
                        enter_block_scope(
                            sc,
                            &key,
                            opts,
                            ctx.game,
                            entered_via_alias,
                            ctx.type_index,
                        );
                    }
                    return descend(ctx, clause_children, &next, scope_context, line, col);
                }

                // A scalar `key = value` is single-line, but the parser's leaf
                // range absorbs trailing whitespace up to the next token (see
                // parse_value). So a cursor on a later, blank line falls inside
                // this leaf's range while actually being a new-field insert
                // position — fall through to the block's child rules instead of
                // offering this leaf's (usually empty) value completions.
                if line != leaf.pos.start.line {
                    continue;
                }
                // Scalar leaf: cursor on a `key = value` line.
                let value = leaf_value_to_string(&leaf.value, ctx.table);
                return leaf_context(ctx, rules, scope_context, leaf, &key, value, !on_key);
            }
            Child::LeafValue(idx) => {
                let lv = &ctx.ast.arena.leaf_values[*idx as usize];
                if !pos_in_range(line, col, &lv.pos) {
                    continue;
                }
                if let Value::Clause(ch) = &lv.value {
                    // Anonymous `{ ... }` block → ValueClauseRule bodies.
                    let next = valueclause_bodies(rules);
                    if next.is_empty() {
                        return RuleContext {
                            scope: scope_context.clone(),
                            ..Default::default()
                        };
                    }
                    return descend(ctx, ch, &next, scope_context, line, col);
                }
                // Bare value: complete against the block's LeafValueRules.
                let value = leaf_value_to_string(&lv.value, ctx.table);
                let value_rules: Vec<(RuleType, Options)> = rules
                    .iter()
                    .filter(|(rt, _)| matches!(rt, RuleType::LeafValueRule { .. }))
                    .cloned()
                    .collect();
                return RuleContext {
                    child_rules: rules.to_vec(),
                    value_rules,
                    leaf: Some(LeafAtPos {
                        key: String::new(),
                        value,
                        in_value: true,
                        line: lv.pos.start.line,
                        col: lv.pos.start.col,
                    }),
                    scope: scope_context.clone(),
                };
            }
            _ => {}
        }
    }

    // No child contains the position: the cursor is at an insert position in
    // this block.
    RuleContext {
        child_rules: rules.to_vec(),
        value_rules: Vec::new(),
        leaf: None,
        scope: scope_context.clone(),
    }
}

fn valueclause_bodies(rules: &[(RuleType, Options)]) -> Vec<(RuleType, Options)> {
    let mut next = Vec::new();
    for (rt, _) in rules {
        if let RuleType::ValueClauseRule { rules: body } = rt {
            next.extend(body.iter().cloned());
        }
    }
    next
}

/// Build the context for a cursor on a leaf: the matched rules become
/// `value_rules` (alias matches expanded to their leaf overloads), the current
/// block's rules stay available as `child_rules` for key edits.
fn leaf_context(
    ctx: &ValidationCtx,
    rules: &[(RuleType, Options)],
    scope_context: &Option<ScopeContext>,
    leaf: &cwtools_parser::ast::Leaf,
    key: &str,
    value: String,
    in_value: bool,
) -> RuleContext {
    RuleContext {
        child_rules: rules.to_vec(),
        value_rules: value_rules_for_key(ctx.ruleset, ctx.type_index, rules, key)
            .into_iter()
            .cloned()
            .collect(),
        leaf: Some(LeafAtPos {
            key: key.to_string(),
            value,
            in_value,
            line: leaf.pos.start.line,
            col: leaf.pos.start.col,
        }),
        scope: scope_context.clone(),
    }
}

/// The alias category (`trigger`, `effect`, `modifier`, …) that `key` resolves
/// through within `child_rules`, if any. Editor hovers use it as the header
/// ("trigger" vs "effect") for a usage like `has_completed_focus`.
pub fn alias_category_for_key(
    ruleset: &RuleSet,
    type_index: Option<&cwtools_index::TypeIndex>,
    child_rules: &[(RuleType, Options)],
    key: &str,
) -> Option<String> {
    let candidates =
        matching_candidates(child_rules, key, ruleset, type_index, rule_matches_leaf_key);
    candidates.iter().find_map(|(rt, _)| match rt {
        RuleType::LeafRule {
            left: NewField::AliasField(cat),
            ..
        }
        | RuleType::NodeRule {
            left: NewField::AliasField(cat),
            ..
        } => Some(cat.clone()),
        _ => None,
    })
}

/// The matched rules for `key` within a block whose rules are `child_rules`.
/// Alias-keyed matches are expanded to their alias overloads (so
/// `has_completed_focus` resolves through `alias[trigger:...]` to its
/// `<focus>` right side). Includes matched NodeRules too — completion only
/// reads LeafRule/LeafValueRule rights, while hover wants any matched rule's
/// description. Public so the LSP can resolve a mid-edit `key = |` line where
/// no leaf exists in the last good parse yet.
/// Borrows: the matches all live in `child_rules` or in the ruleset's alias
/// table, so the per-leaf semantic-token sweep copies nothing.
pub fn value_rules_for_key<'a>(
    ruleset: &'a RuleSet,
    type_index: Option<&cwtools_index::TypeIndex>,
    child_rules: &'a [(RuleType, Options)],
    key: &str,
) -> Vec<&'a (RuleType, Options)> {
    let candidates =
        matching_candidates(child_rules, key, ruleset, type_index, rule_matches_leaf_key);
    let mut out: Vec<&(RuleType, Options)> = Vec::new();
    for rule in candidates {
        match &rule.0 {
            RuleType::LeafRule {
                left: NewField::AliasField(cat),
                ..
            }
            | RuleType::NodeRule {
                left: NewField::AliasField(cat),
                ..
            } => out.extend(alias_overloads(ruleset, type_index, cat, key)),
            RuleType::LeafRule { .. } | RuleType::NodeRule { .. } => out.push(rule),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_parser::ast::{SourcePos, SourceRange};

    fn range(sl: u32, sc: u16, el: u32, ec: u16) -> SourceRange {
        SourceRange {
            start: SourcePos { line: sl, col: sc },
            end: SourcePos { line: el, col: ec },
        }
    }

    #[test]
    fn pos_in_range_is_inclusive_on_both_ends() {
        let r = range(5, 3, 5, 10);
        assert!(pos_in_range(5, 3, &r), "start is inclusive");
        assert!(pos_in_range(5, 10, &r), "end is inclusive");
        assert!(pos_in_range(5, 5, &r));
        assert!(!pos_in_range(5, 2, &r), "one before start");
        assert!(!pos_in_range(5, 11, &r), "one after end");
    }

    #[test]
    fn pos_in_range_handles_multiline() {
        let r = range(2, 0, 4, 5);
        assert!(pos_in_range(2, 0, &r));
        assert!(pos_in_range(3, 100, &r), "middle line any col");
        assert!(pos_in_range(4, 5, &r));
        assert!(!pos_in_range(4, 6, &r));
        assert!(!pos_in_range(1, 0, &r), "before start line");
        assert!(!pos_in_range(5, 0, &r), "after end line");
    }

    #[test]
    fn pos_in_range_zero_width_point() {
        let r = range(1, 5, 1, 5);
        assert!(pos_in_range(1, 5, &r));
        assert!(!pos_in_range(1, 4, &r));
        assert!(!pos_in_range(1, 6, &r));
    }

    #[test]
    fn scope_transitions_follow_rule_push_scope() {
        use cwtools_game::constants::Game;
        use cwtools_parser::parser::parse_string;
        use cwtools_rules::rules_converter::ast_to_ruleset;
        use cwtools_string_table::string_table::StringTable;
        let table = StringTable::new();
        let rules = parse_string(
            "types = { type[foo] = { path = \"common/foo\" } }\n\
             scopes = { Country = { aliases = { country } } Character = { aliases = { character } } }\n\
             links = { owner = { output_scope = character input_scopes = country } controller = { output_scope = country input_scopes = character } }\n\
             foo = {\n                 ## push_scope = character\n                 custom = {\n                     add = int\n                 }\n             }\n\
             alias[effect:scope_field] = { alias_name[effect] = alias_match_left[effect] }\n",
            &table,
        );
        let ruleset = ast_to_ruleset(&rules, &table);
        let registry = crate::build_scope_registry_arc(&ruleset, Some(Game::Hoi4));
        let prepared = crate::Prepared {
            ruleset: &ruleset,
            table: &table,
            game: Some(Game::Hoi4),
            type_index: None,
            modifier_keys: None,
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: true,
            var_checks: false,
        };
        assert!(
            ruleset.type_rules_idx().contains_key("foo"),
            "{:#?}",
            ruleset.type_rules_idx()
        );
        assert!(!find_rules_by_name("foo", &ruleset).is_empty());
        assert!(
            registry
                .as_ref()
                .and_then(|r| r.id_of("character"))
                .is_some()
        );
        let ast = parse_string(
            "foo = {\n    custom = {\n        add = { }\n    }\n}\n",
            &table,
        );
        let transitions = scope_transitions(&ast, "common/foo/test.txt", &prepared, 1, 5);
        assert_eq!(transitions.len(), 1, "got: {transitions:?}");
        assert_eq!(transitions[0].range.start.line, 2);
        assert_eq!(transitions[0].resolved, ScopeId(101));
    }

    #[test]
    fn scope_transitions_include_empty_scope_changing_blocks() {
        use cwtools_game::constants::Game;
        use cwtools_parser::parser::parse_string;
        use cwtools_rules::rules_converter::ast_to_ruleset;
        use cwtools_string_table::string_table::StringTable;
        let table = StringTable::new();
        let rules = parse_string(
            "types = { type[foo] = { path = \"common/foo\" } }\n\
             scopes = { Country = { aliases = { country } } Character = { aliases = { character } } }\n\
             foo = {
                 ## push_scope = character
                 custom = { }
             }
",
            &table,
        );
        let ruleset = ast_to_ruleset(&rules, &table);
        assert!(ruleset.type_rules_idx().contains_key("foo"));
        let registry = crate::build_scope_registry_arc(&ruleset, Some(Game::Hoi4));
        let prepared = crate::Prepared {
            ruleset: &ruleset,
            table: &table,
            game: Some(Game::Hoi4),
            type_index: None,
            modifier_keys: None,
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: true,
            var_checks: false,
        };
        let ast = parse_string("foo = { custom = { } }\n", &table);
        let transitions = scope_transitions(&ast, "common/foo/test.txt", &prepared, 1, 1);
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].resolved, ScopeId(101));
    }

    #[test]
    fn scope_transitions_respect_visible_start_and_end_lines() {
        use cwtools_game::constants::Game;
        use cwtools_parser::parser::parse_string;
        use cwtools_rules::rules_converter::ast_to_ruleset;
        use cwtools_string_table::string_table::StringTable;
        let table = StringTable::new();
        let rules = parse_string(
            "types = { type[foo] = { path = \"common/foo\" } }\n\
             scopes = { Country = { aliases = { country } } Character = { aliases = { character } } }\n\
             links = { owner = { output_scope = character input_scopes = country } }\n\
             foo = { alias_name[effect] = alias_match_left[effect] }\n\
             alias[effect:scope_field] = { alias_name[effect] = alias_match_left[effect] }\n",
            &table,
        );
        let ruleset = ast_to_ruleset(&rules, &table);
        let registry = crate::build_scope_registry_arc(&ruleset, Some(Game::Hoi4));
        let prepared = crate::Prepared {
            ruleset: &ruleset,
            table: &table,
            game: Some(Game::Hoi4),
            type_index: None,
            modifier_keys: None,
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: true,
            var_checks: false,
        };
        let ast = parse_string("foo = {\n    owner = { }\n}\n", &table);
        assert!(scope_transitions(&ast, "common/foo/test.txt", &prepared, 1, 1).is_empty());
        assert_eq!(
            scope_transitions(&ast, "common/foo/test.txt", &prepared, 1, 2).len(),
            1
        );
    }

    #[test]
    fn rules_at_pos_distinguishes_on_key_from_in_value() {
        // Exercises the `on_key` vs `in_value` branch without needing a full
        // HOI4 corpus. Uses a minimal focus type so the instance block is
        // recognised via `path = "common/national_focus"`.
        use cwtools_file_manager::file_manager::ScanBudget;
        use cwtools_parser::parser::parse_string;
        use cwtools_rules::ruleset_loader::load_ruleset_from_dir;
        use cwtools_string_table::string_table::StringTable;
        let table = StringTable::new();
        let dir = std::path::PathBuf::from(format!(
            "/tmp/cwtools_position_test_{}_{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("focus.cwt"),
            "types = { type[focus] = { path = \"common/national_focus\" } }\n\
             focus = { id = scalar x = int }\n",
        )
        .unwrap();
        let (ruleset, _) = load_ruleset_from_dir(&dir, &table, ScanBudget::default());
        let _ = std::fs::remove_dir_all(&dir);
        let prepared = crate::Prepared {
            ruleset: &ruleset,
            table: &table,
            game: None,
            type_index: None,
            modifier_keys: None,
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: None,
            scope_checks: false,
            var_checks: false,
        };
        let file_path = "common/national_focus/test.txt";
        let text = "my_focus = {\n    id = my_id\n}\n";
        let ast = parse_string(text, &table);
        let on_key = rules_at_pos(&ast, file_path, &prepared, 2, 4, false);
        let in_val = rules_at_pos(&ast, file_path, &prepared, 2, 9, false);
        let a = on_key.expect("cursor on key must resolve");
        let b = in_val.expect("cursor on value must resolve");
        assert!(a.leaf.is_some() && b.leaf.is_some());
        assert!(!a.leaf.as_ref().unwrap().in_value, "cursor on key");
        assert!(b.leaf.as_ref().unwrap().in_value, "cursor on value");
        assert_eq!(a.leaf.as_ref().unwrap().key, "id");
        assert_eq!(b.leaf.as_ref().unwrap().key, "id");
        // Cursor on the root instance key itself must be outside any entity.
        let root_key = rules_at_pos(&ast, file_path, &prepared, 1, 0, false);
        assert!(
            root_key.is_none(),
            "root key itself is not inside an entity"
        );
    }
}
