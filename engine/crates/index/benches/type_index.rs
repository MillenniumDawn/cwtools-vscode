//! Realistic-scale `TypeIndex` reads: hundreds of files each contributing a
//! handful of instances to a few high-cardinality types (state, character,
//! technology), the shape that made `instances_in_file`/`remove_file` scan a
//! type's entire instance list per call (#129).

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use cwtools_index::{SourceLocation, TypeIndex, TypeInstance};
use std::collections::HashMap;
use std::hint::black_box;

const FILE_COUNT: usize = 400;
const TYPES: &[&str] = &["state", "character", "technology"];
const INSTANCES_PER_TYPE_PER_FILE: usize = 12;

fn file_uri(index: usize) -> String {
    format!("file:///mod/common/{index}.txt")
}

fn instance(type_name: &str, file_index: usize, n: usize) -> TypeInstance {
    TypeInstance {
        name: format!("{type_name}_{file_index}_{n}"),
        location: SourceLocation {
            line: n as u32,
            col: 0,
            end: (n as u32, 0),
        },
        primary_loc_key: None,
        required_loc_keys: Vec::new(),
    }
}

fn build_index() -> TypeIndex {
    let mut idx = TypeIndex::new();
    for file_index in 0..FILE_COUNT {
        let uri = file_uri(file_index);
        let per_type: HashMap<String, Vec<TypeInstance>> = TYPES
            .iter()
            .map(|&ty| {
                let instances = (0..INSTANCES_PER_TYPE_PER_FILE)
                    .map(|n| instance(ty, file_index, n))
                    .collect();
                (ty.to_string(), instances)
            })
            .collect();
        idx.merge(&uri, per_type);
    }
    idx
}

fn bench_instances_in_file(c: &mut Criterion) {
    let idx = build_index();
    let target = file_uri(FILE_COUNT / 2);
    c.bench_function("type_index/instances_in_file", |b| {
        b.iter(|| idx.instances_in_file(black_box(&target)))
    });
}

fn bench_remove_file(c: &mut Criterion) {
    let target = file_uri(FILE_COUNT / 2);
    c.bench_function("type_index/remove_file", |b| {
        b.iter_batched(
            build_index,
            // Return `idx` instead of dropping it here: `iter_batched` times
            // the whole routine call, and dropping a many-thousand-entry
            // `TypeIndex` inline would swamp `remove_file`'s own cost. Handing
            // it back lets criterion drop it after `end` is recorded.
            |mut idx| {
                idx.remove_file(black_box(&target));
                idx
            },
            BatchSize::LargeInput,
        )
    });
}

fn build_primary_loc_index(instance_count: usize) -> TypeIndex {
    let instances = (0..instance_count)
        .map(|i| {
            let target = i + 1 == instance_count;
            TypeInstance {
                name: if target {
                    "target_instance".to_string()
                } else {
                    format!("unrelated_instance_{i}")
                },
                location: SourceLocation {
                    line: i as u32,
                    col: 0,
                    end: (i as u32, 0),
                },
                primary_loc_key: target.then(|| "target_title".to_string()),
                required_loc_keys: Vec::new(),
            }
        })
        .collect();
    let mut idx = TypeIndex::new();
    idx.merge(
        "file:///mod/common/events.txt",
        HashMap::from([("event".to_string(), instances)]),
    );
    idx
}

fn bench_primary_loc_key_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("type_index/primary_loc_key_scaling");
    for count in [100, 10_000, 100_000] {
        let idx = build_primary_loc_index(count);
        group.bench_function(format!("indexed_instances_{count}"), |b| {
            b.iter(|| idx.primary_loc_key(black_box("event"), black_box("TARGET_INSTANCE")))
        });
        group.bench_function(format!("scan_instances_{count}"), |b| {
            b.iter(|| {
                idx.map.get("event").and_then(|entries| {
                    entries.iter().find_map(|(_, instance)| {
                        (instance
                            .name
                            .eq_ignore_ascii_case(black_box("TARGET_INSTANCE")))
                        .then_some(instance.primary_loc_key.as_deref())
                        .flatten()
                    })
                })
            })
        });
    }
    group.finish();
}

fn bench_instance_locations_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("type_index/instance_locations_scaling");
    for count in [100, 10_000, 100_000] {
        let idx = build_primary_loc_index(count);
        group.bench_function(format!("indexed_instances_{count}"), |b| {
            b.iter(|| idx.instance_locations(black_box("TARGET_INSTANCE")))
        });
        group.bench_function(format!("scan_instances_{count}"), |b| {
            b.iter(|| {
                idx.map
                    .values()
                    .flatten()
                    .filter(|(_, instance)| {
                        instance
                            .name
                            .eq_ignore_ascii_case(black_box("TARGET_INSTANCE"))
                    })
                    .map(|(uri, instance)| (std::sync::Arc::clone(uri), instance.location))
                    .collect::<Vec<_>>()
            })
        });
    }
    group.finish();
}

fn bench_primary_loc_index_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("type_index/primary_loc_index_build");
    for count in [10_000, 100_000] {
        group.bench_function(format!("instances_{count}"), |b| {
            b.iter(|| black_box(build_primary_loc_index(black_box(count))))
        });
        let index = build_primary_loc_index(count);
        eprintln!(
            "name_index_capacity_estimate_bytes[{count}]: {}",
            index.approximate_instance_name_index_storage_bytes()
        );
    }
    eprintln!(
        "type_index_size_bytes: {} (the per-name hash-index storage is additional)",
        std::mem::size_of::<TypeIndex>()
    );
    group.finish();
}

fn bench_primary_loc_index_clone(c: &mut Criterion) {
    let index = build_primary_loc_index(100_000);
    c.bench_function("type_index/primary_loc_index_clone/instances_100000", |b| {
        b.iter(|| black_box(index.clone()))
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(30);
    targets = bench_instances_in_file,
        bench_remove_file,
        bench_primary_loc_key_scaling,
        bench_instance_locations_scaling,
        bench_primary_loc_index_build,
        bench_primary_loc_index_clone
}
criterion_main!(benches);
