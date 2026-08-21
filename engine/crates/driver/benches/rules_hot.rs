//! Editor hot paths over a real ruleset: the work a keystroke triggers, plus
//! the one-off load that has to finish before any of them can run.
//!
//! `ruleset/load_from_dir` is that load: walk the `.cwt` directory, parse and
//! convert every file, expand single_aliases, reindex. Every CLI run and every
//! editor startup waits on it before anything else can begin.
//!
//! The rule-resolution cases use the full HOI4 `.cwt` set, where the rule trees
//! are big enough for a deep clone to show up:
//!
//!   `rules_at_pos`        one root descent -> subtype merge -> child rules.
//!                         Completion and hover each run this per request.
//!   `value_rules_for_key` per-leaf match + alias-overload expansion. Semantic
//!                         tokens runs it once per leaf in the document.
//!
//! The ScopeRegistry cases exercise the loaded config's name lookup, named-link
//! resolution, construction paths, and the validation save/restore lifecycle.
//!
//! The ruleset comes from a `cwtools-hoi4-config` checkout, same input as the
//! corpus guard, named by `CWTOOLS_RULES` or found under `CWTOOLS_PROJECTS`:
//!
//!   CWTOOLS_RULES=/path/to/cwtools-hoi4-config/Config cargo bench -p cwtools_driver
//!
//! Without it there is nothing to resolve against and the bench prints why and
//! measures nothing.

use std::hint::black_box;
use std::path::PathBuf;

use criterion::{Criterion, criterion_group, criterion_main};
use cwtools_driver::{RulesInput, load_rules};
use cwtools_game::constants::Game;
use cwtools_game::scope_engine::{ScopeContext, ScopeResult};
use cwtools_index::TypeIndex;
use cwtools_parser::parser::parse_string;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::position::{rules_at_pos, value_rules_for_key};
use cwtools_validation::{Prepared, build_scope_registry_arc};

/// A national focus with a deep effect/trigger body: the shape an editor sits
/// inside while typing. `focus` is a subtype-bearing type, so resolving a
/// position in here goes through the subtype merge.
const FOCUS_FILE: &str = r#"
focus_tree = {
    id = bench_tree
    country = {
        factor = 0
        modifier = {
            add = 10
            tag = GER
        }
    }
    default = no
    focus = {
        id = bench_focus
        icon = GFX_goal_generic_political_pressure
        x = 4
        y = 0
        cost = 10
        available = {
            has_war = no
            OR = {
                has_government = fascism
                AND = {
                    has_country_flag = bench_flag
                    NOT = { has_idea = bench_idea }
                }
            }
        }
        ai_will_do = {
            factor = 5
            modifier = {
                factor = 0
                has_war = yes
            }
        }
        completion_reward = {
            add_political_power = 120
            add_stability = 0.05
            every_owned_state = {
                limit = { is_core_of = ROOT }
                add_extra_state_shared_building_slots = 1
            }
            country_event = { id = bench.1 days = 3 }
            hidden_effect = {
                set_country_flag = bench_done
                add_ideas = bench_idea
            }
        }
    }
}
"#;

/// The line the cursor sits on: inside `completion_reward`, one clause below the
/// entity root, where an editor spends most of its time.
const CURSOR_LINE: &str = "add_political_power";

/// Keys resolved per leaf by the semantic-token walk. `add_political_power`,
/// `country_event` and `set_country_flag` all match through `alias[effect:…]`,
/// whose overload bodies are the largest trees in the ruleset.
const LEAF_KEYS: &[&str] = &[
    "add_political_power",
    "add_stability",
    "every_owned_state",
    "country_event",
    "hidden_effect",
    "set_country_flag",
    "add_ideas",
    "not_a_real_key",
];

/// Parser coordinates (1-based line, 0-based column) of the first line whose
/// trimmed text starts with `needle`, pointing at the key. Derived rather than
/// hardcoded so editing the fixture can't silently move the cursor.
fn cursor_at(text: &str, needle: &str) -> (u32, u16) {
    let (idx, line) = text
        .lines()
        .enumerate()
        .find(|(_, l)| l.trim_start().starts_with(needle))
        .unwrap_or_else(|| panic!("fixture has no line starting with {needle}"));
    let indent = line.len() - line.trim_start().len();
    (idx as u32 + 1, indent as u16)
}

