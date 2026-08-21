//! Block validation: matching each child against candidate rules, cardinality
//! counting/enforcement, and the per-leaf dispatch.

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

/// The call-site key an inline script is pulled in with.
const INLINE_SCRIPT: &str = "inline_script";

use super::alias::validate_alias_usage;
use super::leaf::{check_variable_get, field_matches_value, validate_leaf};
use super::matching::{get_rule_key, matching_candidates, rule_matches_leaf_key};
use super::subtype_merge::flatten_nested_subtype_rules;
use super::suggest::best_suggestion;

/// True when a rule's left-hand field is `IgnoreField` (`key = ignore_field`),
/// meaning the matched field/block is accepted without validating its contents.
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
    // `key = ignore_field`: the field's value is accepted unvalidated.
    if rule_left_is_ignore(rule_type) {
        return;
    }
    // A rule keyed by `<type>` (`<technology> = { … }`) makes the KEY the
    // reference, not the value — F# records the same two shapes for its unused
    // check. An overloaded key runs every candidate here, so a losing candidate
    // can record too; that only ever marks something used, which is the safe
    // direction for CW239/CW231.
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
        // F# `ConfigRulesRuleWrongScope` (CW247): a trigger/effect/modifier rule
        // used in a scope it doesn't support. (Was the Rust-invented CW400.)
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
                // CW282: a bool field explicitly set to the default declared by
                // `## default_bool = yes|no` is redundant and can be omitted.
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
                            // Fix: delete the redundant `key = <default>` leaf.
                            // `leaf.pos` spans key→value; its end lands at the next
                            // token, taking the line and its newline with it.
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
                    // Explicit field rule (e.g. `int = {}` random_list weight): a
                    // numeric key here is NOT a state scope, so `numeric_state_ok=false`.
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
                // NodeRule (block expected) but the value is a scalar — kind mismatch.
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

/// Run several candidate rules for one overloaded key as a disjunction: accept on
/// the first clean match, otherwise surface the fewest-errors candidate.
///
/// `only_match_error(i)` returns the `## error_if_only_match` (CW272) custom error
/// for candidate `i` when that rule carries the directive, and `None` otherwise.
/// A directive-carrying candidate whose value matches cleanly is NOT an accept: it
/// is held aside and surfaced only if no directive-free candidate also matches
/// cleanly (a later clean, directive-free match still wins). Mirrors F#
/// `errorIfOnlyMatch`, gated by `lazyErrorMerge` on the absence of a clean match.
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
                // Clean match, but the rule says "error if this is the only match":
                // keep it aside and keep scanning for a directive-free clean match.
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
                // New best — take `temp`'s contents, leaving a reusable empty buffer.
                _ => best = Some(std::mem::take(&mut temp)),
            }
        }
    }
    // No directive-free clean match. A directive-carrying candidate that matched
    // cleanly is the sole match → surface its custom error; otherwise fall back to
    // the closest (fewest-errors) candidate.
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
    // Position of the block that owns `children` (its opening `key = {`). Used to
    // anchor cardinality diagnostics when the block is empty — so a missing
    // required field reports on the block's line, not at the file root (0,0).
    block_pos: (u32, u16),
    errors: &mut Vec<ValidationError>,
) {
    if ctx.alias_branch_budget_exhausted() {
        return;
    }
    // Nested subtype blocks (a `subtype[x] = {...}` not at the entity root) carry
    // their fields inside SubtypeRule entries that the candidate matcher below
    // doesn't see. Flatten them in — but only pay the clone when any are present,
    // since this is a hot path called for every block.
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

    // Cardinality bookkeeping for this rule list, derived once and shared by the
    // counting pass and phase 3 — both are no-ops for an absent rule kind.
    let mut block = BlockRules::of(rules);

    // Phases 1+2 fused: one pass over `children` both tallies occurrences (for
    // cardinality) and validates each child. Phase-2 validation never reads the
    // phase-1 counts (only phase 3 does) and counting emits nothing, so the two
    // passes collapse into one with identical output.
    let (leafvalue_counts, valueclause_counts) =
        count_and_validate_children(ctx, children, rules, &mut block, scope_context, errors);
    if ctx.alias_branch_budget_exhausted() {
        return;
    }

    // Phase 3: cardinality enforcement against the phase-1 counts.
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

/// Aggregated cardinality for one distinct keyed rule key within a block.
///
/// Duplicate keys are overloads/alternatives (e.g. two `clicksound =` rules in
/// one subtype), so the key is checked once against the most permissive bounds
/// rather than once per overload — otherwise a present-once field reads as
/// missing N-1 times, or an absent optional alternative double-reports.
struct KeyCard<'a> {
    /// The rule's key as authored. Paradox keys are case-insensitive and every
    /// comparison against this one is too, so no lowercased copy is ever
    /// materialised — this runs for every block in the corpus.
    key: &'a str,
    min: i32,
    max: i32,
    /// A `~` (soft) minimum on ANY overload of the key makes the whole key's
    /// minimum soft, so an under-count is not flagged.
    strict_min: bool,
    /// Occurrences of the key among the block's children.
    count: i32,
    /// Set once the key has been reported, so the per-rule report loop below
    /// visits each key once rather than once per overload.
    reported: bool,
}

