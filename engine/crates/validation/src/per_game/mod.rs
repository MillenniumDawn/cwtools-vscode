use crate::ValidationError;
use crate::ctx::ValidationCtx;
use cwtools_game::constants::Game;

pub mod common;
pub mod hoi4;
pub mod stellaris;
pub mod structural;

pub(crate) fn run_game_validators(ctx: &ValidationCtx, game: Game) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let ast = ctx.ast;
    let ruleset = ctx.ruleset;
    let table = ctx.table;
    let file_path = ctx.file_path;

    common::validate_common(ctx, &mut errors);

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
        _ => {}
    }

    errors
}
