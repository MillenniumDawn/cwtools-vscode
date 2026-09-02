use cwtools_game::scope_engine::ScopeContext;
use cwtools_parser::ast::{Child, Value};
use cwtools_rules::rules_types::*;
use smallvec::SmallVec;
use std::sync::LazyLock;

use crate::common::*;
use crate::ctx::ValidationCtx;
use crate::inline_script;
use crate::scope::{enter_block_scope, scope_matches_required};
use cwtools_error_codes as error_codes;

const INLINE_SCRIPT: &str = "inline_script";

use super::alias::validate_alias_usage;
use super::leaf::{check_variable_get, field_matches_value, validate_leaf};
use super::matching::{get_rule_key, matching_candidates, rule_matches_leaf_key};
use super::subtype_merge::flatten_nested_subtype_rules;
use super::suggest::best_suggestion;

fn rule_left_is_ignore(rule_type: &RuleType) -> bool {
    matches!(
        rule_type,
        RuleType::LeafRule {
            left: NewField::IgnoreField(_),
            ..
        } | RuleType::NodeRule {
            left: NewField::IgnoreField(_),
            ..
        }
    )
}

fn validate_leaf_against_rule(
    ctx: &ValidationCtx,
    leaf: &cwtools_parser::ast::Leaf,
    key: &str,
    rule_type: &RuleType,
    opts: &Options,
    scope_context: &mut Option<ScopeContext>,
    errors: &mut Vec<ValidationError>,
) {
    if ctx.alias_branch_budget_exhausted() {
        return;
    }
    if rule_left_is_ignore(rule_type) {
        return;
    }
    if let RuleType::LeafRule { left, .. } | RuleType::NodeRule { left, .. } = rule_type
        && let NewField::TypeField(type_type) = left
    {
        crate::references::mark_type_field_use(ctx, type_type, key);
    }
    if let Some(sc) = scope_context.as_ref()
        && !opts.required_scopes.is_empty()
        && !scope_matches_required(sc.current(), sc.registry.as_ref(), &opts.required_scopes)
    {
        let current = sc.current();
        let code = &error_codes::CW247_RULE_WRONG_SCOPE;
        errors.push(
            ValidationError::from_code(
                code,
                ctx.file_path,
                leaf.pos.start.line,
                leaf.pos.start.col,
                &[
                    key,
                    &sc.registry.name_of(current),
                    &opts.required_scopes.join(" or "),
                ],
            )
            .with_end(key_token_end(leaf, key, ctx.table)),
        );
    }
    match rule_type {
        RuleType::LeafRule { left, .. } => {
            if let NewField::AliasField(category) = left {
                let leaf_pos = (leaf.pos.start.line, leaf.pos.start.col);
                validate_alias_usage(
                    ctx,
                    category,
                    key,
                    Some(leaf),
                    None,
                    leaf_pos,
                    scope_context,
                    errors,
                );
            } else {
                validate_leaf(ctx, leaf, rule_type, scope_context.as_ref(), errors);
                if let Some(default) = opts.default_bool {
                    with_leaf_value_str(&leaf.value, ctx.table, |raw| {
                        let v = raw.trim_matches('"').trim();
                        let is_default = if v.eq_ignore_ascii_case("yes")
                            || v.eq_ignore_ascii_case("true")
                        {
                            default
                        } else if v.eq_ignore_ascii_case("no") || v.eq_ignore_ascii_case("false") {
                            !default
                        } else {
                            false
                        };
                        if is_default {
                            let code = &error_codes::CW282_REDUNDANT_DEFAULT_BOOL;
                            let fix = cwtools_parser::fix::SuggestedFix::delete(
                                cwtools_i18n::t(cwtools_i18n::Key::ActionRemoveRedundantDefault),
                                leaf.pos,
                            );
                            errors.push(
                                ValidationError::from_code(
                                    code,
                                    ctx.file_path,
                                    leaf.pos.start.line,
                                    leaf.pos.start.col,
                                    &[v],
                                )
                                .with_fix(fix)
                                .with_end(leaf.pos.end),
                            );
                        }
                    });
                }
            }
        }
        RuleType::NodeRule {
            left,
            rules: inner_rules,
            ..
        } => {
            if let NewField::AliasField(category) = left {
                let leaf_pos = (leaf.pos.start.line, leaf.pos.start.col);
                validate_alias_usage(
                    ctx,
                    category,
                    key,
                    Some(leaf),
                    None,
                    leaf_pos,
                    scope_context,
                    errors,
                );
            } else if let Value::Clause(clause_children) = &leaf.value {
                let saved = scope_context.as_ref().map(|sc| sc.save());
                if let Some(sc) = scope_context.as_mut() {
                    enter_block_scope(sc, key, opts, ctx.game, false, ctx.type_index);
                }
                validate_children(
                    ctx,
                    clause_children,
                    inner_rules,
                    scope_context,
                    (leaf.pos.start.line, leaf.pos.start.col),
                    errors,
                );
                if let (Some(saved), Some(ref mut sc)) = (saved, scope_context.as_mut()) {
                    sc.restore(saved);
                }
            } else {
                let val_str = leaf_value_to_string(&leaf.value, ctx.table);
                errors.push(
                    ValidationError::from_code(
                        &error_codes::CW267_UNEXPECTED_ALIAS_KEY_VALUE,
                        ctx.file_path,
                        leaf.pos.start.line,
                        leaf.pos.start.col,
                        &[key, &val_str],
                    )
                    .with_end(key_token_end(leaf, key, ctx.table)),
                );
            }
        }
        _ => {}
    }
}

