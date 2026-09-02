use cwtools_game::scope_engine::ScopeContext;
use cwtools_parser::ast::{Child, SourcePos, Value};
use cwtools_rules::rules_types::*;
use cwtools_string_table::string_table::StringTable;
use smallvec::SmallVec;

use crate::common::*;
use crate::ctx::ValidationCtx;
use crate::scope::{enter_block_scope, scope_matches_required};
use cwtools_error_codes as error_codes;

use super::children::{rule_right_is_math_expr, validate_children};
use super::leaf::validate_leaf;
use super::matching::{PatternMatch, classify_pattern_match, is_scope_key};

type Overloads<'a> = SmallVec<[(&'a (RuleType, Options), bool); 4]>;

const EQUIVALENT_OVERLOAD_SCAN_CAP: usize = 32;

fn push_overload<'a>(
    overloads: &mut Overloads<'a>,
    rule: &'a (RuleType, Options),
    confident: bool,
    coalesce_equivalent: bool,
) {
    if coalesce_equivalent
        && overloads.len() < EQUIVALENT_OVERLOAD_SCAN_CAP
        && let Some((_, existing_confident)) =
            overloads.iter_mut().find(|(existing, _)| *existing == rule)
    {
        *existing_confident |= confident;
        return;
    }
    overloads.push((rule, confident));
}

pub(crate) fn alias_overloads<'a>(
    ruleset: &'a RuleSet,
    type_index: Option<&cwtools_index::TypeIndex>,
    category: &str,
    key: &str,
) -> Vec<&'a (RuleType, Options)> {
    alias_overloads_with_confidence(ruleset, type_index, category, key, false)
        .into_iter()
        .map(|(rule, _)| rule)
        .collect()
}

pub(crate) fn alias_overloads_with_confidence<'a>(
    ruleset: &'a RuleSet,
    type_index: Option<&cwtools_index::TypeIndex>,
    category: &str,
    key: &str,
    coalesce_equivalent: bool,
) -> Overloads<'a> {
    let mut overloads: Overloads<'a> = SmallVec::new();
    if let Some(idxs) = ruleset.alias_exact().get(category).and_then(|m| m.get(key)) {
        for &i in idxs {
            push_overload(
                &mut overloads,
                &ruleset.aliases[i].1,
                true,
                coalesce_equivalent,
            );
        }
    }
    if overloads.is_empty() && key.bytes().any(|b| b.is_ascii_uppercase()) {
        let lower = key.to_ascii_lowercase();
        if let Some(idxs) = ruleset
            .alias_exact()
            .get(category)
            .and_then(|m| m.get(lower.as_str()))
        {
            for &i in idxs {
                push_overload(
                    &mut overloads,
                    &ruleset.aliases[i].1,
                    true,
                    coalesce_equivalent,
                );
            }
        }
    }
    if let Some(cat) = ruleset.alias_categories().get(category) {
        for pat in &cat.parsed_patterns {
            match classify_pattern_match(pat, key, ruleset, type_index) {
                PatternMatch::Confident => push_overload(
                    &mut overloads,
                    &ruleset.aliases[pat.alias_idx].1,
                    true,
                    coalesce_equivalent,
                ),
                PatternMatch::PermissiveOnly => push_overload(
                    &mut overloads,
                    &ruleset.aliases[pat.alias_idx].1,
                    false,
                    coalesce_equivalent,
                ),
                PatternMatch::No => {}
            }
        }
        if let Some(sf_idx) = cat.scope_field_idx
            && is_scope_key(key, ruleset, type_index)
        {
            push_overload(
                &mut overloads,
                &ruleset.aliases[sf_idx].1,
                true,
                coalesce_equivalent,
            );
        }
    }
    overloads
}

fn loop_var_default(key: &str) -> Option<&'static str> {
    match key {
        "value" => Some("v"),
        "index" => Some("i"),
        "break" => Some("break"),
        _ => None,
    }
}

