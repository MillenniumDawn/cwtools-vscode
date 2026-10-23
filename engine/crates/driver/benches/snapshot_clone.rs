//! Cost of the workspace-scan pass 2 index snapshot.
//!
//! Pass 2 clones an `Arc<TypeIndex>` under a brief `info_service` read guard
//! (`crates/lsp/src/scan/workspace.rs`) so rayon holds no locks. The Arc clone is
//! the entire lock hold. If a file is indexed while pass 2 still owns that
//! snapshot, the first write instead pays the deep clone through
//! `Arc::make_mut`.
//!
//! `type_index/snapshot_arc_corpus` measures the lock hold and
//! `type_index/first_write_cow_corpus` measures the deferred copy. The original
//! deep-clone and dynamic-value benches remain beside them to show where that
//! deferred cost goes.
//!
//! Inputs are the checkouts the corpus guard uses:
//!
//!   CWTOOLS_RULES=/path/to/cwtools-hoi4-config/Config \
//!   CWTOOLS_CORPUS=/path/to/Millennium-Dawn \
//!     cargo bench -p cwtools_driver --bench snapshot_clone
//!
//! Either can also be found under `CWTOOLS_PROJECTS`, or as a sibling of this
//! repo. The corpus case prints why and measures nothing when a checkout is
//! missing; `type_index/clone_synthetic` always runs so a machine with no
//! siblings still gets a number.
//!
//! To include the full HOI4 base-game + MD index, set `CWTOOLS_VANILLA` to a
//! game install containing `common/events/localisation` and run the same bench.
//! This opt-in tier loads a `cwtools_driver::Session`, so its layered index uses
//! the same base-game/mod merge semantics as validation. It reports input
//! revisions, index shape and Linux process RSS observations. Criterion retains
//! its per-sample raw data under `target/criterion`; process RSS is not an
//! allocation-level measurement and `/proc` is Linux-only.

use std::collections::HashMap;
use std::hint::black_box;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use cwtools_driver::{RulesInput, Session, SessionConfig, index_game_dir, load_rules};
use cwtools_game::constants::Game;
use cwtools_index::{
    SourceLocation, TypeIndex, TypeInstance, dynamic_values::NamedValueIndex,
    variable_defining_effects,
};
use cwtools_string_table::string_table::StringTable;
use cwtools_test_support::{checkout, rules_dir};

/// Millennium Dawn rather than Kaiserreich: it is the larger of the two pinned
/// corpora and the one whose flag-heavy scripts fill `value_set_values`.
const CORPUS_NAME: &str = "Millennium-Dawn";
const FULL_SAMPLE_COUNT: usize = 10;

/// Synthetic fallback, sized to land in the same order of magnitude as the real
/// corpus so the fallback number is not misleading about the ratio.
const SYNTH_FILES: usize = 7000;
const SYNTH_TYPES: &[&str] = &["state", "character", "technology", "country_event"];
const SYNTH_INSTANCES_PER_TYPE_PER_FILE: usize = 6;
const SYNTH_VALUE_SETS: &[&str] = &["country_flag", "global_flag", "state_flag"];
const SYNTH_VALUES_PER_SET_PER_FILE: usize = 4;

fn synthetic_index() -> TypeIndex {
    let mut idx = TypeIndex::new();
    for file in 0..SYNTH_FILES {
        let uri = format!("file:///mod/common/{file}.txt");
        let per_type: HashMap<String, Vec<TypeInstance>> = SYNTH_TYPES
            .iter()
            .map(|&ty| {
                let instances = (0..SYNTH_INSTANCES_PER_TYPE_PER_FILE)
                    .map(|n| TypeInstance {
                        name: format!("{ty}_{file}_{n}"),
                        location: SourceLocation {
                            line: n as u32,
                            col: 0,
                            end: (n as u32, 0),
                        },
                        primary_loc_key: None,
                        required_loc_keys: Vec::new(),
                    })
                    .collect();
                (ty.to_string(), instances)
            })
            .collect();
        idx.merge(&uri, per_type);
        let sets: HashMap<String, Vec<String>> = SYNTH_VALUE_SETS
            .iter()
            .map(|&set| {
                let values = (0..SYNTH_VALUES_PER_SET_PER_FILE)
                    .map(|n| format!("{set}_{file}_{n}"))
                    .collect();
                (set.to_string(), values)
            })
            .collect();
        idx.value_set_values.merge_file(&uri, sets);
    }
    idx
}

/// How many (name, value) pairs an index holds, so the printed shape says
/// whether a small clone time means cheap work or an empty index.
fn value_count(idx: &NamedValueIndex) -> usize {
    idx.export().iter().map(|(_, vals)| vals.len()).sum()
}

