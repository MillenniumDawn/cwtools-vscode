//! Scope-registry construction and per-block scope seeding/tracking, plus the
//! scope-target / wrong-scope diagnostics (CW243/244/245).

use cwtools_game::constants::Game;
use cwtools_game::scope_engine::{SCOPE_ANY, ScopeContext, ScopeId};
use cwtools_game::scope_registry::ScopeRegistry;
use cwtools_rules::rules_types::*;

use crate::common::{ValidationError, looks_like_data_ref};
use crate::resolve::find_type_rule_opts;
use cwtools_error_codes as error_codes;

/// Build the runtime [`ScopeRegistry`] for a ruleset. Thin wrapper over
/// [`ScopeRegistry::from_config`], which owns the construction (config inputs
/// merged over the game's hardcoded tables).
pub(crate) fn build_scope_registry(ruleset: &RuleSet, game: Game) -> ScopeRegistry {
    ScopeRegistry::from_config(&ruleset.scope_inputs, &ruleset.link_inputs, game)
}

pub fn scope_matches_required(
    current: ScopeId,
    registry: &ScopeRegistry,
    required: &[String],
) -> bool {
    // No restriction declared -> valid anywhere.
    if required.is_empty() {
        return true;
    }
    // `## scope = any` / `all` mean unrestricted (very common on triggers).
    if required
        .iter()
        .any(|s| s.eq_ignore_ascii_case("any") || s.eq_ignore_ascii_case("all"))
    {
        return true;
    }
    // Current scope is the open wildcard -> lenient.
    if current == SCOPE_ANY {
        return true;
    }
    // A requirement is satisfied if the current scope is that scope or a subscope
    // of it (e.g. `character` satisfies a `country` requirement). An unresolvable
    // requirement name does NOT auto-satisfy.
    required.iter().any(|r| {
        registry
            .id_of(r)
            .is_some_and(|rid| registry.is_subscope_or_eq(current, rid))
    })
}

/// Validate a scope-target value (`owner`, `root.capital_scope`, …) by resolving
/// the chain from the current scope on a throwaway clone of the context:
/// - CW245 (error in target) if a link in the chain is used in the wrong scope;
/// - CW243 (target wrong scope) if the resolved scope doesn't satisfy the
///   field's expected scopes.
///
/// Lenient: empty, data-ref (tags/ids/`prefix:`), upper-case magic-word and
/// unresolved targets are accepted, so only genuine lower-case link chains are
/// checked. Gated with the other scope checks by the caller.
pub(crate) fn validate_scope_target(
    ctx: &ScopeContext,
    value: &str,
    expected: &[String],
    leaf: &cwtools_parser::ast::Leaf,
    file_path: &crate::FilePath,
    errors: &mut Vec<ValidationError>,
) {
    if value.is_empty() || looks_like_data_ref(value) {
        return;
    }
    let reg = ctx.registry.as_ref();
    // Only validate genuine scope fields: every expected scope name must resolve
    // in the registry. A garbage entry (e.g. `country].value[variable` from a
    // mis-parsed `scope[country].value[variable]`) means this isn't really a
    // scope target slot — skip rather than flag the value as an invalid target.
    if expected.iter().any(|s| {
        !s.eq_ignore_ascii_case("any") && !s.eq_ignore_ascii_case("all") && reg.id_of(s).is_none()
    }) {
        return;
    }
    let mut probe = ctx.clone();
    let (code, message) = match probe.change_scope(value) {
        cwtools_game::scope_engine::ScopeResult::WrongScope {
            command,
            current,
            expected: exp,
        } => {
            let exp_names: Vec<String> = exp.iter().map(|s| reg.name_of(*s)).collect();
            let code = &error_codes::CW245_ERROR_IN_TARGET;
            (
                code,
                code.format(&[&command, &reg.name_of(current), &exp_names.join(" or ")]),
            )
        }
        cwtools_game::scope_engine::ScopeResult::NewScope { scope, .. }
            if !expected.is_empty() && !scope_matches_required(scope, reg, expected) =>
        {
            let code = &error_codes::CW243_TARGET_WRONG_SCOPE;
            (
                code,
                code.format(&[value, &reg.name_of(scope), &expected.join(" or ")]),
            )
        }
        // The token is not a recognised link/var/value target at all. With the
        // full config link map loaded, NotFound is reliable -> CW244.
        cwtools_game::scope_engine::ScopeResult::NotFound => {
            let code = &error_codes::CW244_INVALID_TARGET;
            (code, code.format(&[value, &expected.join(" or ")]))
        }
        // NewScope-in-expected / VarFound / ValueFound / AnyScope -> lenient.
        _ => return,
    };
    errors.push(
        ValidationError::from_code_with(
            code,
            code.severity,
            file_path,
            leaf.pos.start.line,
            leaf.pos.start.col,
            message,
        )
        .with_end(leaf.pos.end),
    );
}