fn collect_loop_vars(
    alias_inner: &[(RuleType, Options)],
    children: &[Child],
    ast: &cwtools_parser::ast::ParsedFile,
    table: &StringTable,
) -> Vec<String> {
    let mut seeded: Vec<String> = Vec::new();
    for (rule, _) in alias_inner {
        let RuleType::LeafRule {
            left: NewField::SpecificField(key),
            right: NewField::VariableSetField(_),
        } = rule
        else {
            continue;
        };
        let Some(default) = loop_var_default(key.as_str()) else {
            continue;
        };
        seeded.push(cwtools_index::VarIndex::normalize(default));
        for child in children {
            let Child::Leaf(idx) = child else { continue };
            let leaf = &ast.arena.leaves[*idx as usize];
            let matches_key = table
                .with_string(leaf.key.normal, |s| {
                    unquote_key(s).eq_ignore_ascii_case(key)
                })
                .unwrap_or(false);
            if matches_key {
                let norm = with_leaf_value_str(&leaf.value, table, |name| {
                    cwtools_index::VarIndex::normalize(name)
                });
                if !norm.is_empty() {
                    seeded.push(norm);
                }
            }
        }
    }
    seeded
}

#[allow(clippy::too_many_arguments)]
#[tracing::instrument(level = "trace", skip_all)]
pub(super) fn validate_alias_usage(
    ctx: &ValidationCtx,
    category: &str,
    key: &str,
    leaf: Option<&cwtools_parser::ast::Leaf>,
    clause_children: Option<&[Child]>,
    fallback_pos: (u32, u16),
    scope_context: &mut Option<ScopeContext>,
    errors: &mut Vec<ValidationError>,
) {
    let Some(memo_key) = ctx.alias_memo_key(category, key, leaf, clause_children, scope_context)
    else {
        validate_alias_usage_uncached(
            ctx,
            category,
            key,
            leaf,
            clause_children,
            fallback_pos,
            scope_context,
            errors,
        );
        return;
    };
    if ctx.alias_memo_replay(&memo_key, errors) {
        return;
    }
    let mut produced = Vec::new();
    validate_alias_usage_uncached(
        ctx,
        category,
        key,
        leaf,
        clause_children,
        fallback_pos,
        scope_context,
        &mut produced,
    );
    if !ctx.alias_branch_budget_exhausted() {
        ctx.alias_memo_store(memo_key, &produced);
    }
    errors.append(&mut produced);
}

