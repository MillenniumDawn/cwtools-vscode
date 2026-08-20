//! CW261: an instance of a `## unique` type whose id the project defines more
//! than once. Project-wide, so a second definition in another file counts.

use cwtools_game::constants::Game;
use cwtools_index::{TypeIndex, collect_type_instances};
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::{Prepared, build_scope_registry_arc, validate_prepared};

const RULES: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        unique = yes
    }
    type[other] = {
        path = "game/common/others"
    }
    ## unique = yes
    type[commented] = {
        path = "game/common/commented"
    }
}
thing = { x = scalar }
other = { x = scalar }
commented = { x = scalar }
"#;

/// Index every file, then validate every file, the way the driver does.
/// `base_game` files are indexed but not validated, standing in for a vanilla
/// install the mod is overriding.
fn cw261_for(files: &[(&str, &str)], base_game: &[(&str, &str)]) -> Vec<(String, String)> {
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(RULES, &table), &table);
    let registry = build_scope_registry_arc(&ruleset, Some(Game::Hoi4));

    let parsed: Vec<_> = files
        .iter()
        .chain(base_game)
        .map(|(path, src)| (*path, parse_string(src, &table)))
        .collect();

    let mut index = TypeIndex::new();
    for (path, ast) in &parsed {
        let instances = collect_type_instances(&ruleset, ast, path, &table);
        if base_game.iter().any(|(p, _)| p == path) {
            index.merge_base_game(path, instances);
        } else {
            index.merge(path, instances);
        }
    }

    let prepared = Prepared {
        ruleset: &ruleset,
        table: &table,
        game: Some(Game::Hoi4),
        type_index: Some(&index),
        modifier_keys: None,
        loc_index: None,
        extra_loc_keys: None,
        inline_scripts: None,
        registry: registry.as_ref(),
        scope_checks: false,
        var_checks: false,
    };

    let mut out = Vec::new();
    for (path, ast) in parsed.iter().take(files.len()) {
        for err in validate_prepared(ast, path, &prepared) {
            if err.code == Some("CW261") {
                out.push((path.to_string(), err.message.clone()));
            }
        }
    }
    out
}

#[test]
fn duplicate_in_one_file_is_flagged_at_every_site() {
    let found = cw261_for(
        &[(
            "game/common/things/a.txt",
            "dup = { x = yes }\ndup = { x = no }\n",
        )],
        &[],
    );
    assert_eq!(found.len(), 2, "got: {found:?}");
    assert!(
        found
            .iter()
            .all(|(_, m)| m.contains("dup") && m.contains("thing")),
        "got: {found:?}"
    );
}

#[test]
fn duplicate_across_files_is_flagged_in_both() {
    // The newly-covered case: neither file repeats the id on its own.
    let found = cw261_for(
        &[
            ("game/common/things/a.txt", "dup = { x = yes }\n"),
            ("game/common/things/b.txt", "dup = { x = no }\n"),
        ],
        &[],
    );
    let files: Vec<&str> = found.iter().map(|(f, _)| f.as_str()).collect();
    assert_eq!(found.len(), 2, "got: {found:?}");
    assert!(
        files.contains(&"game/common/things/a.txt"),
        "got: {files:?}"
    );
    assert!(
        files.contains(&"game/common/things/b.txt"),
        "got: {files:?}"
    );
}

#[test]
fn distinct_ids_are_clean() {
    let found = cw261_for(
        &[
            ("game/common/things/a.txt", "one = { x = yes }\n"),
            ("game/common/things/b.txt", "two = { x = no }\n"),
        ],
        &[],
    );
    assert!(found.is_empty(), "got: {found:?}");
}

// The type declares `unique` as a `## ` directive rather than a body leaf, which
// is how most of the HOI4 config spells it (#264).
#[test]
fn duplicate_of_a_comment_declared_unique_type_is_flagged() {
    let found = cw261_for(
        &[
            ("game/common/commented/a.txt", "dup = { x = yes }\n"),
            ("game/common/commented/b.txt", "dup = { x = no }\n"),
        ],
        &[],
    );
    assert_eq!(found.len(), 2, "got: {found:?}");
    assert!(
        found
            .iter()
            .all(|(_, m)| m.contains("dup") && m.contains("commented")),
        "got: {found:?}"
    );
}

#[test]
fn duplicate_of_a_non_unique_type_is_clean() {
    let found = cw261_for(
        &[
            ("game/common/others/a.txt", "dup = { x = yes }\n"),
            ("game/common/others/b.txt", "dup = { x = no }\n"),
        ],
        &[],
    );
    assert!(found.is_empty(), "got: {found:?}");
}

#[test]
fn overriding_a_base_game_definition_is_clean() {
    // The mod defines `dup` once. The base game's own `dup` is an override
    // target, not a second project definition.
    let found = cw261_for(
        &[("game/common/things/a.txt", "dup = { x = yes }\n")],
        &[("vanilla/common/things/00_things.txt", "dup = { x = no }\n")],
    );
    assert!(found.is_empty(), "got: {found:?}");
}
