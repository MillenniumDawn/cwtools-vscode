//! Alias-usage validation: resolving `alias_name[cat]` overloads and validating a
//! usage against every overload as a disjunction.

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

/// The overload set a single alias usage resolves to, each tagged `confident`.
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

/// Gather every alias overload `alias[cat:key]` that the usage `key` resolves
/// to: exact name, lowercase retry, `<type>`/`value[..]`/`enum[..]` patterns,
/// and the category's `scope_field` entry for scope-switching keys.
///
/// Shared between alias validation (below) and the position resolver
/// (`crate::position`) so both resolve aliases in the same priority order.
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

/// Gather every alias overload for `key` in a single pass, tagging each with
/// whether the match is `confident` (verified against populated backing data)
/// vs a permissive-only pattern match (backing enum/value set absent/empty).
///
/// A confident pattern match (`enum[..]` / `value[..]` / `<type>` matched against
/// populated data, never the empty/absent permissive fallback) is what the scope
/// check trusts, so a coincidental match against an unpopulated game-derived enum
/// (e.g. `oil` against an empty `enum[equipment_category]` when vanilla isn't
/// indexed) doesn't drag in that alias's unrelated `## scope` and flag a false
/// CW104. Exact, lowercase-retry and `scope_field` overloads are always confident.
///
/// Push order is exact → lowercase → patterns → scope_field. Validation
/// coalesces fully equivalent candidates while the set is small; navigation
/// retains every candidate. The scope check filters to the confident subset,
/// preserving order for `pick_best_candidate`'s tie-break.
///
/// A usage resolves to a handful of overloads at most, and this runs for every
/// effect/trigger in the corpus, so the set is inline until it doesn't fit.
pub(crate) fn alias_overloads_with_confidence<'a>(
    ruleset: &'a RuleSet,
    type_index: Option<&cwtools_index::TypeIndex>,
    category: &str,
    key: &str,
    coalesce_equivalent: bool,
) -> Overloads<'a> {
    // Gather candidate overloads via the precomputed alias index (O(1) exact +
    // O(patterns)) rather than scanning every alias.
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
    // Case-insensitive retry: usages like `IF`, `Country_event` resolve to the
    // lowercase alias (config alias names are lowercase). Mirrors the fallback in
    // field_matches_key, which matches the key so the body must validate too.
    // Only allocate the lowercased form when `key` actually has uppercase.
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
            // Classify once: a `Confident` match is included in both sets, a
            // `PermissiveOnly` match only in the permissive (all) set.
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

/// The implicit default temp-variable name a loop-effect field declares when it
/// is omitted: `value` → `v`, `index` → `i`, `break` → `break` (HOI4
/// `for_each_loop` & friends, documented in effects.cwt). Any other key is not a
/// loop-variable binding.
fn loop_var_default(key: &str) -> Option<&'static str> {
    match key {
        "value" => Some("v"),
        "index" => Some("i"),
        "break" => Some("break"),
        _ => None,
    }
}

