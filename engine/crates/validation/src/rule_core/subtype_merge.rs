use cwtools_game::scope_engine::ScopeContext;
use cwtools_parser::ast::Child;
use cwtools_rules::rules_types::*;
use rustc_hash::FxHashSet;
use std::sync::Arc;

use crate::common::*;
use crate::ctx::{SubtypeMergeMemoKey, ValidationCtx};
use crate::scope::seed_root_scope;
use crate::subtype::{SubtypeMatchContext, subtype_matches};

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

pub(crate) enum MergedRules<'a> {
    Borrowed(&'a [(RuleType, Options)]),
    Shared(Arc<[(RuleType, Options)]>),
}

impl MergedRules<'_> {
    pub(crate) fn as_ref(&self) -> &[(RuleType, Options)] {
        match self {
            Self::Borrowed(rules) => rules,
            Self::Shared(rules) => rules,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.as_ref().is_empty()
    }
}

pub(crate) type MergedTypeRules<'a> = (MergedRules<'a>, Vec<&'a str>, Option<&'a str>);

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
        return (MergedRules::Borrowed(inner_rules), Vec::new(), None);
    }

    let mut matched_subtype_indices: Vec<usize> = Vec::new();
    for (index, subtype) in type_def.subtypes.iter().enumerate() {
        let match_context = SubtypeMatchContext::for_subtype(ctx.ruleset, type_def, subtype, index);
        if subtype_matches(
            subtype,
            children,
            ctx.ast,
            ctx.table,
            &match_context,
            node_key,
            ctx.type_index,
        ) {
            matched_subtype_indices.push(index);
        }
    }
    let all_matched_names: FxHashSet<&str> = matched_subtype_indices
        .iter()
        .map(|&index| type_def.subtypes[index].name.as_str())
        .collect();
    matched_subtype_indices.retain(|&index| {
        !type_def.subtypes[index]
            .only_if_not
            .iter()
            .any(|excluded| all_matched_names.contains(excluded.as_str()))
    });
    let matched_subtype_names: Vec<&str> = matched_subtype_indices
        .iter()
        .map(|&index| type_def.subtypes[index].name.as_str())
        .collect();

    if union_all_subtypes {
        let merged = memoized_merge(
            ctx,
            type_def,
            inner_rules,
            &matched_subtype_indices,
            true,
            || all_subtype_rules_union(type_def, inner_rules),
        );
        let push_scope = first_matching_push_scope(type_def, &matched_subtype_names);
        return (
            MergedRules::Shared(merged),
            matched_subtype_names,
            push_scope,
        );
    }

    let inner_has_subtype_rules = inner_rules
        .iter()
        .any(|(rt, _)| matches!(rt, RuleType::SubtypeRule { .. }));
    let active_subtype_names: FxHashSet<&str> = matched_subtype_names.iter().copied().collect();

    let merged = if inner_has_subtype_rules {
        let merged = memoized_merge(
            ctx,
            type_def,
            inner_rules,
            &matched_subtype_indices,
            false,
            || {
                let mut merged = Vec::new();
                for (rule_type, opts) in inner_rules {
                    match rule_type {
                        RuleType::SubtypeRule {
                            name,
                            positive,
                            rules: subtype_rules,
                        } => {
                            let is_active = active_subtype_names.contains(name.as_str());
                            let should_include = if *positive { is_active } else { !is_active };
                            if should_include {
                                merged.extend(subtype_rules.iter().map(zero_min));
                            }
                        }
                        _ => merged.push((rule_type.clone(), opts.clone())),
                    }
                }
                merged
            },
        );
        MergedRules::Shared(merged)
    } else {
        let extra_rules_needed = matched_subtype_indices
            .iter()
            .any(|&index| !type_def.subtypes[index].rules.is_empty());
        if extra_rules_needed {
            let merged = memoized_merge(
                ctx,
                type_def,
                inner_rules,
                &matched_subtype_indices,
                false,
                || {
                    let mut merged = inner_rules.to_vec();
                    for &index in &matched_subtype_indices {
                        merged.extend(type_def.subtypes[index].rules.iter().map(zero_min));
                    }
                    merged
                },
            );
            MergedRules::Shared(merged)
        } else {
            MergedRules::Borrowed(inner_rules)
        }
    };

    let push_scope = first_matching_push_scope(type_def, &matched_subtype_names);
    (merged, matched_subtype_names, push_scope)
}

