use crate::ValidationError;
use crate::ctx::ValidationCtx;
use cwtools_game::constants::Game;

pub mod common;
pub mod hoi4;
pub mod stellaris;
pub mod structural;

/// Intern every literal the per-game validators compare against an id taken from
/// a parsed AST. Seeding them into a long-lived table up front keeps them out of
/// any per-document overlay region, so the comparison holds whichever handle a
/// document happened to be parsed through (#475). Calling the real constructors
/// is what stops this list from drifting as keywords are added.
pub fn seed_comparison_literals(table: &cwtools_string_table::string_table::StringTable) {
    let _ = structural::Keywords::new(table);
    let _ = stellaris::Keys::new(table);
    let _ = hoi4::redundant_block_defaults(table);
}

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