#[allow(clippy::too_many_arguments)]
fn validate_alias_usage_uncached(
    ctx: &ValidationCtx,
    category: &str,
    key: &str,
    leaf: Option<&cwtools_parser::ast::Leaf>,
    clause_children: Option<&[Child]>,
    fallback_pos: (u32, u16),
    scope_context: &mut Option<ScopeContext>,
    errors: &mut Vec<ValidationError>,
) {
    if ctx.alias_branch_budget_exhausted() {
        return;
    }
    let table = ctx.table;
    let file_path = ctx.file_path;
    let ruleset = ctx.ruleset;
    let overloads_conf =
        alias_overloads_with_confidence(ruleset, ctx.type_index, category, key, true);
    if overloads_conf.is_empty() {
        return;
    }
    let key_end = leaf.map(|l| key_token_end(l, key, table));
    if overloads_conf.len() > 1 {
        let branch_pos = leaf.map(|leaf| leaf.pos.start).unwrap_or(SourcePos {
            line: fallback_pos.0,
            col: fallback_pos.1,
        });
        if !ctx.reserve_alias_branches(overloads_conf.len(), branch_pos, key_end) {
            return;
        }
    }

    if ctx.scope_checks
        && key.contains('.')
        && !looks_like_data_ref(key)
        && let Some(sc) = scope_context.as_mut()
        && !sc.registry.is_empty()
    {
        let saved = sc.save();
        let result = sc.change_scope(key);
        sc.restore(saved);
        if matches!(result, cwtools_game::scope_engine::ScopeResult::NotFound) {
            let code = &error_codes::CW248_INVALID_SCOPE_COMMAND;
            let (line, col) = leaf
                .map(|l| (l.pos.start.line, l.pos.start.col))
                .unwrap_or(fallback_pos);
            let mut err = ValidationError::from_code(code, file_path, line, col, &[key]);
            if let Some(end) = key_end {
                err = err.with_end(end);
            }
            errors.push(err);
        }
    }

    if ctx.scope_checks
        && category != "modifier"
        && let Some(sc) = scope_context.as_ref()
    {
        let reg = sc.registry.as_ref();
        let current = sc.current();
        let mut any_confident = false;
        let mut any_ok = false;
        for &((_, opts), confident) in &overloads_conf {
            if !confident {
                continue;
            }
            any_confident = true;
            if scope_matches_required(current, reg, &opts.required_scopes) {
                any_ok = true;
                break;
            }
        }
        if any_confident && !any_ok {
            let mut expected: Vec<String> = overloads_conf
                .iter()
                .filter(|(_, c)| *c)
                .flat_map(|((_, o), _)| o.required_scopes.iter().cloned())
                .collect();
            expected.sort_unstable();
            expected.dedup();
            let code = match category {
                "trigger" => &error_codes::CW104_INCORRECT_TRIGGER_SCOPE,
                "effect" => &error_codes::CW105_INCORRECT_EFFECT_SCOPE,
                _ => &error_codes::CW106_INCORRECT_SCOPE_SCOPE,
            };
            let (line, col) = leaf
                .map(|l| (l.pos.start.line, l.pos.start.col))
                .unwrap_or(fallback_pos);
            let mut err = ValidationError::from_code(
                code,
                file_path,
                line,
                col,
                &[key, &reg.name_of(current), &expected.join(" or ")],
            );
            if let Some(end) = key_end {
                err = err.with_end(end);
            }
            errors.push(err);
        }
    }

    if let Some(leaf) = leaf
        && matches!(&leaf.value, Value::Clause(_))
        && let Some(((mrt, _), _)) = overloads_conf
            .iter()
            .find(|((rt, _), _)| rule_right_is_math_expr(rt))
    {
        validate_leaf(ctx, leaf, mrt, scope_context.as_ref(), errors);
        return;
    }

    let mut best: Option<Vec<ValidationError>> = None;
    let mut only_match: Option<ValidationError> = None;
    let mut temp: Vec<ValidationError> = Vec::new();
    for &((rule_type, opts), _) in &overloads_conf {
        temp.clear();
        match rule_type {
            RuleType::LeafRule { .. } => {
                if let Some(leaf) = leaf {
                    validate_leaf(ctx, leaf, rule_type, scope_context.as_ref(), &mut temp);
                } else {
                    let (line, col) = fallback_pos;
                    temp.push(alias_mismatch_error(
                        file_path, category, "{...}", line, col, None,
                    ));
                }
            }
            RuleType::NodeRule {
                rules: alias_inner, ..
            } => {
                let children = clause_children.or_else(|| match leaf.map(|l| &l.value) {
                    Some(Value::Clause(ch)) => Some(ch.as_slice()),
                    _ => None,
                });
                if let Some(children) = children {
                    let saved = scope_context.as_ref().map(|sc| sc.save());
                    if let Some(sc) = scope_context.as_mut() {
                        enter_block_scope(sc, key, opts, ctx.game, true, ctx.type_index);
                    }
                    let loop_var_base = ctx.loop_vars.borrow().len();
                    let seeded = collect_loop_vars(alias_inner, children, ctx.ast, table);
                    if !seeded.is_empty() {
                        ctx.loop_vars.borrow_mut().extend(seeded);
                    }
                    validate_children(
                        ctx,
                        children,
                        alias_inner,
                        scope_context,
                        leaf.map(|l| (l.pos.start.line, l.pos.start.col))
                            .unwrap_or(fallback_pos),
                        &mut temp,
                    );
                    ctx.loop_vars.borrow_mut().truncate(loop_var_base);
                    if let (Some(saved), Some(sc)) = (saved, scope_context.as_mut()) {
                        sc.restore(saved);
                    }
                } else {
                    let (value, line, col) = leaf
                        .map(|l| {
                            (
                                leaf_value_to_string(&l.value, table),
                                l.pos.start.line,
                                l.pos.start.col,
                            )
                        })
                        .unwrap_or_else(|| (String::new(), fallback_pos.0, fallback_pos.1));
                    temp.push(alias_mismatch_error(
                        file_path, category, &value, line, col, key_end,
                    ));
                }
            }
            _ => continue,
        }

        if temp.is_empty() {
            if let Some(msg) = opts.error_if_only_match.as_ref() {
                if only_match.is_none() {
                    let sev = opts
                        .severity
                        .as_ref()
                        .map(severity_to_error)
                        .unwrap_or(ErrorSeverity::Error);
                    let (line, col) = leaf
                        .map(|l| (l.pos.start.line, l.pos.start.col))
                        .unwrap_or(fallback_pos);
                    let mut custom = ValidationError::from_code_with(
                        &error_codes::CW272_FROM_RULES_CUSTOM_ERROR,
                        sev,
                        file_path,
                        line,
                        col,
                        msg.clone(),
                    );
                    if let Some(end) = key_end {
                        custom = custom.with_end(end);
                    }
                    only_match = Some(custom);
                }
                continue;
            }
            return; // clean match — accept with no errors
        }
        match &best {
            Some(b) if b.len() <= temp.len() => {}
            _ => best = Some(std::mem::take(&mut temp)),
        }
    }

    if let Some(custom) = only_match {
        errors.push(custom);
    } else if let Some(b) = best {
        errors.extend(b);
    }
}

