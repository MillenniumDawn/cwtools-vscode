use cwtools_game::scope_engine::ScopeContext;
use cwtools_parser::ast::Child;
use cwtools_rules::rules_types::*;
use rustc_hash::FxHashSet;
use std::borrow::Cow;

use crate::common::*;
use crate::ctx::ValidationCtx;
use crate::scope::seed_root_scope;
use crate::subtype::subtype_matches;

use super::children::validate_children;

#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_with_type(
    ctx: &ValidationCtx,
    type_def: &TypeDefinition,
    children: &[Child],
    inner_rules: &[(RuleType, Options)],
    scope_context: &mut Option<ScopeContext>,
    node_key: Option<&str>,
    node_pos: (u32, u16),
    errors: &mut Vec<ValidationError>,
) {
    if ctx.alias_branch_budget_exhausted() {
        return;
    }
    let game = ctx.game;
    let ruleset = ctx.ruleset;
    if type_def.subtypes.is_empty() {
        let pre_count = errors.len();
        let saved = scope_context.as_ref().map(|sc| sc.save());
        if let Some(sc) = scope_context.as_mut() {
            seed_root_scope(sc, type_def, None, node_key, ruleset, game);
        }
        validate_children(ctx, children, inner_rules, scope_context, node_pos, errors);
        if let (Some(saved), Some(sc)) = (saved, scope_context.as_mut()) {
            sc.restore(saved);
        }
        if type_def.warning_only {
            for err in errors[pre_count..].iter_mut() {
                if err.severity == ErrorSeverity::Error {
                    err.severity = ErrorSeverity::Warning;
                }
            }
        }
        return;
    }

    let (merged, matched_subtype_names, push_scope) =
        merged_rules_for_type(ctx, type_def, children, inner_rules, node_key, false);

    if matched_subtype_names.is_empty() && merged.is_empty() {
        return;
    }

    let saved = scope_context.as_ref().map(|sc| sc.save());
    if let Some(sc) = scope_context.as_mut() {
        seed_root_scope(sc, type_def, push_scope, node_key, ruleset, game);
    }

    let pre_count = errors.len();
    validate_children(
        ctx,
        children,
        merged.as_ref(),
        scope_context,
        node_pos,
        errors,
    );

    if type_def.warning_only {
        for err in errors[pre_count..].iter_mut() {
            if err.severity == ErrorSeverity::Error {
                err.severity = ErrorSeverity::Warning;
            }
        }
    }

    if let (Some(saved), Some(sc)) = (saved, scope_context.as_mut()) {
        sc.restore(saved);
    }
}

pub(crate) type MergedTypeRules<'a> = (
    Cow<'a, [(RuleType, Options)]>,
    Vec<&'a str>,
    Option<&'a str>,
);