/// Seed the scope context for a type instance's body. Precedence:
/// 1. a matched subtype's `## push_scope`;
/// 2. the type's root rule `## push_scope` or `## replace_scope` (the
///    state-history `state` object uses `replace_scope = { this = state ... }`);
/// 3. the instance's own key when that's a scope link / data ref (`state = {…}`).
///
/// A `## push_scope` becomes the instance's ROOT too (F# `{ Root = ps;
/// Scopes = [ps] }`), so `ROOT = { … }` blocks inside resolve to the instance's
/// scope rather than the file default. Caller must `save()` first and
/// `restore()` after.
pub(crate) fn seed_root_scope(
    ctx: &mut ScopeContext,
    type_def: &TypeDefinition,
    subtype_push: Option<&str>,
    node_key: Option<&str>,
    ruleset: &RuleSet,
    game: Option<Game>,
) {
    if let Some(ps) = subtype_push {
        seed_root_from_push(ctx, ps);
        return;
    }
    let root_opts = find_type_rule_opts(&type_def.name, ruleset);
    if let Some(push) = root_opts.and_then(|o| o.push_scope.as_deref()) {
        seed_root_from_push(ctx, push);
    } else if let Some(replace) = root_opts.and_then(|o| o.replace_scopes.as_ref()) {
        apply_replace_scopes(ctx, replace, game);
    } else if let Some(k) = node_key {
        let before = ctx.scope_depth();
        ctx.change_scope(k);
        if ctx.scope_depth() == before && looks_like_data_ref(k) {
            ctx.push_scope(SCOPE_ANY);
        }
    }
}

/// Seed an instance's scope from a `## push_scope` value: the scope becomes
/// both the current scope and ROOT, matching the F# original (which built a
/// fresh `{ Root = ps; Scopes = [ps] }` context). Without the ROOT update,
/// `ROOT = { … }` blocks inside a type instance resolve to the file-default
/// scope instead of the instance's, so a lenient event type (`unit_leader_event`,
/// `state_event` — both `push_scope = any` for HOI4's hybrid event scopes) gets
/// its ROOT falsely scope-checked against country. The stack reset also makes
/// `prev` inside the instance match F# (it can't fall through to the file root).
fn seed_root_from_push(ctx: &mut ScopeContext, push: &str) {
    let first = push
        .trim_matches(|c: char| c == '{' || c == '}' || c.is_whitespace())
        .split_whitespace()
        .next()
        .unwrap_or(push);
    match ctx.registry.id_of(first) {
        Some(id) => {
            ctx.root = id;
            ctx.scopes.clear();
            ctx.scopes.push(id);
        }
        None => {
            ctx.change_scope(push);
        }
    }
}

/// Apply a `## push_scope = <scope>` value (or a `{ a b }` list): resolve the
/// scope NAME through the registry and push that scope id. `any`/`all` push the
/// wildcard, which is the correct lenient behaviour for `for_each_scope_loop`
/// etc. Falls back to `change_scope` for command-like values (`prev`, `root`).
pub(crate) fn push_named_scope(ctx: &mut ScopeContext, push: &str) {
    let first = push
        .trim_matches(|c: char| c == '{' || c == '}' || c.is_whitespace())
        .split_whitespace()
        .next()
        .unwrap_or(push);
    match ctx.registry.id_of(first) {
        Some(id) => ctx.push_scope(id),
        None => {
            ctx.change_scope(push);
        }
    }
}

