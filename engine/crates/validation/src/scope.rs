use cwtools_game::constants::Game;
use cwtools_game::scope_engine::{SCOPE_ANY, ScopeContext, ScopeId};
use cwtools_game::scope_registry::ScopeRegistry;
use cwtools_rules::rules_types::*;

use crate::common::{ValidationError, looks_like_data_ref};
use crate::resolve::find_type_rule_opts;
use cwtools_error_codes as error_codes;

pub(crate) fn build_scope_registry(ruleset: &RuleSet, game: Game) -> ScopeRegistry {
    ScopeRegistry::from_config(&ruleset.scope_inputs, &ruleset.link_inputs, game)
}

pub fn scope_matches_required(
    current: ScopeId,
    registry: &ScopeRegistry,
    required: &[String],
) -> bool {
    if required.is_empty() {
        return true;
    }
    if required
        .iter()
        .any(|s| s.eq_ignore_ascii_case("any") || s.eq_ignore_ascii_case("all"))
    {
        return true;
    }
    if current == SCOPE_ANY {
        return true;
    }
    if registry.is_empty() {
        return true;
    }
    required.iter().any(|r| {
        registry
            .id_of(r)
            .is_some_and(|rid| registry.is_subscope_or_eq(current, rid))
    })
}

pub(crate) fn with_saved_scope_context<R>(
    scope_context: &mut Option<ScopeContext>,
    validate: impl FnOnce(&mut Option<ScopeContext>) -> R,
) -> R {
    let saved = scope_context.as_ref().map(ScopeContext::save);
    let result = validate(scope_context);
    if let (Some(saved), Some(ctx)) = (saved, scope_context.as_mut()) {
        ctx.restore(saved);
    }
    result
}

pub(crate) fn validate_scope_target(
    scope_context: &mut Option<ScopeContext>,
    value: &str,
    expected: &[String],
    leaf: &cwtools_parser::ast::Leaf,
    file_path: &crate::FilePath,
    errors: &mut Vec<ValidationError>,
) {
    if value.is_empty() || looks_like_data_ref(value) {
        return;
    }
    let Some(ctx) = scope_context.as_mut() else {
        return;
    };
    let reg = std::sync::Arc::clone(&ctx.registry);
    if reg.is_empty()
        || expected.iter().any(|s| {
            !s.eq_ignore_ascii_case("any")
                && !s.eq_ignore_ascii_case("all")
                && reg.id_of(s).is_none()
        })
    {
        return;
    }
    let Some(result) = with_saved_scope_context(scope_context, |context| {
        context.as_mut().map(|ctx| ctx.change_scope(value))
    }) else {
        return;
    };
    let (code, message) = match result {
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
            if !expected.is_empty() && !scope_matches_required(scope, &reg, expected) =>
        {
            let code = &error_codes::CW243_TARGET_WRONG_SCOPE;
            (
                code,
                code.format(&[value, &reg.name_of(scope), &expected.join(" or ")]),
            )
        }
        cwtools_game::scope_engine::ScopeResult::NotFound => {
            let code = &error_codes::CW244_INVALID_TARGET;
            (code, code.format(&[value, &expected.join(" or ")]))
        }
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

#[cfg(test)]
mod tests {
    use super::with_saved_scope_context;
    use cwtools_game::constants::Game;
    use cwtools_game::scope_engine::{ScopeContext, ScopeId};

    #[test]
    fn saved_scope_context_restores_root_scopes_and_from_stack() {
        let mut scope_context = Some(ScopeContext::new(Game::Hoi4, ScopeId(2)));
        let before = scope_context.as_ref().unwrap().save();

        let result = with_saved_scope_context(&mut scope_context, |context| {
            let scope = context.as_mut().unwrap();
            scope.root = ScopeId(3);
            scope.push_scope(ScopeId(4));
            scope.from.push(ScopeId(5));
            "validated"
        });

        assert_eq!(result, "validated");
        let restored = scope_context.as_ref().unwrap();
        assert_eq!(restored.root, before.root);
        assert_eq!(restored.scopes.as_slice(), before.scopes.as_slice());
        assert_eq!(restored.from.as_slice(), before.from.as_slice());
    }
}

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

pub(crate) fn enter_block_scope(
    ctx: &mut ScopeContext,
    key: &str,
    opts: &Options,
    game: Option<Game>,
    numeric_state_ok: bool,
    type_index: Option<&cwtools_index::TypeIndex>,
) {
    if key.contains(':') {
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
        let indexed_instance = type_index.is_some_and(|index| index.is_any_instance(key));
        if ctx.scope_depth() == before && (looks_like_data_ref(key) || indexed_instance) {
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