fn pick_best_candidate<F, G>(
    ctx: &ValidationCtx,
    mut validate_one: F,
    mut only_match_error: G,
    errors: &mut Vec<ValidationError>,
    n: usize,
) where
    F: FnMut(usize, &mut Vec<ValidationError>),
    G: FnMut(usize) -> Option<ValidationError>,
{
    let mut best: Option<Vec<ValidationError>> = None;
    let mut only_match: Option<ValidationError> = None;
    let mut temp: Vec<ValidationError> = Vec::new();
    for i in 0..n {
        if ctx.alias_branch_budget_exhausted() {
            return;
        }
        temp.clear();
        validate_one(i, &mut temp);
        if ctx.alias_branch_budget_exhausted() {
            return;
        }
        if temp.is_empty() {
            match only_match_error(i) {
                Some(custom) => {
                    if only_match.is_none() {
                        only_match = Some(custom);
                    }
                }
                None => return, // directive-free clean match — accept
            }
        } else {
            match &best {
                Some(b) if b.len() <= temp.len() => {}
                _ => best = Some(std::mem::take(&mut temp)),
            }
        }
    }
    if let Some(custom) = only_match {
        errors.push(custom);
    } else if let Some(b) = best {
        errors.extend(b);
    }
}

pub(crate) fn validate_children(
    ctx: &ValidationCtx,
    children: &[Child],
    rules: &[(RuleType, Options)],
    scope_context: &mut Option<ScopeContext>,
    block_pos: (u32, u16),
    errors: &mut Vec<ValidationError>,
) {
    if ctx.alias_branch_budget_exhausted() {
        return;
    }
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

    let mut block = BlockRules::of(rules);

    let (leafvalue_counts, valueclause_counts) =
        count_and_validate_children(ctx, children, rules, &mut block, scope_context, errors);
    if ctx.alias_branch_budget_exhausted() {
        return;
    }

    enforce_cardinality(
        ctx,
        children,
        rules,
        &mut block,
        block_pos,
        &leafvalue_counts,
        &valueclause_counts,
        errors,
    );
}

struct KeyCard<'a> {
    key: &'a str,
    min: i32,
    max: i32,
    strict_min: bool,
    count: i32,
    reported: bool,
}

struct BlockRules<'a> {
    cards: SmallVec<[KeyCard<'a>; 8]>,
    leafvalue: bool,
    valueclause: bool,
}

impl<'a> BlockRules<'a> {
    fn of(rules: &'a [(RuleType, Options)]) -> Self {
        let mut out = BlockRules {
            cards: SmallVec::new(),
            leafvalue: false,
            valueclause: false,
        };
        for (rule_type, opts) in rules {
            match rule_type {
                RuleType::LeafValueRule { .. } => out.leafvalue = true,
                RuleType::ValueClauseRule { .. } => out.valueclause = true,
                _ => {}
            }
            let Some(key) = get_rule_key(rule_type) else {
                continue;
            };
            match out.position(key) {
                Some(i) => {
                    let c = &mut out.cards[i];
                    c.min = c.min.min(opts.min);
                    c.max = c.max.max(opts.max);
                    c.strict_min = c.strict_min && opts.strict_min;
                }
                None => out.cards.push(KeyCard {
                    key,
                    min: opts.min,
                    max: opts.max,
                    strict_min: opts.strict_min,
                    count: 0,
                    reported: false,
                }),
            }
        }
        out
    }

