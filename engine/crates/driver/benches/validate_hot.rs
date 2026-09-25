//! Batch validation inner loop over a real ruleset and a pinned corpus file.
//!
//! `rules_hot` covers the work a keystroke triggers. This one times
//! `validate_prepared` on a fixed Kaiserreich scripted-effects file, the shape
//! a CLI or workspace scan walks per file. Combined with the TRACE spans on
//! `count_and_validate_children`, `validate_leaf` and `validate_alias_usage`,
//! a later change to those loops can land with before/after numbers.
//!
//! Inputs are the same two checkouts the corpus guard uses:
//!
//!   CWTOOLS_RULES=/path/to/cwtools-hoi4-config/Config \
//!   CWTOOLS_CORPUS=/path/to/Kaiserreich-4-Development \
//!     cargo bench -p cwtools_driver --bench validate_hot
//!
//! Either can also be found under `CWTOOLS_PROJECTS`, or as a sibling of this
//! repo. The corpus case prints why and measures nothing when either checkout
//! is missing. `validate_prepared/fixture` always runs, against an in-repo
//! ruleset and script, so a machine with no siblings still gets a number.
//! `validate_prepared/keyed_entity` is the same kind of in-repo case, sized
//! like a large HOI4 type (many specific fields, a few hundred entities) so
//! cardinality bookkeeping shows up in the number.

use std::hint::black_box;
use std::path::{Path, PathBuf};

use criterion::{Criterion, criterion_group, criterion_main};
use cwtools_driver::{RulesInput, load_rules};
use cwtools_game::constants::Game;
use cwtools_index::{TypeIndex, collect_defined_variables_from_rules, collect_type_instances};
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_rules::rules_types::{RootRule, RuleSet, RuleType};
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::{Prepared, build_scope_registry_arc, validate_prepared};

/// Alias-heavy scripted-effects file from the pinned Kaiserreich corpus.
/// Large enough for the inner loop to show up, small enough to iterate.
const CORPUS_FILE: &str = "common/scripted_effects/RUS effects (Russia).txt";

const FIXTURE_PATH: &str = "common/scripted_effects/bench.txt";
const KEYED_PATH: &str = "common/keyed/bench.txt";
const KEYED_FIELDS: usize = 80;
const KEYED_ENTITIES: usize = 200;
const FIXTURE_RULES: &str = r#"
types = { type[scripted_effect] = { path = "common/scripted_effects" } }
scripted_effect = {
    cost = int
    alias_name[effect] = alias_match_left[effect]
}
alias[effect:add_political_power] = int
alias[effect:set_country_flag] = scalar
alias[effect:every_owned_state] = {
    limit = { alias_name[trigger] = alias_match_left[trigger] }
    alias_name[effect] = alias_match_left[effect]
}
alias[trigger:is_core_of] = scope[country]
"#;
const FIXTURE_LEAVES: usize = 200;

fn projects_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CWTOOLS_PROJECTS") {
        return Some(PathBuf::from(dir));
    }
    // crates/driver -> repo root is ../../.., siblings sit next to it.
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    repo.join("..").canonicalize().ok()
}

fn rules_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CWTOOLS_RULES") {
        return Some(PathBuf::from(dir));
    }
    let dir = projects_dir()?.join("cwtools-hoi4-config/Config");
    dir.is_dir().then_some(dir)
}

fn corpus_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CWTOOLS_CORPUS") {
        return Some(PathBuf::from(dir));
    }
    let dir = projects_dir()?.join("Kaiserreich-4-Development");
    dir.is_dir().then_some(dir)
}

fn read_corpus_file(root: &Path) -> Option<(String, String)> {
    let path = root.join(CORPUS_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => Some((CORPUS_FILE.to_string(), text)),
        Err(e) => {
            eprintln!("validate_hot: could not read {}: {e}", path.display());
            None
        }
    }
}

fn fixture_script() -> String {
    let mut body = String::new();
    for i in 0..FIXTURE_LEAVES {
        body.push_str(&format!(
            "fx_{i} = {{\n\
             cost = {i}\n\
             add_political_power = {i}\n\
             set_country_flag = flag_{i}\n\
             every_owned_state = {{\n\
             limit = {{ is_core_of = ROOT }}\n\
             add_political_power = 1\n\
             }}\n\
             }}\n"
        ));
    }
    body
}

fn keyed_rules() -> String {
    let mut rules = String::from(
        "types = { type[keyed_entity] = { path = \"common/keyed\" } }\nkeyed_entity = {\n",
    );
    for i in 0..KEYED_FIELDS {
        rules.push_str(&format!("    field_{i} = int\n"));
    }
    rules.push_str("}\n");
    rules
}

fn keyed_script() -> String {
    let mut body = String::new();
    for entity in 0..KEYED_ENTITIES {
        body.push_str(&format!("ent_{entity} = {{\n"));
        for field in 0..KEYED_FIELDS {
            body.push_str(&format!("    field_{field} = {entity}\n"));
        }
        body.push_str("}\n");
    }
    body
}

fn index_file(
    ruleset: &RuleSet,
    ast: &cwtools_parser::ast::ParsedFile,
    file_path: &str,
    table: &StringTable,
) -> TypeIndex {
    let mut idx = TypeIndex::new();
    idx.merge(
        file_path,
        collect_type_instances(ruleset, ast, file_path, table),
    );
    for vars in
        collect_defined_variables_from_rules(ruleset, ast, file_path, table, None).into_values()
    {
        for v in vars {
            idx.var_index.add_name(&v.name);
        }
    }
    idx
}