fn alias_mismatch_error(
    file_path: &crate::FilePath,
    category: &str,
    value: &str,
    line: u32,
    col: u16,
    end: Option<SourcePos>,
) -> ValidationError {
    let code = &error_codes::CW267_UNEXPECTED_ALIAS_KEY_VALUE;
    let err = ValidationError::from_code(code, file_path, line, col, &[category, value]);
    match end {
        Some(end) => err.with_end(end),
        None => err,
    }
}

#[cfg(test)]
mod tests {
    use crate::{Prepared, ValidationError, build_scope_registry_arc, validate_prepared_inner};
    use cwtools_parser::parser::parse_string;
    use cwtools_rules::rules_converter::ast_to_ruleset;
    use cwtools_string_table::string_table::StringTable;

    const RECURSIVE_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
## severity = warning
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
"#;

    fn recursive_script(depth: usize) -> String {
        let mut script = String::from("foo = {\n");
        for _ in 0..depth {
            script.push_str("recurse = {\n");
        }
        script.push_str("bad = nope\n");
        for _ in 0..=depth {
            script.push_str("}\n");
        }
        script
    }

    fn branches_for(depth: usize) -> (Vec<ValidationError>, usize) {
        let table = StringTable::new();
        let ruleset = ast_to_ruleset(&parse_string(RECURSIVE_RULES, &table), &table);
        let parsed = parse_string(&recursive_script(depth), &table);
        let registry = build_scope_registry_arc(&ruleset, None);
        let prepared = Prepared {
            ruleset: &ruleset,
            table: &table,
            game: None,
            type_index: None,
            modifier_keys: None,
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: registry.as_ref(),
            scope_checks: false,
            var_checks: false,
        };
        validate_prepared_inner(&parsed, "game/common/foo/test.txt", &prepared, None)
    }

    #[test]
    fn recursive_overloads_memoize_instead_of_exhausting_the_budget() {
        let (errors, branches) = branches_for(20);
        assert!(
            errors.iter().all(|error| error.code != Some("CW277")),
            "the memo must keep the file under the branch limit: {errors:?}"
        );
        assert!(
            errors.iter().any(|error| error.code == Some("CW263")),
            "the file must validate to the deepest invalid field: {errors:?}"
        );
        assert!(
            branches < 65_536,
            "expected fewer branches than the budget, evaluated {branches}"
        );
    }

    #[test]
    fn memoized_recursion_does_not_grow_exponentially_with_depth() {
        let (_, shallow) = branches_for(20);
        let (_, deeper) = branches_for(24);
        assert!(
            deeper - shallow <= 32,
            "four more levels should cost a few branches, went from {shallow} to {deeper}"
        );
    }
}
