//! Completion list building over synthetic fixtures sized like Millennium Dawn.
//!
//! `completion_synthetic` times `completions_from_rules` and `value_completions`
//! plus the `prepare_context_items` cap that follows them, over a ruleset with
//! thousands of aliases, modifiers and type instances. It needs no checkout.
//!
//! `loc_completion_keys` times picking the top localisation keys for a token
//! out of ~400k keys. `before` is the original ordered-set scan, `linear` is
//! `select_loc_keys`, and `indexed` is `LocKeyIndex::select`. The bench asserts
//! all three return the same set before timing them. `index_build` is the
//! once-per-workspace-scan cost of building the index.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use cwtools_game::constants::Game;
use cwtools_game::scope_registry::{ScopeInput, ScopeRegistry};
use cwtools_lsp::bench_support::{
    CONTEXT_CAP, CONTEXT_COMPLETE_THRESHOLD, LocKeyIndex, ValueCompletionSets,
    completions_from_rules, expanded_modifier_scopes, prepare_context_items, select_loc_keys,
    value_completions,
};
use cwtools_rules::rules_types::{
    NewField, NewRule, Options, RuleSet, RuleType, TypeType, ValueType,
};

const EXACT_EFFECTS: usize = 600;
const SCRIPTED_EFFECTS: usize = 8_000;
const PLAIN_MODIFIERS: usize = 5_000;
const TEMPLATED_BUILDINGS: usize = 3_000;
const STATES: usize = 2_000;

const LOC_KEYS: usize = 399_781;
const OWNERS: [&str; 14] = [
    "mds",
    "politics",
    "focus",
    "hol",
    "eng",
    "usa",
    "ger",
    "sov",
    "generic",
    "decision",
    "idea",
    "event",
    "state",
    "equipment",
];
const STEMS: [&str; 8] = [
    "title", "desc", "tooltip", "effect", "flavor", "name", "option", "log",
];

fn alias_usage(cat: &str) -> Vec<NewRule> {
    vec![(
        RuleType::LeafRule {
            left: NewField::AliasField(cat.to_string()),
            right: NewField::AliasField(cat.to_string()),
        },
        Options::default(),
    )]
}

fn synthetic_ruleset() -> RuleSet {
    let mut rs = RuleSet::new();
    for i in 0..EXACT_EFFECTS {
        let scopes = if i % 2 == 0 {
            vec!["country".to_string()]
        } else {
            Vec::new()
        };
        rs.aliases.push((
            format!("effect:eff_{:04}", i),
            (
                RuleType::LeafRule {
                    left: NewField::SpecificField(format!("alias[effect:eff_{:04}]", i)),
                    right: NewField::ScalarField,
                },
                Options {
                    required_scopes: scopes,
                    ..Options::default()
                },
            ),
        ));
    }
    for name in ["if", "else_if", "else"] {
        rs.aliases.push((
            format!("effect:{}", name),
            (
                RuleType::NodeRule {
                    left: NewField::SpecificField(format!("alias[effect:{}]", name)),
                    rules: alias_usage("effect").into(),
                },
                Options::default(),
            ),
        ));
    }
    rs.aliases.push((
        "effect:<scripted_effect>".to_string(),
        (
            RuleType::LeafRule {
                left: NewField::SpecificField("alias[effect:<scripted_effect>]".to_string()),
                right: NewField::ValueField(ValueType::Bool),
            },
            Options::default(),
        ),
    ));
    for i in 0..PLAIN_MODIFIERS {
        rs.modifiers
            .push((format!("mod_{:04}", i), "country".to_string()));
    }
    rs.modifiers.push((
        "production_speed_<building>_factor".to_string(),
        "state".to_string(),
    ));
    rs.modifier_categories
        .insert("country".to_string(), vec!["country".to_string()]);
    rs.modifier_categories
        .insert("state".to_string(), vec!["state".to_string()]);
    rs.reindex();
    rs
}