/// A block's rule list reduced to what cardinality needs: one card per distinct
/// keyed rule key, plus whether the list has any bare-value or value-clause rule
/// at all. The dominant block kind (effect/trigger bodies whose sole rule is an
/// `alias_name[...]` wildcard) has none of them, so nothing is allocated and
/// phase 3 has nothing to enforce.
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

/// The alias category a `modifier = { ... }` block's contents are matched
/// through. A key that only ever matched rules of this category is in the block
/// *because* it is a modifier, which is what makes a zero value a no-op.
const MODIFIER_ALIAS: &str = "modifier";

/// Whether every candidate rule matched the key through the `modifier` alias
/// category. False as soon as one candidate is a rule field of the block's own
/// (a `SpecificField`, an `enum[...]`, …), so a legitimately-zero rule field is
/// never read as a zero modifier.
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

/// CW235 (F# `ZeroModifier`): a known modifier set to 0 does nothing, because
/// modifiers are additive.
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

/// Whether a rule's right-hand side is the `math_expr` value type.
pub(crate) fn rule_right_is_math_expr(rule_type: &RuleType) -> bool {
    matches!(
        rule_type,
        RuleType::LeafRule {
            right: NewField::ValueField(ValueType::MathExpr),
            ..
        }
    )
}

/// The rule set a `math_expr` `{block}` is validated against: a `value` base
/// (itself a math operand), an optional `tooltip`, and any registered
/// `mathexpr` operator key (`add`/`subtract`/…). No `= scalar` catch-all — that
/// is the whole point: an unrecognised key is an unexpected field, not a
/// silently-accepted variable assignment. Operator argument shapes are owned by
/// the config's `alias[mathexpr:*]` definitions, which `validate_children`
/// expands; an operator whose argument is itself `math_expr` recurses here.
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

/// Validate a `{block}` math expression strictly against [`math_clause_rules`].
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