/// Apply the scope change for entering a `key = { ... }` block. The caller must
/// have `save()`d the context first and `restore()` after.
///
/// Order of precedence:
/// 1. an explicit `## push_scope` on the rule (alias effects like `every_state`);
/// 2. otherwise the scope produced by the key itself when it's a scope-change
///    link/iterator/data-ref (`owner`, `random_owned_state`, `root`, `prev`, …);
/// 3. otherwise, an unmodeled data reference (a tag/id/prefix or verified
///    indexed instance) enters ANY scope so inner effects aren't falsely
///    scope-checked. Plain effect blocks keep the current scope unchanged.
///
/// `numeric_state_ok` lets a bare integer block key resolve to the `state` scope
/// in HOI4 (`129 = { ... }` scopes to state 129). It must be `true` ONLY when the
/// block was matched as an effect/trigger alias usage (a real scope block), and
/// `false` when it was matched as an explicit `int = { ... }` field — e.g. a
/// `random_list` weight bucket, whose body runs in the current scope, not state.
pub(crate) fn enter_block_scope(
    ctx: &mut ScopeContext,
    key: &str,
    opts: &Options,
    game: Option<Game>,
    numeric_state_ok: bool,
    type_index: Option<&cwtools_index::TypeIndex>,
) {
    if key.contains(':') {
        // A `prefix:data` key (`var:x`, `event_target:y`, `sp:z`) is resolved by
        // the registry prefix link, NOT by a matched rule's `## push_scope` — that
        // would be an unreliable guess (a `var:` ref holds a data-dependent scope).
        // change_scope pushes ANY for value prefixes (var:/event_target:) or the
        // target scope for scope-change prefixes (sp: -> project). Unknown prefix
        // -> ANY (lenient).
        let before = ctx.scope_depth();
        ctx.change_scope(key);
        if ctx.scope_depth() == before {
            ctx.push_scope(SCOPE_ANY);
        }
    } else if let Some(ref push) = opts.push_scope {
        push_named_scope(ctx, push);
    } else {
        let before = ctx.scope_depth();
        ctx.change_scope(key);
        // change_scope didn't recognise the key, but a from-data reference used
        // as a block key DOES change scope to whatever it resolves to: a numeric
        // state/province id (`857 = {...}`), a country tag (`GER = {...}`), or an
        // indexed type instance. The type index does not retain the target scope,
        // so an indexed instance remains lenient rather than inheriting its parent.
        let indexed_instance = type_index.is_some_and(|index| index.is_any_instance(key));
        if ctx.scope_depth() == before && (looks_like_data_ref(key) || indexed_instance) {
            // HOI4: a bare integer scope block is a state (`129 = {...}`), so push
            // `state` rather than the lenient ANY — the body's resource/state
            // triggers then resolve against the right scope and hover shows it.
            // Only for genuine scope blocks (`numeric_state_ok`); a `random_list`
            // `int = {}` weight bucket keeps the current scope.
            let state_id = if numeric_state_ok
                && game == Some(Game::Hoi4)
                && !key.is_empty()
                && key.bytes().all(|b| b.is_ascii_digit())
            {
                ctx.registry.id_of("state")
            } else {
                None
            };
            ctx.push_scope(state_id.unwrap_or(SCOPE_ANY));
        }
    }
    if let Some(ref replace) = opts.replace_scopes {
        apply_replace_scopes(ctx, replace, game);
    }
}

pub(crate) fn apply_replace_scopes(
    ctx: &mut ScopeContext,
    replace: &ReplaceScopes,
    game: Option<Game>,
) {
    if game.is_some() {
        ctx.apply_replace_scope(
            replace.root.as_deref(),
            replace.this.as_deref(),
            &replace.froms,
            &replace.prevs,
        );
    }
}