#[tracing::instrument(skip_all, fields(type_name = %type_def.name))]
pub(crate) fn merged_rules_for_type<'a>(
    ctx: &ValidationCtx,
    type_def: &'a TypeDefinition,
    children: &[Child],
    inner_rules: &'a [(RuleType, Options)],
    node_key: Option<&str>,
    union_all_subtypes: bool,
) -> MergedTypeRules<'a> {
    if type_def.subtypes.is_empty() {
        return (Cow::Borrowed(inner_rules), Vec::new(), None);
    }

    let mut matched_subtype_names: Vec<&str> = Vec::new();
    for subtype in &type_def.subtypes {
        if subtype_matches(
            subtype,
            children,
            ctx.ast,
            ctx.table,
            ctx.ruleset,
            node_key,
            ctx.type_index,
        ) {
            matched_subtype_names.push(subtype.name.as_str());
        }
    }
    let all_names_copy: FxHashSet<&str> = matched_subtype_names.iter().copied().collect();
    matched_subtype_names.retain(|name| {
        let st = type_def.subtypes.iter().find(|s| s.name == *name).unwrap();
        !st.only_if_not
            .iter()
            .any(|excl| all_names_copy.contains(excl.as_str()))
    });

    if union_all_subtypes {
        let merged = all_subtype_rules_union(type_def, inner_rules);
        let push_scope = first_matching_push_scope(type_def, &matched_subtype_names);
        return (merged, matched_subtype_names, push_scope);
    }

    let inner_has_subtype_rules = inner_rules
        .iter()
        .any(|(rt, _)| matches!(rt, RuleType::SubtypeRule { .. }));

    let merged: Cow<'_, [(RuleType, Options)]>;
    if inner_has_subtype_rules {
        let mut v: Vec<(RuleType, Options)> = Vec::new();
        for (rule_type, opts) in inner_rules {
            match rule_type {
                RuleType::SubtypeRule {
                    name,
                    positive,
                    rules: st_rules,
                } => {
                    let is_active = matched_subtype_names.contains(&name.as_str());
                    let should_include = if *positive { is_active } else { !is_active };
                    if should_include {
                        v.extend(st_rules.iter().map(|(rt, o)| {
                            let mut o2 = o.clone();
                            o2.min = 0;
                            (rt.clone(), o2)
                        }));
                    }
                }
                _ => {
                    v.push((rule_type.clone(), opts.clone()));
                }
            }
        }
        merged = Cow::Owned(v);
    } else {
        let extra_rules_needed = type_def
            .subtypes
            .iter()
            .any(|s| matched_subtype_names.contains(&s.name.as_str()) && !s.rules.is_empty());
        if extra_rules_needed {
            let mut v: Vec<(RuleType, Options)> = inner_rules.to_vec();
            for subtype in &type_def.subtypes {
                if matched_subtype_names.contains(&subtype.name.as_str()) {
                    v.extend(subtype.rules.iter().map(|(rt, o)| {
                        let mut o2 = o.clone();
                        o2.min = 0;
                        (rt.clone(), o2)
                    }));
                }
            }
            merged = Cow::Owned(v);
        } else {
            merged = Cow::Borrowed(inner_rules);
        }
    }

    let push_scope = first_matching_push_scope(type_def, &matched_subtype_names);

    (merged, matched_subtype_names, push_scope)
}

fn first_matching_push_scope<'a>(
    type_def: &'a TypeDefinition,
    matched_subtype_names: &[&str],
) -> Option<&'a str> {
    type_def
        .subtypes
        .iter()
        .filter(|s| matched_subtype_names.contains(&s.name.as_str()))
        .find_map(|s| s.push_scope.as_deref())
}

fn all_subtype_rules_union<'a>(
    type_def: &TypeDefinition,
    inner_rules: &[(RuleType, Options)],
) -> Cow<'a, [(RuleType, Options)]> {
    let mut v: Vec<(RuleType, Options)> = Vec::with_capacity(inner_rules.len());
    for (rt, opts) in inner_rules {
        if let RuleType::SubtypeRule {
            rules: st_rules, ..
        } = rt
        {
            v.extend(st_rules.iter().map(zero_min));
        } else {
            v.push((rt.clone(), opts.clone()));
        }
    }
    for subtype in &type_def.subtypes {
        v.extend(subtype.rules.iter().map(zero_min));
    }
    Cow::Owned(v)
}

fn zero_min((rt, opts): &(RuleType, Options)) -> (RuleType, Options) {
    let mut o = opts.clone();
    o.min = 0;
    (rt.clone(), o)
}

pub(crate) fn flatten_nested_subtype_rules(
    rules: &[(RuleType, Options)],
) -> Vec<(RuleType, Options)> {
    let mut out: Vec<(RuleType, Options)> = Vec::with_capacity(rules.len());
    for (rt, opts) in rules {
        if let RuleType::SubtypeRule {
            rules: st_rules, ..
        } = rt
        {
            out.extend(flatten_nested_subtype_rules(st_rules));
        } else {
            out.push((rt.clone(), opts.clone()));
        }
    }
    out
}
