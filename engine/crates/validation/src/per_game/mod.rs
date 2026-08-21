use crate::ValidationError;
use crate::ctx::ValidationCtx;
use cwtools_game::constants::Game;

pub mod common;
pub mod hoi4;
pub mod stellaris;
pub mod structural;

/// Run game-specific validators after generic rule validation.
///
/// Takes the shared [`ValidationCtx`] (not a handful of positional args) so the
/// per-game layer reads from the same context the rest of the engine threads,
/// and so checks that need cross-file state (the loc-key index for
/// missing-localisation checks, the type index, …) can reach it here instead of
/// being limited to the local AST.
pub(crate) fn run_game_validators(ctx: &ValidationCtx, game: Game) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let ast = ctx.ast;
    let ruleset = ctx.ruleset;
    let table = ctx.table;
    let file_path = ctx.file_path;

    // Common checks (duplicate `unique` type instances, project-wide off the
    // index). Whether a `should_be_referenced` type is ever referenced needs
    // every file's references first, so it lives in `crate::references`.
    common::validate_common(ctx, &mut errors);

    // Cross-game structural hints.
    structural::validate_structural(ast, table, file_path, game, &mut errors);

    match game {
        Game::Stellaris => {
            stellaris::validate_stellaris(
                ast,
                ruleset,
                table,
                file_path,
                ctx.type_index,
                &mut errors,
            );
            stellaris::mark_exempt_technologies(ctx);
        }
        Game::Hoi4 => {
            hoi4::validate_hoi4(ast, ruleset, table, file_path, &mut errors);
        }
        _ => {
            // Other games: only common checks for now
        }
    }

    errors
}