fn bench_one(
    c: &mut Criterion,
    name: &str,
    ast: &cwtools_parser::ast::ParsedFile,
    file_path: &str,
    prepared: &Prepared,
) {
    black_box(validate_prepared(ast, file_path, prepared));
    c.bench_function(name, |b| {
        b.iter(|| {
            black_box(validate_prepared(
                black_box(ast),
                black_box(file_path),
                black_box(prepared),
            ))
        })
    });
}

fn bench_validate_hot(c: &mut Criterion) {
    let table = StringTable::new();
    let fixture_ruleset = ast_to_ruleset(&parse_string(FIXTURE_RULES, &table), &table);
    let fixture_source = fixture_script();
    let fixture_ast = parse_string(&fixture_source, &table);
    assert!(
        !fixture_ast.arena.leaves.is_empty(),
        "validate_hot: fixture parsed to no leaves"
    );
    assert!(
        fixture_ruleset
            .types
            .iter()
            .any(|t| cwtools_index::check_path_dir(&t.path_options, FIXTURE_PATH)),
        "validate_hot: fixture path matches no type; the inner loop would not run"
    );
    let fixture_index = index_file(&fixture_ruleset, &fixture_ast, FIXTURE_PATH, &table);
    let fixture_prepared = Prepared {
        ruleset: &fixture_ruleset,
        table: &table,
        game: Some(Game::Hoi4),
        type_index: Some(&fixture_index),
        modifier_keys: None,
        loc_index: None,
        extra_loc_keys: None,
        inline_scripts: None,
        registry: None,
        scope_checks: false,
        var_checks: false,
    };
    bench_one(
        c,
        "validate_prepared/fixture",
        &fixture_ast,
        FIXTURE_PATH,
        &fixture_prepared,
    );

    let keyed_ruleset = ast_to_ruleset(&parse_string(&keyed_rules(), &table), &table);
    let keyed_source = keyed_script();
    let keyed_ast = parse_string(&keyed_source, &table);
    assert_eq!(
        keyed_ast.arena.leaves.len(),
        KEYED_ENTITIES * (KEYED_FIELDS + 1),
        "validate_hot: keyed fixture did not parse one leaf per entity plus one per field"
    );
    assert!(
        keyed_ruleset
            .types
            .iter()
            .any(|t| cwtools_index::check_path_dir(&t.path_options, KEYED_PATH)),
        "validate_hot: keyed fixture path matches no type; the inner loop would not run"
    );
    let keyed_rule_idx = keyed_ruleset
        .type_rules_idx()
        .get("keyed_entity")
        .copied()
        .expect("validate_hot: keyed fixture has no type rules");
    let RootRule::TypeRule(_, (rule_type, _)) = &keyed_ruleset.root_rules[keyed_rule_idx] else {
        panic!("validate_hot: keyed fixture root rule is not a type rule");
    };
    let RuleType::NodeRule { rules, .. } = rule_type else {
        panic!("validate_hot: keyed fixture type is not a node rule");
    };
    assert_eq!(
        rules.len(),
        KEYED_FIELDS,
        "validate_hot: keyed fixture is not a wide specific-field type"
    );
    let keyed_index = index_file(&keyed_ruleset, &keyed_ast, KEYED_PATH, &table);
    let keyed_prepared = Prepared {
        ruleset: &keyed_ruleset,
        table: &table,
        game: Some(Game::Hoi4),
        type_index: Some(&keyed_index),
        modifier_keys: None,
        loc_index: None,
        extra_loc_keys: None,
        inline_scripts: None,
        registry: None,
        scope_checks: false,
        var_checks: false,
    };
    bench_one(
        c,
        "validate_prepared/keyed_entity",
        &keyed_ast,
        KEYED_PATH,
        &keyed_prepared,
    );

    let Some(rules) = rules_dir() else {
        eprintln!(
            "validate_hot: no ruleset. Set CWTOOLS_RULES to a cwtools-hoi4-config/Config checkout"
        );
        return;
    };
    let Some(corpus) = corpus_dir() else {
        eprintln!(
            "validate_hot: no corpus. Set CWTOOLS_CORPUS to a Kaiserreich-4-Development checkout"
        );
        return;
    };
    let Some((file_path, source)) = read_corpus_file(&corpus) else {
        return;
    };

    let (ruleset, rule_errors) = match load_rules(&RulesInput::Dir(rules), &table) {
        Ok(loaded) => loaded,
        Err(e) => {
            eprintln!("validate_hot: could not load rules: {e}");
            return;
        }
    };
    assert!(
        rule_errors.is_empty(),
        "validate_hot: rules problems: {rule_errors:?}"
    );

    let ast = parse_string(&source, &table);
    assert!(
        !ast.arena.leaves.is_empty(),
        "validate_hot: {CORPUS_FILE} parsed to no leaves"
    );
    assert!(
        ruleset
            .types
            .iter()
            .any(|t| cwtools_index::check_path_dir(&t.path_options, &file_path)),
        "validate_hot: {CORPUS_FILE} matches no type; the inner loop would not run"
    );

    let type_index = index_file(&ruleset, &ast, &file_path, &table);
    let scope_registry = build_scope_registry_arc(&ruleset, Some(Game::Hoi4));
    let prepared = Prepared {
        ruleset: &ruleset,
        table: &table,
        game: Some(Game::Hoi4),
        type_index: Some(&type_index),
        modifier_keys: None,
        loc_index: None,
        extra_loc_keys: None,
        inline_scripts: None,
        registry: scope_registry.as_ref(),
        scope_checks: true,
        var_checks: true,
    };
    bench_one(
        c,
        "validate_prepared/scripted_effects",
        &ast,
        &file_path,
        &prepared,
    );
}

criterion_group!(benches, bench_validate_hot);
criterion_main!(benches);