fn synthetic_info() -> cwtools_info::InfoService {
    let mut info = cwtools_info::InfoService::new();
    let inst = |name: String| cwtools_info::TypeInstance {
        name,
        location: cwtools_info::SourceLocation {
            line: 1,
            col: 0,
            end: (1, 0),
        },
        primary_loc_key: None,
        required_loc_keys: Vec::new(),
    };
    let mut per_type: HashMap<String, Vec<cwtools_info::TypeInstance>> = HashMap::new();
    per_type.insert(
        "scripted_effect".to_string(),
        (0..SCRIPTED_EFFECTS)
            .map(|i| inst(format!("se_do_things_{:05}", i)))
            .collect(),
    );
    per_type.insert(
        "building".to_string(),
        (0..TEMPLATED_BUILDINGS)
            .map(|i| inst(format!("building_{:04}", i)))
            .collect(),
    );
    per_type.insert(
        "state".to_string(),
        (0..STATES).map(|i| inst(format!("{}", i + 1))).collect(),
    );
    Arc::make_mut(&mut info.type_index).merge("file:///bench/defs.txt", per_type);
    info
}

fn country_registry() -> ScopeRegistry {
    ScopeRegistry::from_config(
        &[ScopeInput {
            name: "Country".to_string(),
            aliases: vec!["country".to_string()],
            is_subscope_of: Vec::new(),
        }],
        &[],
        Game::Stellaris,
    )
}

fn bench_completion_synthetic(c: &mut Criterion) {
    let rs = synthetic_ruleset();
    let info = synthetic_info();
    let reg = country_registry();
    let country = reg.id_of("country").expect("country scope");
    let modifier_keys = cwtools_validation::build_modifier_keys(&rs, &info.type_index);
    eprintln!(
        "completion_synthetic: {} aliases, {} modifier keys, {SCRIPTED_EFFECTS} scripted \
         effects, {STATES} states",
        rs.aliases.len(),
        modifier_keys.len(),
    );

    let modifier_scopes = expanded_modifier_scopes(&rs, &info.type_index);
    let effect_rules = alias_usage("effect");
    let modifier_rules = alias_usage("modifier");
    let effect_rules_dup: Vec<NewRule> = effect_rules
        .iter()
        .cloned()
        .chain(effect_rules.iter().cloned())
        .collect();
    let state_value_rules: Vec<NewRule> = vec![(
        RuleType::LeafRule {
            left: NewField::SpecificField("add_state_core".to_string()),
            right: NewField::TypeField(TypeType::Simple("state".to_string())),
        },
        Options::default(),
    )];

    let key_items = |rules: &[NewRule], token: &str| {
        let (items, dropped) = completions_from_rules(
            rules,
            &rs,
            &info,
            "stellaris",
            &modifier_keys,
            &modifier_scopes,
            Some(&reg),
            Some(country),
            token,
        );
        prepare_context_items(
            items,
            dropped,
            token,
            true,
            true,
            CONTEXT_COMPLETE_THRESHOLD,
            CONTEXT_CAP,
        )
        .0
        .len()
    };

    let mut group = c.benchmark_group("completion_synthetic");
    group.sample_size(30);
    group.bench_function("effect_key/no_token", |b| {
        b.iter(|| key_items(&effect_rules, ""))
    });
    group.bench_function("effect_key/token_if", |b| {
        b.iter(|| key_items(&effect_rules, "if"))
    });
    group.bench_function("effect_key/token_add_p", |b| {
        b.iter(|| key_items(&effect_rules, "add_p"))
    });
    group.bench_function("effect_key/dup_arm", |b| {
        b.iter(|| key_items(&effect_rules_dup, ""))
    });
    group.bench_function("modifier_key/scoped", |b| {
        b.iter(|| key_items(&modifier_rules, ""))
    });
    group.bench_function("state_value/token_28", |b| {
        b.iter(|| {
            let (items, dropped) = value_completions(
                &state_value_rules,
                &rs,
                &info,
                Some(&reg),
                "stellaris",
                ValueCompletionSets {
                    modifier_keys: &modifier_keys,
                    modifier_scopes: &modifier_scopes,
                    loc_keys: &HashSet::new(),
                },
                Some(country),
                "28",
            );
            prepare_context_items(
                items,
                dropped,
                "28",
                true,
                true,
                CONTEXT_COMPLETE_THRESHOLD,
                CONTEXT_CAP,
            )
            .0
            .len()
        })
    });
    group.finish();
}

