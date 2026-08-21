use criterion::{Criterion, criterion_group, criterion_main};
use cwtools_game::constants::Game;
use cwtools_game::scope_engine::{ScopeContext, ScopeId, ScopeResult};
use cwtools_game::scope_registry::{LinkInput, ScopeInput, ScopeRegistry};
use std::hint::black_box;
use std::sync::Arc;

// Stellaris has a populated hardcoded link table (HOI4 links come from config),
// so it exercises the real resolve path. Root = Country (200), matching the
// existing unit tests. Mixed case + prev/dotted keys exercise #10 (lowercase
// alloc), #11 (pop_n), #12 (is_subscope_or_eq).
const KEYS: &[&str] = &[
    "owner",
    "Owner",
    "controller",
    "PREV",
    "prevprev",
    "root",
    "from",
    "fromfrom",
    "leader",
    "planet",
    "star",
    "fleet",
    "ship",
    "capital_scope",
    "owner.capital_scope",
    "system",
];

fn bench_change_scope(c: &mut Criterion) {
    let base = ScopeContext::new(Game::Stellaris, ScopeId(200));
    c.bench_function("change_scope/stellaris_mixed", |b| {
        b.iter(|| {
            let mut ctx = base.clone();
            for k in KEYS {
                black_box(ctx.change_scope(black_box(k)));
            }
        })
    });
}

fn config_registry() -> Arc<ScopeRegistry> {
    Arc::new(ScopeRegistry::from_config(
        &[
            ScopeInput {
                name: "Country".to_string(),
                aliases: vec!["country".to_string()],
                is_subscope_of: Vec::new(),
            },
            ScopeInput {
                name: "Character".to_string(),
                aliases: vec!["character".to_string()],
                is_subscope_of: vec!["country".to_string()],
            },
            ScopeInput {
                name: "State".to_string(),
                aliases: vec!["state".to_string()],
                is_subscope_of: Vec::new(),
            },
        ],
        &[LinkInput {
            name: "owner".to_string(),
            output_scope: Some("country".to_string()),
            input_scopes: vec!["country".to_string()],
            prefix: None,
            from_data: false,
            data_source: Vec::new(),
        }],
        Game::Hoi4,
    ))
}

fn bench_config_registry(c: &mut Criterion) {
    let registry = config_registry();
    let country = registry.id_of("country").expect("country resolves");
    let character = registry.id_of("character").expect("character resolves");
    let base = ScopeContext::from_registry(Arc::clone(&registry), character);

    let mut ctx = base.clone();
    assert_eq!(
        ctx.change_scope("owner"),
        ScopeResult::NewScope {
            scope: country,
            ignore_keys: Vec::new(),
        }
    );

    c.bench_function("scope_registry/id_of/lowercase", |b| {
        b.iter(|| black_box(registry.id_of(black_box("country"))))
    });
    c.bench_function("scope_registry/id_of/mixed_case", |b| {
        b.iter(|| black_box(registry.id_of(black_box("Character"))))
    });
    c.bench_function("scope_registry/links/get", |b| {
        b.iter(|| black_box(registry.links.get(black_box("owner"))))
    });
    let mut ctx = base.clone();
    c.bench_function("scope_registry/change_scope/owner", |b| {
        b.iter(|| {
            let saved = ctx.save();
            let result = ctx.change_scope(black_box("owner"));
            ctx.restore(saved);
            black_box(result)
        })
    });

    let partial_scopes = vec![ScopeInput {
        name: "Country".to_string(),
        aliases: vec!["country".to_string()],
        is_subscope_of: Vec::new(),
    }];
    let partial_links = Vec::new();
    c.bench_function("scope_registry/build/partial_stellaris", |b| {
        b.iter(|| {
            black_box(ScopeRegistry::from_config(
                black_box(&partial_scopes),
                black_box(&partial_links),
                Game::Stellaris,
            ))
        })
    });
}

criterion_group!(benches, bench_change_scope, bench_config_registry);
criterion_main!(benches);
