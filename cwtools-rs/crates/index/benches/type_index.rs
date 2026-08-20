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

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(30);
    targets = bench_instances_in_file, bench_remove_file
}
criterion_main!(benches);