    fn position(&self, key: &str) -> Option<usize> {
        self.cards
            .iter()
            .position(|c| c.key.eq_ignore_ascii_case(key))
    }

    fn any(&self) -> bool {
        !self.cards.is_empty() || self.leafvalue || self.valueclause
    }
}

const MODIFIER_ALIAS: &str = "modifier";

fn candidates_are_modifier_alias_only(candidates: &[&(RuleType, Options)]) -> bool {
    !candidates.is_empty()
        && candidates.iter().all(|(rule_type, _)| {
            matches!(
                rule_type,
                RuleType::LeafRule {
                    left: NewField::AliasField(category),
                    ..
                } | RuleType::NodeRule {
                    left: NewField::AliasField(category),
                    ..
                } if category == MODIFIER_ALIAS
            )
        })
}

fn push_zero_modifier(
    file_path: &FilePath,
    leaf: &cwtools_parser::ast::Leaf,
    key: &str,
    errors: &mut Vec<ValidationError>,
) {
    errors.push(
        ValidationError::from_code(
            &error_codes::CW235_ZERO_MODIFIER,
            file_path,
            leaf.pos.start.line,
            leaf.pos.start.col,
            &[key],
        )
        .with_end(leaf.pos.end),
    );
}

pub(crate) fn rule_right_is_math_expr(rule_type: &RuleType) -> bool {
    matches!(
        rule_type,
        RuleType::LeafRule {
            right: NewField::ValueField(ValueType::MathExpr),
            ..
        }
    )
}

static MATH_CLAUSE_RULES: LazyLock<Vec<(RuleType, Options)>> = LazyLock::new(|| {
    let many = Options {
        min: 0,
        max: i32::MAX,
        ..Default::default()
    };
    vec![
        (
            RuleType::LeafRule {
                left: NewField::SpecificField("value".to_string()),
                right: NewField::ValueField(ValueType::MathExpr),
            },
            many.clone(),
        ),
        (
            RuleType::LeafRule {
                left: NewField::SpecificField("tooltip".to_string()),
                right: NewField::ScalarField,
            },
            many.clone(),
        ),
        (
            RuleType::LeafRule {
                left: NewField::AliasField("mathexpr".to_string()),
                right: NewField::AliasField("mathexpr".to_string()),
            },
            many,
        ),
    ]
});

pub(crate) fn math_clause_rules() -> &'static [(RuleType, Options)] {
    &MATH_CLAUSE_RULES
}

pub(super) fn validate_math_clause(
    ctx: &ValidationCtx,
    children: &[Child],
    scope_context: &mut Option<ScopeContext>,
    pos: (u32, u16),
    errors: &mut Vec<ValidationError>,
) {
    validate_children(
        ctx,
        children,
        math_clause_rules(),
        scope_context,
        pos,
        errors,
    );
}