fn rules_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CWTOOLS_RULES") {
        return Some(PathBuf::from(dir));
    }
    let projects = std::env::var("CWTOOLS_PROJECTS").ok()?;
    let dir = PathBuf::from(projects).join("cwtools-hoi4-config/Config");
    dir.is_dir().then_some(dir)
}

fn bench_rules_hot(c: &mut Criterion) {
    let Some(dir) = rules_dir() else {
        eprintln!(
            "rules_hot: no ruleset. Set CWTOOLS_RULES to a cwtools-hoi4-config/Config checkout"
        );
        return;
    };
    let table = StringTable::new();
    let (ruleset, rule_errors) = match load_rules(&RulesInput::Dir(dir.clone()), &table) {
        Ok(loaded) => loaded,
        Err(e) => {
            eprintln!("rules_hot: could not load rules: {e}");
            return;
        }
    };
    assert!(
        rule_errors.is_empty(),
        "rules_hot: rules problems: {rule_errors:?}"
    );
    let type_index = TypeIndex::default();
    let scope_registry =
        build_scope_registry_arc(&ruleset, Some(Game::Hoi4)).expect("HOI4 scope registry loads");
    let character = scope_registry.id_of("character").unwrap_or_else(|| {
        panic!(
            "character scope resolves; loaded scopes: {:?}",
            ruleset.scope_inputs
        )
    });
    let scope_base = ScopeContext::from_registry(scope_registry.clone(), character);
    let mut scope_ctx = scope_base.clone();
    assert!(matches!(
        scope_ctx.change_scope("owner"),
        ScopeResult::NewScope { .. }
    ));

    let prepared = Prepared {
        ruleset: &ruleset,
        table: &table,
        game: Some(Game::Hoi4),
        type_index: Some(&type_index),
        modifier_keys: None,
        loc_index: None,
        extra_loc_keys: None,
        inline_scripts: None,
        registry: None,
        scope_checks: false,
        var_checks: false,
    };
    let ast = parse_string(FOCUS_FILE, &table);
    let path = "common/national_focus/bench.txt";
    let (line, col) = cursor_at(FOCUS_FILE, CURSOR_LINE);

    // Guard the fixture: a cursor that resolves to nothing would still benchmark,
    // just not the path we care about.
    let ctx = rules_at_pos(&ast, path, &prepared, line, col, true)
        .expect("cursor should resolve to a rule context");
    assert!(
        !ctx.child_rules.is_empty(),
        "cursor should land in a block with rules"
    );

    // Each iteration re-reads the whole `.cwt` directory from the page cache and
    // interns into a fresh table, so it measures the load a cold CLI run and an
    // LSP startup both pay.
    c.bench_function("ruleset/load_from_dir", |b| {
        b.iter(|| {
            black_box(load_rules(
                &RulesInput::Dir(dir.clone()),
                &StringTable::new(),
            ))
        })
    });

    c.bench_function("rules_at_pos/completion", |b| {
        b.iter(|| {
            black_box(rules_at_pos(
                &ast,
                path,
                &prepared,
                black_box(line),
                black_box(col),
                true,
            ))
        })
    });

    c.bench_function("rules_at_pos/hover", |b| {
        b.iter(|| {
            black_box(rules_at_pos(
                &ast,
                path,
                &prepared,
                black_box(line),
                black_box(col),
                false,
            ))
        })
    });

    let child_rules = ctx.child_rules;
    c.bench_function("value_rules_for_key/effect_block", |b| {
        b.iter(|| {
            for key in LEAF_KEYS {
                black_box(value_rules_for_key(
                    &ruleset,
                    Some(&type_index),
                    &child_rules,
                    black_box(key),
                ));
            }
        })
    });

    c.bench_function("scope_registry/config/id_of", |b| {
        b.iter(|| black_box(scope_registry.id_of(black_box("character"))))
    });
    c.bench_function("scope_registry/config/links/get", |b| {
        b.iter(|| black_box(scope_registry.links.get(black_box("owner"))))
    });
    let mut scope_ctx = scope_base.clone();
    c.bench_function("scope_registry/config/change_scope/owner", |b| {
        b.iter(|| {
            let saved = scope_ctx.save();
            let result = scope_ctx.change_scope(black_box("owner"));
            scope_ctx.restore(saved);
            black_box(result)
        })
    });
    c.bench_function("scope_registry/config/build", |b| {
        b.iter(|| {
            black_box(build_scope_registry_arc(
                black_box(&ruleset),
                black_box(Some(Game::Hoi4)),
            ))
        })
    });
}

criterion_group!(benches, bench_rules_hot);
criterion_main!(benches);