/// Phases 1+2 of [`validate_children`], fused into one pass: tally occurrences of
/// every child kind (for cardinality) AND validate each child against the matching
/// rules, emitting unexpected-property and per-rule diagnostics. Recurses into
/// nested blocks via [`validate_children`]. Tallies keyed children straight into
/// `block`'s cards, and returns the two per-rule count vectors that phase 3
/// ([`enforce_cardinality`]) consumes:
/// - `leafvalue_counts`: per-rule count of matching `LeafValueRule`s,
/// - `valueclause_counts`: per-rule count of anonymous `{ ... }` clauses.
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

    // Item 5: LeafValues — count per LeafValueRule index.
    let mut leafvalue_counts: Vec<usize> = if any_leafvalue {
        vec![0usize; rules.len()]
    } else {
        Vec::new()
    };
    // Item 5: ValueClause — count per ValueClauseRule index.
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
                // Script keys are short; buffer the unquoted key on the stack to
                // avoid a per-leaf heap String. Borrowing across `with_string` is
                // unsafe (the closure holds the table's read guard and validation
                // recurses), so copy the bytes out into an owned stack buffer.
                let mut keybuf: SmallVec<[u8; 24]> = SmallVec::new();
                table.with_string(leaf.key.normal, |s| {
                    keybuf.extend_from_slice(unquote_key(s).as_bytes())
                });
                let key: &str = std::str::from_utf8(&keybuf).unwrap_or_default();
                // An inline_script call is not a field of the block it sits in —
                // it stands for the body it pulls in. Expand that body and count
                // and check it as though it had been written here.
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
                // Phase-1 tally: keyed children matched case-insensitively so a
                // field written `texturefile` satisfies a rule keyed
                // `textureFile`. Only keys a rule actually asks about are
                // counted; the rest can't affect cardinality.
                if any_keyed && let Some(i) = block.position(key) {
                    block.cards[i].count += 1;
                }
                let candidates =
                    matching_candidates(rules, key, ruleset, type_index, rule_matches_leaf_key);
                // `math_expr` is authoritative: a `{block}` math expression is
                // validated strictly here, BEFORE the candidate disjunction
                // below. A permissive sibling overload (`value_set[variable] =
                // scalar`, present on every variable-math effect) would
                // otherwise accept the block with zero errors and `pick_best_candidate`
                // would discard the strict unexpected-key diagnostic. Bypassing
                // the disjunction keeps the strict check.
                if let Value::Clause(math_children) = &leaf.value
                    && candidates.iter().any(|(rt, _)| rule_right_is_math_expr(rt))
                {
                    let pos = (leaf.pos.start.line, leaf.pos.start.col);
                    validate_math_clause(ctx, math_children, scope_context, pos, errors);
                    continue;
                }
                if candidates.is_empty() {
                    // Item 5: dynamic modifier keys — if provided and this key is a
                    // known modifier, accept silently (modifier context mechanism).
                    // The modifier set is built lowercase; compare lowercase. Compute
                    // the lowercase form lazily here — only the no-candidate branch
                    // needs it, and most leaves match a candidate.
                    let key_lower = key.to_lowercase();
                    let is_modifier = modifier_keys
                        .map(|mk| mk.contains(key_lower.as_str()))
                        .unwrap_or(false);
                    // CW235 (F# `ZeroModifier`): a known modifier set to 0 is a no-op
                    // (modifiers are additive). Only fires on confirmed modifiers.
                    if is_modifier && value_is_zero(&leaf.value) {
                        push_zero_modifier(file_path, leaf, key, errors);
                    }
                    // A `@name = value` leaf is a Paradox read-time variable
                    // definition, valid anywhere in a block. F# skips these from the
                    // unexpected-field check (RuleValidationService.fs:266,
                    // `leaf.Key.[0] <> '@'`).
                    let is_define = key.starts_with('@');
                    if !is_modifier && !is_define {
                        // This parser stores `key = { ... }` as a Leaf with a
                        // Clause value, so split the F# way: a clause value is an
                        // unexpected property NODE (CW262), a scalar value an
                        // unexpected property LEAF (CW263).
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
                        // CW262 complains about the key, so its squiggle covers
                        // the key token (the same span the did-you-mean fix
                        // edits), not the whole block it opens.
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
                        // Did-you-mean (fix metadata only, corpus-inert): on this
                        // error path, scan the sibling SpecificField keys for a
                        // single close match and offer a key-token rename. The span
                        // covers the raw key token (quotes included, from the
                        // interned source string) so a quoted key is replaced whole.
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
                    // CW235 again, for the common case: the key matched, but only
                    // through the `modifier` alias, so it IS a modifier and a zero
                    // is still a no-op. Gated on the match being modifier-only so a
                    // rule field of the type's own (`factor = 0`, `cost = 0`) that
                    // happens to share a modifier's name is left alone.
                    if value_is_zero(&leaf.value)
                        && candidates_are_modifier_alias_only(&candidates)
                        && modifier_keys.is_some_and(|mk| mk.contains(key.to_lowercase().as_str()))
                    {
                        push_zero_modifier(file_path, leaf, key, errors);
                    }
                    // An overloaded key (several rules with the same key, e.g. two
                    // `province = { ... }` forms) is a disjunction — accept if any
                    // candidate validates cleanly.
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
            // Item 5: LeafValue validation
            Child::LeafValue(lvidx) => {
                let lv = &ast.arena.leaf_values[*lvidx as usize];
                // Phase-1 tally. An anonymous `{ ... }` block parses as a
                // clause-valued LeafValue; count it toward a ValueClauseRule, not a
                // LeafValueRule. Non-clause values count toward EVERY matching
                // LeafValueRule (checkCardinality is a per-rule sum) — breaking on
                // the first match lets a permissive earlier alternative (e.g. a
                // `<type>` TypeField, which accepts any token) starve a later
                // `enum[...]` rule, producing a spurious "appears 0 time(s)" error.
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
                // Anonymous `{ ... }` block: validate against a ValueClauseRule,
                // recursing into the block's children (e.g. milestones entries).
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
                            // VariableGetField bare read: validate against the
                            // project-wide variable index (CW246), mirroring the
                            // Leaf path and F# checkVariableGetFieldNE.
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
                            // A bare `<type>` member of a list (`prerequisites =
                            // { tech_x }`) is a reference too — record it for the
                            // project-wide unused check.
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

/// Check one `inline_script` call: rebuild the body it names with its arguments
/// substituted, then count and validate that body against the caller's own rules,
/// scope and cardinality tally — the only context the body has.
///
/// Its diagnostics are re-anchored onto the call site. The body's line numbers
/// belong to the script file, which is not the file being validated, and the same
/// body is right or wrong per call: the caller is what has to change. Each message
/// picks up the script line it came from, so a nested call leaves a trail reading
/// innermost first. CW274 stands in when the body cannot be pulled in at all.
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
        // Both carry spans into the script file, which nothing downstream can
        // resolve against the file this diagnostic is now reported in.
        error.fix = None;
        error.related.clear();
        error
    }));
}

