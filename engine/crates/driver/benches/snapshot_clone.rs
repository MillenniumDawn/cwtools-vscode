//! Cost of the workspace-scan pass 2 index snapshot.
//!
//! Pass 2 clones the whole `TypeIndex` under a brief `info_service` read guard
//! (`crates/lsp/src/scan/workspace.rs`) so rayon holds no locks. The clone is
//! the entire lock hold, so its cost is the window a concurrent keystroke's
//! `write()` waits on.
//!
//! `type_index/clone_corpus` is that lock hold. The two part benches beside it
//! say where the time goes: the type maps proper, versus the dynamic-value
//! indexes (`complex_enum_values`, `value_set_values`) that #332 proposed to
//! keep out of the snapshot. Their share is what that change could save, and on
//! Millennium Dawn it is about 7% of a 41ms clone: the maps are the cost.
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

use std::collections::HashMap;
use std::hint::black_box;
use std::path::PathBuf;

use criterion::{Criterion, criterion_group, criterion_main};
use cwtools_driver::{RulesInput, index_game_dir, load_rules};
use cwtools_index::{
    SourceLocation, TypeIndex, TypeInstance, dynamic_values::NamedValueIndex,
    variable_defining_effects,
};
use cwtools_string_table::string_table::StringTable;

/// Millennium Dawn rather than Kaiserreich: it is the larger of the two pinned
/// corpora and the one whose flag-heavy scripts fill `value_set_values`.
const CORPUS_NAME: &str = "Millennium-Dawn";

/// Synthetic fallback, sized to land in the same order of magnitude as the real
/// corpus so the fallback number is not misleading about the ratio.
const SYNTH_FILES: usize = 7000;
const SYNTH_TYPES: &[&str] = &["state", "character", "technology", "country_event"];
const SYNTH_INSTANCES_PER_TYPE_PER_FILE: usize = 6;
const SYNTH_VALUE_SETS: &[&str] = &["country_flag", "global_flag", "state_flag"];
const SYNTH_VALUES_PER_SET_PER_FILE: usize = 4;

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
    let dir = projects_dir()?.join(CORPUS_NAME);
    dir.is_dir().then_some(dir)
}

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

fn bench_index(c: &mut Criterion, label: &str, idx: &TypeIndex) {
    let instances: usize = idx.map.values().map(Vec::len).sum();
    eprintln!(
        "snapshot_clone/{label}: {instances} instances, \
         {} complex-enum values, {} value-set values",
        value_count(&idx.complex_enum_values),
        value_count(&idx.value_set_values),
    );
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

fn bench_snapshot_clone(c: &mut Criterion) {
    bench_index(c, "synthetic", &synthetic_index());

    let Some(rules) = rules_dir() else {
        eprintln!(
            "snapshot_clone: no ruleset. Set CWTOOLS_RULES to a cwtools-hoi4-config/Config checkout"
        );
        return;
    };
    let Some(corpus) = corpus_dir() else {
        eprintln!("snapshot_clone: no corpus. Set CWTOOLS_CORPUS to a {CORPUS_NAME} checkout");
        return;
    };
    let table = StringTable::new();
    let (ruleset, rule_errors) = match load_rules(&RulesInput::Dir(rules), &table) {
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
    let idx = index_game_dir(&corpus, &ruleset, &table, &var_effects);
    assert!(
        !idx.map.is_empty(),
        "snapshot_clone: {CORPUS_NAME} indexed to nothing; the clone would measure an empty map"
    );
    bench_index(c, "corpus", &idx);
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(30);
    targets = bench_snapshot_clone
}
criterion_main!(benches);