fn bench_loc_completion_keys(c: &mut Criterion) {
    let keys: HashSet<String> = (0..LOC_KEYS)
        .map(|i| {
            format!(
                "{}_{}_{:06}",
                OWNERS[i % OWNERS.len()],
                STEMS[(i / OWNERS.len()) % STEMS.len()],
                i
            )
        })
        .collect();
    let overlay: HashSet<String> = (0..400)
        .map(|i| format!("unsaved_open_yml_key_{:04}", i))
        .collect();
    let bytes: usize = keys.iter().map(|k| k.len()).sum();
    eprintln!(
        "loc_completion_keys: {} keys ({} KiB of key text), {} overlay keys, cap {CONTEXT_CAP}",
        keys.len(),
        bytes / 1024,
        overlay.len(),
    );
    let candidates = || {
        keys.iter()
            .map(String::as_str)
            .chain(overlay.iter().map(String::as_str))
    };

    let mut group = c.benchmark_group("loc_completion_keys");
    group.sample_size(10);
    group.bench_function("index_build", |b| {
        b.iter(|| LocKeyIndex::build(keys.iter().map(String::as_str)))
    });
    let index = LocKeyIndex::build(keys.iter().map(String::as_str));

    for token in ["", "f", "mds_f", "mdsfoc", "zqxv", "lltt"] {
        let linear = select_loc_keys(candidates(), token, CONTEXT_CAP);
        let indexed = index.select(token, overlay.iter().map(String::as_str), CONTEXT_CAP);
        let baseline = reference::select(candidates(), token, CONTEXT_CAP);
        assert_eq!(baseline, linear, "token {token:?} diverged (linear)");
        assert_eq!(baseline, indexed, "token {token:?} diverged (indexed)");

        let label = if token.is_empty() { "empty" } else { token };
        group.bench_function(format!("before/{label}"), |b| {
            b.iter(|| reference::select(candidates(), token, CONTEXT_CAP).len())
        });
        group.bench_function(format!("linear/{label}"), |b| {
            b.iter(|| select_loc_keys(candidates(), token, CONTEXT_CAP).len())
        });
        group.bench_function(format!("indexed/{label}"), |b| {
            b.iter(|| {
                index
                    .select(token, overlay.iter().map(String::as_str), CONTEXT_CAP)
                    .len()
            })
        });
    }
    group.finish();
}

/// The selection as it was before `select_loc_keys`, kept as the baseline.
mod reference {
    use std::collections::{BTreeSet, HashSet};

    fn subsequence_match(haystack: &str, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        let mut needle_it = needle.chars().flat_map(char::to_lowercase).peekable();
        for c in haystack.chars().flat_map(char::to_lowercase) {
            if needle_it.peek() == Some(&c) {
                needle_it.next();
            }
        }
        needle_it.peek().is_none()
    }

    pub fn select<'a>(
        keys: impl Iterator<Item = &'a str>,
        token: &str,
        cap: usize,
    ) -> HashSet<String> {
        let mut selected = BTreeSet::new();
        for key in keys.filter(|key| subsequence_match(key, token)) {
            let ranked = (
                !key.get(..token.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(token)),
                key,
            );
            if selected.len() < cap {
                selected.insert(ranked);
            } else if selected.last().is_some_and(|largest| ranked < *largest)
                && selected.insert(ranked)
            {
                selected.pop_last();
            }
        }
        selected
            .into_iter()
            .map(|(_, key)| key.to_owned())
            .collect()
    }
}

criterion_group!(
    benches,
    bench_completion_synthetic,
    bench_loc_completion_keys
);
criterion_main!(benches);