/// Phase 3 of [`validate_children`]: enforce cardinality (min/max occurrence)
/// against the counts gathered by [`count_and_validate_children`]. Reads
/// `block`'s key cards, `leafvalue_counts`, and `valueclause_counts`; emits
/// CW242 diagnostics.
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
    // Every arm below is keyed on one of the three rule kinds; with none of them
    // present there is no cardinality to enforce, so the block (the common
    // effect/trigger body) skips the rule scan entirely.
    if !block.any() {
        return;
    }

    let ast = ctx.ast;
    let table = ctx.table;
    let file_path = ctx.file_path;

    // Under-count: anchor on the block's key position (block_pos) so the
    // squiggle lands on the `key = { ... }` opener, not on whatever child
    // happens to be first. When block_pos is (0,0) — the type_per_file
    // sentinel where the whole file is one entity and there is no block key
    // — fall back to the first child to avoid a line-0 diagnostic.
    let (block_line, block_col) = if block_pos != (0, 0) {
        block_pos
    } else {
        children
            .iter()
            .find_map(|c| child_start_pos(c, ast))
            .unwrap_or(block_pos)
    };

    for (rule_idx, (rule_type, opts)) in rules.iter().enumerate() {
        // Both under- and over-count default to a WARNING (config cardinalities are
        // often stricter than the game, and cardinality-max is emitted as a Warning);
        // an explicit `## severity` still wins.
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
                    // Each distinct key is reported at most once, deduped via the
                    // card's "reported" flag.
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
                            // Anchor the over-count on the first actual
                            // occurrence of this key rather than the block's
                            // first child — the squiggle belongs on the field
                            // being flagged, not on whatever happens to sit at
                            // the top of the block. (The under-count case has no
                            // occurrence to point at, so it stays on the block.)
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
            // Item 5: LeafValueRule cardinality
            RuleType::LeafValueRule { right } => {
                let count = leafvalue_counts[rule_idx] as i32;
                // `~` (soft) minimum: don't flag an under-count. These rules are
                // typically a disjunction of overlapping leafvalue kinds (e.g.
                // `ship_types` accepts <naval_equip> OR <ship_unit> OR
                // enum[ship_units], each `~1..inf`); a value matching one leaves
                // the others at 0, which is not an error. Genuinely invalid values
                // are still caught by the per-value "Unexpected bare value" check.
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
            // Item 5: ValueClauseRule cardinality
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