fn git_revision(path: &Path) -> String {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|revision| !revision.is_empty())
        .unwrap_or_else(|| "unavailable".to_string())
}

/// Whole-process resident memory, available on Linux only. VmHWM is a
/// process-lifetime high-water mark, not an isolated allocation measurement.
fn process_memory_kib() -> Option<(usize, usize)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    };
    Some((field("VmRSS:")?, field("VmHWM:")?))
}

fn report_memory(label: &str) {
    match process_memory_kib() {
        Some((rss, high_water)) => {
            eprintln!("snapshot_clone/{label}: process VmRSS={rss} KiB, VmHWM={high_water} KiB")
        }
        None => eprintln!(
            "snapshot_clone/{label}: process RSS unavailable (Linux /proc/self/status required)"
        ),
    }
}

fn bench_index(c: &mut Criterion, label: &str, idx: &TypeIndex) {
    let instances: usize = idx.map().values().map(Vec::len).sum();
    eprintln!(
        "snapshot_clone/{label}: {instances} instances, \
         {} complex-enum values, {} value-set values",
        value_count(&idx.complex_enum_values),
        value_count(&idx.value_set_values),
    );
    let shared = Arc::new(idx.clone());
    c.bench_function(&format!("type_index/snapshot_arc_{label}"), |b| {
        b.iter(|| black_box(Arc::clone(black_box(&shared))))
    });
    c.bench_function(&format!("type_index/first_write_cow_{label}"), |b| {
        b.iter_batched(
            || (Arc::clone(&shared), Arc::clone(&shared)),
            |(mut live, snapshot)| {
                let _ = black_box(Arc::make_mut(&mut live));
                black_box((live, snapshot))
            },
            BatchSize::LargeInput,
        )
    });
    c.bench_function(&format!("type_index/clone_{label}"), |b| {
        b.iter(|| black_box(black_box(idx).clone()))
    });
    c.bench_function(
        &format!("type_index/clone_{label}_complex_enum_values"),
        |b| b.iter(|| black_box(black_box(&idx.complex_enum_values).clone())),
    );
    c.bench_function(&format!("type_index/clone_{label}_value_set_values"), |b| {
        b.iter(|| black_box(black_box(&idx.value_set_values).clone()))
    });
}

fn bench_full_index(c: &mut Criterion, idx: &TypeIndex) {
    let instances: usize = idx.map().values().map(Vec::len).sum();
    eprintln!(
        "snapshot_clone/full_hoi4_md: {instances} instances, {} complex-enum values, {} value-set values",
        value_count(&idx.complex_enum_values),
        value_count(&idx.value_set_values),
    );
    let shared = Arc::new(idx.clone());
    let mut group = c.benchmark_group("type_index/full_hoi4_md");
    group.sample_size(FULL_SAMPLE_COUNT);
    group.bench_function("snapshot_arc", |b| {
        b.iter(|| black_box(Arc::clone(black_box(&shared))))
    });
    group.bench_function("first_write_cow", |b| {
        b.iter_batched(
            || (Arc::clone(&shared), Arc::clone(&shared)),
            |(mut live, snapshot)| {
                let _ = black_box(Arc::make_mut(&mut live));
                black_box((live, snapshot))
            },
            BatchSize::PerIteration,
        )
    });
    group.bench_function("clone", |b| b.iter(|| black_box(black_box(idx).clone())));
    group.bench_function("clone_complex_enum_values", |b| {
        b.iter(|| black_box(black_box(&idx.complex_enum_values).clone()))
    });
    group.bench_function("clone_value_set_values", |b| {
        b.iter(|| black_box(black_box(&idx.value_set_values).clone()))
    });
    group.finish();
}

fn index_contribution_under_root(idx: &TypeIndex, root: &Path) -> (usize, usize) {
    let mut files = std::collections::HashSet::new();
    let mut instances = 0;
    for entries in idx.map().values() {
        for (file, _) in entries {
            if Path::new(file.as_ref()).starts_with(root) {
                files.insert(file.as_ref());
                instances += 1;
            }
        }
    }
    (files.len(), instances)
}

