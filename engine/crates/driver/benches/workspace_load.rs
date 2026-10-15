//! Discovery, localisation parsing, and localisation indexing over Millennium Dawn.
//! Finds the corpus and rules through the same checkout helpers as `rules_hot`.
//! Run with `cargo bench -p cwtools_driver --bench workspace_load`.
//! Inputs are page-cache warm. Discovery, parsing, and indexing exclude output
//! drops; key normalization includes freeing each lowercase temporary.

use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use cwtools_driver::{
    DiscoveryPolicy, RulesInput, WorkspaceDiscoveryRequest, discover_workspace, load_rules,
    workspace_discovery_config,
};
use cwtools_file_manager::{FileKind, ScanBudget};
use cwtools_localization::{LocIndex, LocService};
use cwtools_string_table::string_table::StringTable;
use cwtools_test_support::{checkout, rules_dir};

fn bench_workspace_load(c: &mut Criterion) {
    let (Some(corpus), Some(rules)) = (checkout("CWTOOLS_CORPUS", "Millennium-Dawn"), rules_dir())
    else {
        eprintln!(
            "workspace_load: set CWTOOLS_CORPUS and CWTOOLS_RULES, or provide sibling checkouts"
        );
        return;
    };
    let table = StringTable::new();
    let (ruleset, errors) = load_rules(&RulesInput::Dir(rules), &table).expect("load rules");
    assert!(
        errors.is_empty(),
        "workspace_load: rules problems: {errors:?}"
    );
    let config = workspace_discovery_config(&corpus, Some(&ruleset));
    let discovery_request = || WorkspaceDiscoveryRequest {
        roots: vec![corpus.clone()],
        kinds: vec![FileKind::Script, FileKind::Localisation],
        config: config.clone(),
        policy: DiscoveryPolicy::Workspace,
    };
    let discovery = discover_workspace(discovery_request()).expect("discover corpus");
    assert!(discovery.failures.is_empty(), "corpus discovery failed");
    let paths: Vec<_> = discovery
        .files
        .iter()
        .filter(|file| file.kind == FileKind::Localisation)
        .map(|file| file.path.clone())
        .collect();
    assert!(!paths.is_empty(), "corpus has no localisation files");

    let mut group = c.benchmark_group("workspace_load");
    group.bench_function("discovery", |b| {
        b.iter_batched(
            discovery_request,
            |request| black_box(discover_workspace(request).expect("discover corpus")),
            BatchSize::PerIteration,
        )
    });
    group.bench_function("loc_read_parse", |b| {
        b.iter_batched(
            || paths.clone(),
            |paths| black_box(LocService::from_paths(paths, ScanBudget::default(), None)),
            BatchSize::PerIteration,
        )
    });
    let service = LocService::from_paths(paths, ScanBudget::default(), None);
    assert!(
        service.errors().is_empty(),
        "localisation read/parse failed"
    );
    let entries: usize = service.files().iter().map(|file| file.entries.len()).sum();
    assert!(entries > 0, "corpus parsed to no localisation entries");
    let keys = LocIndex::build(&service).union().len();
    assert!(keys > 0, "corpus indexed to no localisation keys");
    eprintln!(
        "workspace_load: {} discovered files, {} localisation files, {entries} entries, {keys} keys",
        discovery.files.len(),
        service.files().len(),
    );
    group.bench_function("loc_key_normalization", |b| {
        b.iter(|| {
            for file in service.files() {
                for entry in &file.entries {
                    black_box(entry.key.to_lowercase());
                }
            }
        })
    });
    group.bench_function("loc_index", |b| {
        b.iter_batched(
            || (),
            |()| black_box(LocIndex::build(&service)),
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

criterion_group!(benches, bench_workspace_load);
criterion_main!(benches);