/// Collect the loop-local variable names a loop-effect block exposes to its body,
/// normalized for the variable index.
///
/// A loop effect (`for_each_loop`, `while_loop`, `every_country`, …) is detected
/// purely from its rule shape: a `value`/`index`/`break` field bound to
/// `value_set[variable]`. For each such field we seed its implicit default name
/// (`v`/`i`/`break`) and, when the block explicitly rebinds it (`value = my_elem`),
/// the explicit name too. Seeding both is the lenient choice and matches the
/// `var:NAME` form already accepted. Returns empty for any non-loop block.
fn collect_loop_vars(
    alias_inner: &[(RuleType, Options)],
    children: &[Child],
    ast: &cwtools_parser::ast::ParsedFile,
    table: &StringTable,
) -> Vec<String> {
    // Which keys this alias declares as `<key> = value_set[variable]`.
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
        // Default name (used when the key is omitted).
        seeded.push(cwtools_index::VarIndex::normalize(default));
        // Explicit rebinding, if the block provides `<key> = NAME`.
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

/// Validate an aliased usage, reusing the result if this exact usage has already
/// been validated in this exact state.
///
/// Recursive overloads make the same subtree reachable through many candidate
/// paths, and validating one is a pure function of the usage and the state it is
/// read in: the AST node, the alias category and key, the scope context, and the
/// loop-local variables in scope (see [`ValidationCtx::alias_memo_key`]). What it
/// leaves behind is its diagnostics, which the entry carries, and its type uses,
/// which are already in the per-file sink from the walk that filled the entry.
/// The scope and loop-variable stacks are restored by the walk itself, so a
/// replay leaves the context exactly where a full walk would.
///
/// A hit skips the branch reservation the walk would have made, which is the
/// point: a file whose repeats are equivalent now finishes instead of stopping
/// at the branch limit. A result produced after the budget ran out is truncated,
/// so it is never stored.
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

/// Validate an aliased usage (`alias_name[cat] = ...`) against EVERY overload
/// declared as `alias[cat:key]`.
///
/// CWT lets the same alias name be defined many times (e.g. two
/// `alias[trigger:original_tag]` — one `scope[country]`, one `enum[country_tags]`
/// — or ~40 `alias[ai_strategy_rule:ai_strategy]` blocks keyed by `type`). A usage
/// is valid if it matches ANY overload (F# cwtools semantics). We therefore try
/// each candidate into a throwaway buffer and accept on the first clean match;
/// only when none match do we surface the closest (fewest-errors) candidate's
/// errors, which is also how the `type = ...` discriminator naturally wins.
#[allow(clippy::too_many_arguments)]
fn validate_alias_usage_uncached(
    ctx: &ValidationCtx,
    category: &str,
    key: &str,
    leaf: Option<&cwtools_parser::ast::Leaf>,
    clause_children: Option<&[Child]>,
    // Position to anchor diagnostics when `leaf` is None (node-form usage).
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
    // Compute the overload set (with per-overload confidence) ONCE; the scope
    // check below reuses the confident subset instead of re-walking the aliases.
    let overloads_conf =
        alias_overloads_with_confidence(ruleset, ctx.type_index, category, key, true);
    if overloads_conf.is_empty() {
        // Category unloaded or no such alias key — accept silently, matching the
        // permissive key-match in field_matches_key.
        return;
    }
    // Advice-about-the-key diagnostics below (scope, shape mismatch, custom
    // error) squiggle the key token, not the whole usage. Node-form usages
    // have no leaf and keep the whole-line fallback.
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

    // CW248: an invalid scope command in a chain. Restricted to dotted lower-case
    // chains (`owner.capital`): a bare command that's missing from this config's
    // links.cwt (e.g. `overlord`) is valid-but-unlisted, not invalid, so only
    // chains — where a segment is genuinely unresolvable — are flagged.
    if ctx.scope_checks
        && key.contains('.')
        && !looks_like_data_ref(key)
        && let Some(sc) = scope_context.as_mut()
    {
        // Probe on the live context and roll back: `save` snapshots into inline
        // storage, where cloning the whole context heap-allocates its two scope
        // stacks for every dotted key in the corpus.
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

    // CW104/105/106: scope check. A trigger/effect (alias) carries a `## scope`
    // restriction in the config; if NONE of its overloads is valid in the current
    // scope, it's used in the wrong place. `scope_matches_required` treats
    // unrestricted / `any` / unresolved scopes leniently, so this only fires when
    // the current scope is known and every overload demands a different one.
    //
    // ON by default (escape hatch CWTOOLS_NO_SCOPE_CHECKS=1). Accurate firing
    // needs scope-change tracking: the engine seeds the right root scope per file
    // type (e.g. state-history files are state-scoped, not country) and pushes
    // scope through every scope-change effect/trigger link (`random_owned_state`,
    // leader abilities, iterators). With the config-driven scope/link registry
    // that tracking is now in place, so this runs by default.
    //
    // Modifiers are exempt: a modifier's `## scope` denotes its CATEGORY (where it
    // takes effect), not where it may be written. A country idea/national-spirit
    // `modifier = {}` block legitimately carries state-category modifiers
    // (`state_resource_cost_<resource>`) that cascade to the country's owned
    // states. Scope-checking them like a trigger/effect is a false positive.
    if ctx.scope_checks
        && category != "modifier"
        && let Some(sc) = scope_context.as_ref()
    {
        let reg = sc.registry.as_ref();
        let current = sc.current();
        // Only fire on overloads we matched confidently: a permissive match
        // against an unpopulated game-derived enum/value (or an unindexed type,
        // e.g. `oil` when vanilla resources aren't indexed) must not contribute
        // its unrelated `## scope`. With no confident overload the key's real
        // alias is unverifiable here, so stay lenient and skip the check.
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

    // math_expr is authoritative: when the usage is a `{block}` and any
    // overload types it as math_expr (e.g. `check_expr = math_expr`), validate
    // it strictly and skip the overload disjunction below. Otherwise a
    // permissive sibling overload — typically a pattern alias whose backing enum
    // is unpopulated, so it matches any key with a `variable_field` that accepts
    // the block cleanly — would win clean and discard the strict math
    // diagnostic. Mirrors the same authoritative bypass in `count_and_validate_children`.
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
    // A `## error_if_only_match` overload that matched cleanly, held aside so a
    // later directive-free clean match can still win. Surfaced only if no such
    // match exists (F# `errorIfOnlyMatch` / CW272).
    let mut only_match: Option<ValidationError> = None;
    let mut temp: Vec<ValidationError> = Vec::new();
    for &((rule_type, opts), _) in &overloads_conf {
        temp.clear();
        match rule_type {
            RuleType::LeafRule { .. } => {
                if let Some(leaf) = leaf {
                    validate_leaf(ctx, leaf, rule_type, scope_context.as_ref(), &mut temp);
                } else {
                    // Scalar-valued overload but the usage is a block — not a match.
                    // No `leaf` here (this branch is the `leaf == None` case), so no
                    // clean end range: fall back to the whole-line squiggle.
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
                        // Effect/trigger alias usage: a bare integer block key here
                        // is a HOI4 state-id scope (`129 = {}`), so allow numeric→state.
                        enter_block_scope(sc, key, opts, ctx.game, true, ctx.type_index);
                    }
                    // Seed loop-local variables: a `for_each_loop`-style block
                    // exposes `value`/`index`/`break` temp variables its body can
                    // read bare. Push them for the body only and truncate back
                    // after, so they don't leak to siblings/parents.
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
                    // Block overload but the usage is a scalar — not a match.
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
            // Clean match. A `## error_if_only_match` overload is not an accept:
            // keep its custom error aside and keep scanning for a directive-free
            // clean match, which still wins.
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
            // New best — take `temp`'s contents, leaving a reusable empty buffer.
            _ => best = Some(std::mem::take(&mut temp)),
        }
    }

    // No directive-free clean match: surface the sole directive match's custom
    // error, else the closest (fewest-errors) candidate.
    if let Some(custom) = only_match {
        errors.push(custom);
    } else if let Some(b) = best {
        errors.extend(b);
    }
}

/// Error used when an alias overload's shape (scalar vs block) can't match the
/// usage; it ranks a candidate and, when no better candidate exists, is surfaced
/// at the offending leaf's position. F# `ConfigRulesUnexpectedAliasKeyValue`.
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

/// Branch-count tests for the memo. They live in-crate because the count is not
/// part of the validation API — the integration tests next door cover what a
/// caller can see (which diagnostics come out).
#[cfg(test)]
mod tests {
    use crate::{Prepared, ValidationError, build_scope_registry_arc, validate_prepared_inner};
    use cwtools_parser::parser::parse_string;
    use cwtools_rules::rules_converter::ast_to_ruleset;
    use cwtools_string_table::string_table::StringTable;

    /// Two `recurse` overloads that are not equal (only the severity differs), so
    /// the equivalent-overload coalescing in `push_overload` cannot collapse them
    /// and every level really does branch two ways.
    const RECURSIVE_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = { alias_name[effect] = alias_match_left[effect] }
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
## severity = warning
alias[effect:recurse] = { alias_name[effect] = alias_match_left[effect] }
"#;

    /// `depth` nested `recurse` blocks around a field no overload accepts, so no
    /// candidate ever comes back clean and the disjunction explores every branch.
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

    /// Validate one script and report its diagnostics with the number of alias
    /// branches evaluated to produce them.
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

    /// 20 levels of two-way recursion is 2^21 branches walked exhaustively, which
    /// is what used to spend the whole 65,536-branch budget and stop the file
    /// short with CW277. Every level below the first repeats the same subtree in
    /// the same state, so the memo answers it and the file finishes.
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

    /// The count above only proves the file stayed inside the budget. This proves
    /// the shape of the work changed: four more levels of two-way recursion is
    /// sixteen times the exhaustive walk and costs a handful of branches.
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