fn bench_snapshot_clone(c: &mut Criterion) {
    bench_index(c, "synthetic", &synthetic_index());

    let Some(rules) = rules_dir() else {
        eprintln!(
            "snapshot_clone: no ruleset. Set CWTOOLS_RULES to a cwtools-hoi4-config/Config checkout"
        );
        return;
    };
    let Some(corpus) = checkout("CWTOOLS_CORPUS", CORPUS_NAME) else {
        eprintln!("snapshot_clone: no corpus. Set CWTOOLS_CORPUS to a {CORPUS_NAME} checkout");
        return;
    };
    {
        let table = StringTable::new();
        let (ruleset, rule_errors) = match load_rules(&RulesInput::Dir(rules.clone()), &table) {
            Ok(loaded) => loaded,
            Err(e) => {
                eprintln!("snapshot_clone: could not load rules: {e}");
                return;
            }
        };
        assert!(
            rule_errors.is_empty(),
            "snapshot_clone: rules problems: {rule_errors:?}"
        );
        let var_effects = variable_defining_effects(&ruleset);
        let idx = index_game_dir(&corpus, &ruleset, &table, &var_effects).unwrap();
        assert!(
            !idx.map().is_empty(),
            "snapshot_clone: {CORPUS_NAME} indexed to nothing; the clone would measure an empty map"
        );
        bench_index(c, "corpus", &idx);
    }

    let Some(vanilla) = std::env::var_os("CWTOOLS_VANILLA").map(std::path::PathBuf::from) else {
        eprintln!(
            "snapshot_clone/full_hoi4_md: skipped; set CWTOOLS_VANILLA to a HOI4 install containing common/events/localisation"
        );
        return;
    };
    if !vanilla.is_dir() {
        eprintln!(
            "snapshot_clone/full_hoi4_md: skipped; CWTOOLS_VANILLA is not a directory: {}",
            vanilla.display()
        );
        return;
    }
    for relative in ["common/events", "localisation"] {
        assert!(
            vanilla.join(relative).is_dir(),
            "snapshot_clone/full_hoi4_md: expected HOI4 content directory missing: {}",
            vanilla.join(relative).display()
        );
    }

    eprintln!(
        "snapshot_clone/full_hoi4_md: loading HOI4 Session; rules={} ({}) corpus={} ({}) vanilla={} ({})",
        rules.display(),
        git_revision(&rules),
        corpus.display(),
        git_revision(&corpus),
        vanilla.display(),
        git_revision(&vanilla),
    );
    report_memory("full_hoi4_md_before_session");
    let full_session = Session::load(SessionConfig {
        game: Game::Hoi4,
        rules: RulesInput::Dir(rules),
        directory: corpus,
        parent_roots: Vec::new(),
        vanilla: Some(vanilla.clone()),
        vanilla_cache: None,
        vanilla_cache_auto: None,
        ignore_files: &[],
        ignore_dirs: &[],
        loc_languages: None,
        case_sensitive_files: false,
        on_rules_diagnostic: None,
    });
    assert!(
        !full_session.discovery_failed,
        "snapshot_clone/full_hoi4_md: workspace discovery failed"
    );
    let session_index = full_session.type_index();
    assert!(
        !session_index.map().is_empty(),
        "snapshot_clone/full_hoi4_md: layered HOI4 + MD index is empty"
    );
    let (vanilla_indexed_files, vanilla_indexed_instances) =
        index_contribution_under_root(session_index, &vanilla);
    assert!(
        vanilla_indexed_files > 0 && vanilla_indexed_instances > 0,
        "snapshot_clone/full_hoi4_md: HOI4 content was present but contributed no indexed vanilla instances"
    );
    let full_instances: usize = session_index.map().values().map(Vec::len).sum();
    eprintln!(
        "snapshot_clone/full_hoi4_md: composition=HOI4 vanilla + Millennium Dawn primary; primary_script_and_loc_files={}, vanilla_indexed_files={}, vanilla_indexed_instances={}, types={}, instances={}, file_paths={}, complex_enum_values={}, value_set_values={}",
        full_session.parsed_files().len(),
        vanilla_indexed_files,
        vanilla_indexed_instances,
        session_index.map().len(),
        full_instances,
        session_index.file_index.len(),
        value_count(&session_index.complex_enum_values),
        value_count(&session_index.value_set_values),
    );
    let full_idx = session_index.clone();
    drop(full_session);
    report_memory("full_hoi4_md_before_write_baseline");
    let mut unique_live = Arc::new(full_idx.clone());
    let _ = Arc::make_mut(&mut unique_live);
    report_memory("full_hoi4_md_after_write_without_retained_snapshot");
    drop(unique_live);

    report_memory("full_hoi4_md_before_snapshot");
    let shared = Arc::new(full_idx.clone());
    let snapshot = Arc::clone(&shared);
    report_memory("full_hoi4_md_with_retained_snapshot_before_write");
    let mut live = Arc::clone(&shared);
    let _ = Arc::make_mut(&mut live);
    report_memory("full_hoi4_md_after_first_write_with_snapshot");
    drop((live, snapshot, shared));

    bench_full_index(c, &full_idx);
    eprintln!(
        "snapshot_clone/full_hoi4_md: Criterion sample count is {FULL_SAMPLE_COUNT}; raw samples are retained under target/criterion. RSS readings are whole-process observations whose changes depend on allocator history/order, not TypeIndex allocation or production peak accounting."
    );
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(30);
    targets = bench_snapshot_clone
}
criterion_main!(benches);