fn memoized_merge(
    ctx: &ValidationCtx,
    type_def: &TypeDefinition,
    inner_rules: &[(RuleType, Options)],
    matched_subtype_indices: &[usize],
    union_all_subtypes: bool,
    build: impl FnOnce() -> Vec<(RuleType, Options)>,
) -> Arc<[(RuleType, Options)]> {
    let key = SubtypeMergeMemoKey {
        type_identity: type_def as *const TypeDefinition as usize,
        inner_rules_identity: (inner_rules.as_ptr() as usize, inner_rules.len()),
        matched_subtype_indices: matched_subtype_indices.iter().copied().collect(),
        union_all_subtypes,
    };
    if let Some(cached) = ctx.subtype_merge_memo.borrow().entries.get(&key) {
        return Arc::clone(cached);
    }

    let merged = Arc::from(build());
    ctx.subtype_merge_memo
        .borrow_mut()
        .entries
        .insert(key, Arc::clone(&merged));
    merged
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

fn all_subtype_rules_union(
    type_def: &TypeDefinition,
    inner_rules: &[(RuleType, Options)],
) -> Vec<(RuleType, Options)> {
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
    v
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::{AliasBranchBudget, InlineScriptExpansionBudget};
    use cwtools_parser::parser::parse_string;
    use cwtools_rules::rules_converter::ast_to_ruleset;
    use cwtools_string_table::string_table::StringTable;
    use std::cell::RefCell;

    fn names(rules: &[(RuleType, Options)]) -> Vec<&str> {
        rules
            .iter()
            .filter_map(|(rule, _)| match rule {
                RuleType::LeafRule {
                    left: NewField::SpecificField(key),
                    ..
                } => Some(key.as_str()),
                _ => None,
            })
            .collect()
    }

    fn type_rules<'a>(ruleset: &'a RuleSet, name: &str) -> &'a [(RuleType, Options)] {
        ruleset
            .root_rules
            .iter()
            .find_map(|root| match root {
                RootRule::TypeRule(root_name, (RuleType::NodeRule { rules, .. }, _))
                    if root_name == name =>
                {
                    Some(rules.as_ref())
                }
                _ => None,
            })
            .expect("fixture type has rules")
    }

    fn shared_rules<'a, 'b>(rules: &'a MergedRules<'b>) -> &'a Arc<[(RuleType, Options)]> {
        match rules {
            MergedRules::Shared(rules) => rules,
            MergedRules::Borrowed(_) => panic!("expected memoized merged rules"),
        }
    }

    #[test]
    fn subtype_merge_memo_separates_type_inner_rules_matches_and_union_mode() {
        let table = StringTable::new();
        let parsed_rules = parse_string(
            r#"
                types = {
                    type[foo] = {
                        path = "common/foo"
                        ## push_scope = country
                        subtype[alpha] = { kind = alpha }
                        ## only_if_not = alpha
                        ## push_scope = character
                        subtype[beta] = { other = beta }
                    }
                    type[bar] = {
                        path = "common/bar"
                        subtype[alpha] = { kind = alpha }
                        subtype[beta] = { kind = beta }
                    }
                    type[plain] = { path = "common/plain" }
                }
                foo = {
                    common = int
                    subtype[alpha] = {
                        ## cardinality = 2..2
                        alpha_value = int
                    }
                    subtype[beta] = { beta_value = int }
                    subtype[!beta] = { fallback = int }
                }
                bar = {
                    common = int
                    subtype[alpha] = { bar_alpha = int }
                    subtype[beta] = { bar_beta = int }
                }
                plain = { common = int }
            "#,
            &table,
        );
        let ruleset = ast_to_ruleset(&parsed_rules, &table);
        let ast = parse_string("kind = alpha\nother = beta\n", &table);
        let file_path: crate::common::FilePath = Arc::from("common/foo/test.txt");
        let alias_branch_budget = RefCell::new(AliasBranchBudget::default());
        let inline_script_expansion_budget = RefCell::new(InlineScriptExpansionBudget::default());
        let inline_stack = RefCell::new(Vec::new());
        let ctx = ValidationCtx {
            ast: &ast,
            ruleset: &ruleset,
            table: &table,
            file_path: &file_path,
            game: None,
            type_index: None,
            modifier_keys: None,
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            scope_checks: false,
            var_checks: false,
            loop_vars: RefCell::new(Vec::new()),
            alias_branch_budget: &alias_branch_budget,
            inline_script_expansion_budget: &inline_script_expansion_budget,
            inline_stack: &inline_stack,
            alias_memo: RefCell::new(crate::ctx::AliasMemo::default()),
            subtype_merge_memo: RefCell::new(crate::ctx::SubtypeMergeMemo::default()),
            type_uses: None,
        };
        let foo = ruleset.types.iter().find(|td| td.name == "foo").unwrap();
        let bar = ruleset.types.iter().find(|td| td.name == "bar").unwrap();
        let plain = ruleset.types.iter().find(|td| td.name == "plain").unwrap();
        let foo_inner = type_rules(&ruleset, "foo");
        let plain_inner = type_rules(&ruleset, "plain");
        let alpha_children = &ast.root_children[..1];
        let beta_children = &ast.root_children[1..];

        let first = merged_rules_for_type(&ctx, foo, alpha_children, foo_inner, None, false);
        assert_eq!(first.1, ["alpha"]);
        assert_eq!(first.2, Some("country"));
        assert!(names(first.0.as_ref()).contains(&"alpha_value"));
        assert!(names(first.0.as_ref()).contains(&"fallback"));
        assert!(!names(first.0.as_ref()).contains(&"beta_value"));
        assert_eq!(
            first
                .0
                .as_ref()
                .iter()
                .find(|(rule, _)| matches!(rule, RuleType::LeafRule { left: NewField::SpecificField(key), .. } if key == "alpha_value"))
                .map(|(_, options)| options.min),
            Some(0),
            "merged subtype rules retain the min=0 transformation"
        );

        let repeated = merged_rules_for_type(&ctx, foo, alpha_children, foo_inner, None, false);
        assert!(Arc::ptr_eq(
            shared_rules(&first.0),
            shared_rules(&repeated.0)
        ));

        let beta = merged_rules_for_type(&ctx, foo, beta_children, foo_inner, None, false);
        assert_eq!(beta.1, ["beta"]);
        assert_eq!(beta.2, Some("character"));
        assert!(names(beta.0.as_ref()).contains(&"beta_value"));
        assert!(!names(beta.0.as_ref()).contains(&"fallback"));
        assert!(!Arc::ptr_eq(shared_rules(&first.0), shared_rules(&beta.0)));

        let copied_inner = foo_inner.to_vec();
        let other_inner_list =
            merged_rules_for_type(&ctx, foo, alpha_children, &copied_inner, None, false);
        assert!(!Arc::ptr_eq(
            shared_rules(&first.0),
            shared_rules(&other_inner_list.0)
        ));

        let other_type = merged_rules_for_type(&ctx, bar, alpha_children, foo_inner, None, false);
        assert!(!Arc::ptr_eq(
            shared_rules(&first.0),
            shared_rules(&other_type.0)
        ));
        assert!(names(other_type.0.as_ref()).contains(&"alpha_value"));

        let union = merged_rules_for_type(&ctx, foo, alpha_children, foo_inner, None, true);
        assert!(!Arc::ptr_eq(shared_rules(&first.0), shared_rules(&union.0)));
        assert!(names(union.0.as_ref()).contains(&"alpha_value"));
        assert!(names(union.0.as_ref()).contains(&"beta_value"));

        let both_children = &ast.root_children[..];
        let only_if_not = merged_rules_for_type(&ctx, foo, both_children, foo_inner, None, false);
        assert_eq!(only_if_not.1, ["alpha"]);
        assert_eq!(only_if_not.2, Some("country"));

        let unchanged =
            merged_rules_for_type(&ctx, plain, alpha_children, plain_inner, None, false);
        assert!(matches!(unchanged.0, MergedRules::Borrowed(_)));
        assert_eq!(unchanged.0.as_ref().as_ptr(), plain_inner.as_ptr());
    }
}