#[tracing::instrument(level = "trace", skip_all)]
fn count_and_validate_children<'r>(
    ctx: &ValidationCtx,
    children: &[Child],
    rules: &'r [(RuleType, Options)],
    block: &mut BlockRules<'r>,
    scope_context: &mut Option<ScopeContext>,
    errors: &mut Vec<ValidationError>,
) -> (Vec<usize>, Vec<usize>) {
    let ast = ctx.ast;
    let table = ctx.table;
    let file_path = ctx.file_path;
    let ruleset = ctx.ruleset;
    let type_index = ctx.type_index;
    let modifier_keys = ctx.modifier_keys;

    let any_keyed = !block.cards.is_empty();
    let any_leafvalue = block.leafvalue;
    let any_valueclause = block.valueclause;

    let mut leafvalue_counts: Vec<usize> = if any_leafvalue {
        vec![0usize; rules.len()]
    } else {
        Vec::new()
    };
    let mut valueclause_counts: Vec<usize> = if any_valueclause {
        vec![0usize; rules.len()]
    } else {
        Vec::new()
    };

    for child in children {
        if ctx.alias_branch_budget_exhausted() {
            break;
        }
        match child {
            Child::Leaf(idx) => {
                let leaf = &ast.arena.leaves[*idx as usize];
                let mut keybuf: SmallVec<[u8; 24]> = SmallVec::new();
                table.with_string(leaf.key.normal, |s| {
                    keybuf.extend_from_slice(unquote_key(s).as_bytes())
                });
                let key: &str = std::str::from_utf8(&keybuf).unwrap_or_default();
                if key.eq_ignore_ascii_case(INLINE_SCRIPT) {
                    expand_inline_script_call(
                        ctx,
                        leaf,
                        key,
                        rules,
                        block,
                        scope_context,
                        &mut leafvalue_counts,
                        &mut valueclause_counts,
                        errors,
                    );
                    continue;
                }
                if any_keyed && let Some(i) = block.position(key) {
                    block.cards[i].count += 1;
                }
                let candidates =
                    matching_candidates(rules, key, ruleset, type_index, rule_matches_leaf_key);
                if let Value::Clause(math_children) = &leaf.value
                    && candidates.iter().any(|(rt, _)| rule_right_is_math_expr(rt))
                {
                    let pos = (leaf.pos.start.line, leaf.pos.start.col);
                    validate_math_clause(ctx, math_children, scope_context, pos, errors);
                    continue;
                }
                if candidates.is_empty() {
                    let key_lower = key.to_lowercase();
                    let is_modifier = modifier_keys
                        .map(|mk| mk.contains(key_lower.as_str()))
                        .unwrap_or(false);
                    if is_modifier && value_is_zero(&leaf.value) {
                        push_zero_modifier(file_path, leaf, key, errors);
                    }
                    let is_define = key.starts_with('@');
                    if !is_modifier && !is_define {
                        let is_block = matches!(leaf.value, Value::Clause(_));
                        let (msg, code) = if is_block {
                            (
                                format!("Unexpected block '{}'", key),
                                &error_codes::CW262_UNEXPECTED_PROPERTY_NODE,
                            )
                        } else {
                            (
                                format!("Unexpected field '{}'", key),
                                &error_codes::CW263_UNEXPECTED_PROPERTY_LEAF,
                            )
                        };
                        let end = if is_block {
                            key_token_end(leaf, key, table)
                        } else {
                            leaf.pos.end
                        };
                        let mut err = ValidationError::from_code_with(
                            code,
                            ErrorSeverity::Error,
                            file_path,
                            leaf.pos.start.line,
                            leaf.pos.start.col,
                            msg,
                        )
                        .with_end(end);
                        if let Some(cand) = best_suggestion(
                            key,
                            rules.iter().filter_map(|(rt, _)| get_rule_key(rt)),
                        ) {
                            err = err.with_fix(cwtools_parser::fix::SuggestedFix::replace(
                                cwtools_i18n::format(cwtools_i18n::Key::ActionDidYouMean, &[cand]),
                                cwtools_parser::ast::SourceRange {
                                    start: leaf.pos.start,
                                    end: key_token_end(leaf, key, table),
                                },
                                cand,
                            ));
                        }
                        errors.push(err);
                    }
                } else {
                    if value_is_zero(&leaf.value)
                        && candidates_are_modifier_alias_only(&candidates)
                        && modifier_keys.is_some_and(|mk| mk.contains(key.to_lowercase().as_str()))
                    {
                        push_zero_modifier(file_path, leaf, key, errors);
                    }
                    let n = candidates.len();
                    pick_best_candidate(
                        ctx,
                        |i, out| {
                            let (rt, opts) = candidates[i];
                            validate_leaf_against_rule(
                                ctx,
                                leaf,
                                key,
                                rt,
                                opts,
                                scope_context,
                                out,
                            );
                        },
                        |i| {
                            let (_, opts) = candidates[i];
                            opts.error_if_only_match.as_ref().map(|msg| {
                                let sev = opts
                                    .severity
                                    .as_ref()
                                    .map(severity_to_error)
                                    .unwrap_or(ErrorSeverity::Error);
                                ValidationError::from_code_with(
                                    &error_codes::CW272_FROM_RULES_CUSTOM_ERROR,
                                    sev,
                                    file_path,
                                    leaf.pos.start.line,
                                    leaf.pos.start.col,
                                    msg.clone(),
                                )
                                .with_end(key_token_end(leaf, key, table))
                            })
                        },
                        errors,
                        n,
                    );
                }
            }
            Child::LeafValue(lvidx) => {
                let lv = &ast.arena.leaf_values[*lvidx as usize];
                if matches!(lv.value, Value::Clause(_)) {
                    if any_valueclause {
                        for (rule_idx, (rule_type, _)) in rules.iter().enumerate() {
                            if matches!(rule_type, RuleType::ValueClauseRule { .. }) {
                                valueclause_counts[rule_idx] += 1;
                            }
                        }
                    }
                } else if any_leafvalue {
                    for (rule_idx, (rule_type, _)) in rules.iter().enumerate() {
                        if let RuleType::LeafValueRule { right } = rule_type
                            && field_matches_value(right, &lv.value, table, ruleset)
                        {
                            leafvalue_counts[rule_idx] += 1;
                        }
                    }
                }
                if let Value::Clause(clause_children) = &lv.value {
                    let mut matched = false;
                    for (rule_type, _) in rules {
                        if let RuleType::ValueClauseRule { rules: vc_rules } = rule_type {
                            matched = true;
                            validate_children(
                                ctx,
                                clause_children,
                                vc_rules,
                                scope_context,
                                (lv.pos.start.line, lv.pos.start.col),
                                errors,
                            );
                            break;
                        }
                    }
                    if !matched {
                        errors.push(
                            ValidationError::from_code(
                                &error_codes::CW265_UNEXPECTED_PROPERTY_VALUE_CLAUSE,
                                file_path,
                                lv.pos.start.line,
                                lv.pos.start.col,
                                &["Unexpected value clause '{...}'"],
                            )
                            .with_end(lv.pos.end),
                        );
                    }
                } else {
                    let mut matched = false;
                    for (rule_type, _opts) in rules {
                        if let RuleType::LeafValueRule { right } = rule_type
                            && field_matches_value(right, &lv.value, table, ruleset)
                        {
                            if let NewField::VariableGetField(ns) = right {
                                with_leaf_value_str(&lv.value, table, |raw| {
                                    check_variable_get(
                                        ctx,
                                        ns,
                                        raw,
                                        lv.pos.start.line,
                                        lv.pos.start.col,
                                        lv.pos.end,
                                        errors,
                                    );
                                });
                            }
                            if let NewField::TypeField(type_type) = right {
                                with_leaf_value_str(&lv.value, table, |raw| {
                                    crate::references::mark_type_field_use(ctx, type_type, raw);
                                });
                            }
                            matched = true;
                            break;
                        }
                    }
                    if !matched {
                        let val_str = leaf_value_to_string(&lv.value, table);
                        errors.push(
                            ValidationError::from_code(
                                &error_codes::CW264_UNEXPECTED_PROPERTY_LEAF_VALUE,
                                file_path,
                                lv.pos.start.line,
                                lv.pos.start.col,
                                &[&format!("Unexpected bare value '{}'", val_str)],
                            )
                            .with_end(lv.pos.end),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    (leafvalue_counts, valueclause_counts)
}

#[allow(clippy::too_many_arguments)]
fn expand_inline_script_call<'r>(
    ctx: &ValidationCtx,
    leaf: &cwtools_parser::ast::Leaf,
    key: &str,
    rules: &'r [(RuleType, Options)],
    block: &mut BlockRules<'r>,
    scope_context: &mut Option<ScopeContext>,
    leafvalue_counts: &mut [usize],
    valueclause_counts: &mut [usize],
    errors: &mut Vec<ValidationError>,
) {
    let Some(scripts) = ctx.inline_scripts else {
        return;
    };
    let end = key_token_end(leaf, key, ctx.table);
    if ctx.inline_script_expansion_budget_exhausted()
        || !ctx.reserve_inline_script_expansion(leaf.pos.start, Some(end))
    {
        return;
    }
    let expanded = {
        let stack = ctx.inline_stack.borrow();
        inline_script::expand(leaf, &ctx.ast.arena, ctx.table, scripts, &stack)
    };
    let expanded = match expanded {
        Ok(expanded) => expanded,
        Err(failure) => {
            errors.push(
                ValidationError::from_code(
                    &error_codes::CW274_INLINE_SCRIPT_ERROR,
                    ctx.file_path,
                    leaf.pos.start.line,
                    leaf.pos.start.col,
                    &[&failure.to_string()],
                )
                .with_end(end),
            );
            return;
        }
    };

    ctx.inline_stack.borrow_mut().push(expanded.name);
    let body = ctx.for_inline_body(&expanded.ast);
    let mut body_errors = Vec::new();
    let (body_leafvalues, body_valueclauses) = count_and_validate_children(
        &body,
        &expanded.ast.root_children,
        rules,
        block,
        scope_context,
        &mut body_errors,
    );
    ctx.inline_stack.borrow_mut().pop();

    for (slot, count) in leafvalue_counts.iter_mut().zip(body_leafvalues) {
        *slot += count;
    }
    for (slot, count) in valueclause_counts.iter_mut().zip(body_valueclauses) {
        *slot += count;
    }

    errors.extend(body_errors.into_iter().map(|mut error| {
        error.message = format!(
            "{} (in {}:{})",
            error.message, expanded.logical_path, error.line
        );
        error.line = leaf.pos.start.line;
        error.col = leaf.pos.start.col;
        error.end = Some((end.line, end.col));
        error.fix = None;
        error.related.clear();
        error
    }));
}

#[allow(clippy::too_many_arguments)]
fn enforce_cardinality(
    ctx: &ValidationCtx,
    children: &[Child],
    rules: &[(RuleType, Options)],
    block: &mut BlockRules<'_>,
    block_pos: (u32, u16),
    leafvalue_counts: &[usize],
    valueclause_counts: &[usize],
    errors: &mut Vec<ValidationError>,
) {
    if !block.any() {
        return;
    }

    let ast = ctx.ast;
    let table = ctx.table;
    let file_path = ctx.file_path;

    let (block_line, block_col) = if block_pos != (0, 0) {
        block_pos
    } else {
        children
            .iter()
            .find_map(|c| child_start_pos(c, ast))
            .unwrap_or(block_pos)
    };

    for (rule_idx, (rule_type, opts)) in rules.iter().enumerate() {
        let card_sev = opts
            .severity
            .as_ref()
            .map(severity_to_error)
            .unwrap_or(ErrorSeverity::Warning);
        let missing_sev = card_sev;
        let max_sev = card_sev;

        match rule_type {
            RuleType::LeafRule { .. } | RuleType::NodeRule { .. } => {
                if let Some(key) = get_rule_key(rule_type) {
                    let bounds = match block.position(key).map(|i| &mut block.cards[i]) {
                        Some(c) if !c.reported => {
                            c.reported = true;
                            Some((c.min, c.max, c.strict_min, c.count))
                        }
                        _ => None,
                    };
                    if let Some((kmin, kmax, kstrict, count)) = bounds {
                        if count < kmin && kstrict {
                            errors.push(ValidationError::from_code_with(
                                &error_codes::CW242_WRONG_NUMBER,
                                missing_sev,
                                file_path,
                                block_line,
                                block_col,
                                format!(
                                    "Field '{}' appears {} time(s), expected at least {}",
                                    key, count, kmin
                                ),
                            ));
                        }
                        if count > kmax {
                            let (line, col) = children
                                .iter()
                                .find(|c| child_key_matches(c, ast, table, key))
                                .and_then(|c| child_start_pos(c, ast))
                                .unwrap_or((block_line, block_col));
                            errors.push(ValidationError::from_code_with(
                                &error_codes::CW242_WRONG_NUMBER,
                                max_sev,
                                file_path,
                                line,
                                col,
                                format!(
                                    "Field '{}' appears {} time(s), expected at most {}",
                                    key, count, kmax
                                ),
                            ));
                        }
                    }
                }
            }
            RuleType::LeafValueRule { right } => {
                let count = leafvalue_counts[rule_idx] as i32;
                if count < opts.min && opts.strict_min {
                    errors.push(ValidationError::from_code_with(
                        &error_codes::CW242_WRONG_NUMBER,
                        missing_sev,
                        file_path,
                        block_line,
                        block_col,
                        format!(
                            "LeafValue {:?} appears {} time(s), expected at least {}",
                            right, count, opts.min
                        ),
                    ));
                }
                if count > opts.max {
                    errors.push(ValidationError::from_code_with(
                        &error_codes::CW242_WRONG_NUMBER,
                        max_sev,
                        file_path,
                        block_line,
                        block_col,
                        format!(
                            "LeafValue {:?} appears {} time(s), expected at most {}",
                            right, count, opts.max
                        ),
                    ));
                }
            }
            RuleType::ValueClauseRule { .. } => {
                let count = valueclause_counts[rule_idx] as i32;
                if count < opts.min && opts.strict_min {
                    errors.push(ValidationError::from_code_with(
                        &error_codes::CW242_WRONG_NUMBER,
                        missing_sev,
                        file_path,
                        block_line,
                        block_col,
                        format!(
                            "ValueClause appears {} time(s), expected at least {}",
                            count, opts.min
                        ),
                    ));
                }
                if count > opts.max {
                    errors.push(ValidationError::from_code_with(
                        &error_codes::CW242_WRONG_NUMBER,
                        max_sev,
                        file_path,
                        block_line,
                        block_col,
                        format!(
                            "ValueClause appears {} time(s), expected at most {}",
                            count, opts.max
                        ),
                    ));
                }
            }
            _ => {}
        }
    }
}
