//! The cache-writing subcommands: `serialize`/`deserialize` for a single-file
//! `.cwb` AST cache, and `cache-vanilla` for a base-game type index.

use cwtools_driver::index_game_dir;
use cwtools_info::vanilla_cache;
use cwtools_parser::parser::parse_string;
use cwtools_string_table::string_table::StringTable;
use std::path::PathBuf;

use super::rules::load_rules;

pub(super) fn serialize(input: PathBuf, output: PathBuf) {
    let input_str = std::fs::read_to_string(&input).unwrap_or_else(|e| {
        eprintln!("Error reading {}: {}", input.display(), e);
        std::process::exit(1);
    });
    let table = StringTable::new();
    let parsed = parse_string(&input_str, &table);
    let cached =
        cwtools_cache::convert::arena_to_cached(&parsed.arena, &parsed.root_children, &table);
    match cwtools_cache::io::serialize_to_file(&cached, &output) {
        Ok(_) => {
            println!("Serialized to {}", output.display());
        }
        Err(e) => {
            eprintln!("Error serializing: {}", e);
            std::process::exit(1);
        }
    }
}

pub(super) fn deserialize(input: PathBuf) {
    let table = StringTable::new();
    let result = cwtools_cache::io::with_archived_file(&input, |archived| {
        cwtools_cache::convert::archived_to_arena(archived, &table)
    });
    match result {
        Ok(Ok((arena, root))) => {
            println!("Deserialized from {}", input.display());
            println!("  Leaves:   {}", arena.leaves.len());
            println!("  Values:   {}", arena.leaf_values.len());
            println!("  Comments: {}", arena.comments.len());
            println!("  Root children: {}", root.len());
        }
        Ok(Err(e)) | Err(e) => {
            eprintln!("Error deserializing {}: {}", input.display(), e);
            std::process::exit(1);
        }
    }
}

pub(super) fn vanilla(game: String, vanilla: PathBuf, rules: PathBuf, output: PathBuf) {
    use cwtools_game::constants::Game;

    if Game::from_str(&game).is_none() {
        eprintln!(
            "Unknown game: {}. Supported: hoi4, stellaris, eu4, ck2, ck3, vic2, vic3, ir, eu5, custom",
            game
        );
        std::process::exit(1);
    }

    let rules_table = StringTable::new();
    let ruleset = load_rules(&rules, &rules_table);
    println!("  Loaded {} types from rules", ruleset.types.len());

    let var_effects = cwtools_info::variable_defining_effects(&ruleset);
    let index = index_game_dir(&vanilla, &ruleset, &rules_table, &var_effects);
    // Loc keys + file paths + variable names ride along so a cache hit
    // also skips the loc walk and file-index walk over the install.
    let aux = cwtools_driver::build_vanilla_cache_aux(&vanilla, &index);
    // Combined fingerprint = game version + ruleset shape, so a cache
    // built against one rules set is treated as stale by another (the
    // cached instances are extracted by the rules; a rules change can
    // change which instances exist and under what name).
    let fingerprint = vanilla_cache::combined_fingerprint(&vanilla, &ruleset);
    println!("  Vanilla fingerprint: {}", fingerprint);
    match vanilla_cache::save(&index, &game, &fingerprint, &output, aux) {
        Ok(n) => println!("Wrote {} base-game instances to {}", n, output.display()),
        Err(e) => {
            eprintln!("Error writing vanilla cache {}: {}", output.display(), e);
            std::process::exit(1);
        }
    }
}
