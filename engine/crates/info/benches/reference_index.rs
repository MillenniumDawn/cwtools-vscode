//! `ReferenceIndex` at workspace scale: one type bucket holding every use site
//! of a high-traffic type (events, ideas, scripted effects), the shape that made
//! `remove_file` and the name lookups scan the whole bucket on every edit (#866).
//!
//! The bucket grows by adding files and distinct names, never by repeating one
//! name: each name has `SITES_PER_NAME` sites in as many files and each file has
//! `SITES_PER_FILE` sites, so work proportional to the file's own sites or to a
//! name's hits stays flat across `SIZES`, while work proportional to the bucket
//! grows 100x. Every fourth name has uppercase spellings on half its sites, so
//! the exact and case-insensitive lookups differ and `merge` sees mixed case.
//! The largest size matches the 190k type instances of `perf_loc_ref_names`.
//!
//! `reference_index/*` drive the index directly (see `bench_support`);
//! `info_service/clear_file` is the same removal as the editor reaches it.

use std::collections::HashSet;
use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use cwtools_info::bench_support::{self, Refs};
use cwtools_info::{InfoService, ReferenceIndex};
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;

const SIZES: [usize; 3] = [1_900, 19_000, 190_000];
const SITES_PER_FILE: usize = 100;
const SITES_PER_NAME: usize = 4;
const TYPE_NAME: &str = "event";
const RULES: &str = "types = { type[decision] = { path = \"common/decisions\" } }\
                     decision = { my_ref = <event> }";

fn file_uri(file: usize) -> String {
    format!("file:///mod/common/decisions/{file}.txt")
}

fn site_name(site: usize, pool: usize) -> String {
    let n = site % pool;
    if (site / pool) % 2 == 1 && n.is_multiple_of(4) {
        format!("REF_{n:06}")
    } else {
        format!("ref_{n:06}")
    }
}

fn file_names(file: usize, size: usize) -> Vec<String> {
    let pool = size / SITES_PER_NAME;
    (file * SITES_PER_FILE..(file + 1) * SITES_PER_FILE)
        .map(|site| site_name(site, pool))
        .collect()
}

fn file_count(size: usize) -> usize {
    size / SITES_PER_FILE
}

fn target_file(size: usize) -> usize {
    file_count(size) / 2
}

fn files(size: usize) -> Vec<(String, Refs)> {
    (0..file_count(size))
        .map(|file| (file_uri(file), Refs::new(TYPE_NAME, file_names(file, size))))
        .collect()
}

fn built(size: usize) -> (ReferenceIndex, Vec<(String, Refs)>) {
    let files = files(size);
    let mut index = ReferenceIndex::default();
    for (uri, refs) in &files {
        bench_support::merge(&mut index, uri, refs.clone());
    }
    (index, files)
}

/// A name whose four sites split two lowercase, two uppercase.
fn query_name(size: usize) -> String {
    let pool = size / SITES_PER_NAME;
    format!("ref_{:06}", pool / 2 / 4 * 4)
}

fn bench_merge(c: &mut Criterion) {
    let mut group = c.benchmark_group("reference_index/merge");
    group.sample_size(10);
    for size in SIZES {
        let files = files(size);
        group.bench_with_input(BenchmarkId::from_parameter(size), &files, |b, files| {
            b.iter_batched(
                || files.clone(),
                // Returning the index lets criterion drop it after the timing.
                |files| {
                    let mut index = ReferenceIndex::default();
                    for (uri, refs) in files {
                        bench_support::merge(&mut index, &uri, refs);
                    }
                    index
                },
                BatchSize::LargeInput,
            )
        });
    }
    group.finish();
}

fn bench_remove_file(c: &mut Criterion) {
    let mut group = c.benchmark_group("reference_index/remove_file");
    for size in SIZES {
        let (mut index, files) = built(size);
        let (uri, refs) = &files[target_file(size)];
        group.bench_function(BenchmarkId::from_parameter(size), |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let start = Instant::now();
                    bench_support::remove_file(&mut index, black_box(uri));
                    total += start.elapsed();
                    bench_support::merge(&mut index, uri, refs.clone());
                }
                total
            })
        });
    }
    group.finish();
}

fn bench_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("reference_index/lookup");
    let excluded = HashSet::new();
    for size in SIZES {
        let (index, _files) = built(size);
        let exact = query_name(size);
        let ci = exact.to_ascii_uppercase();
        assert_eq!(index.references(TYPE_NAME, &exact).len(), 2);
        assert_eq!(index.references_ci(TYPE_NAME, &ci).len(), 4);

        group.bench_function(BenchmarkId::new("exact", size), |b| {
            b.iter(|| index.references(black_box(TYPE_NAME), black_box(&exact)))
        });
        group.bench_function(BenchmarkId::new("ci", size), |b| {
            b.iter(|| index.references_ci(black_box(TYPE_NAME), black_box(&ci)))
        });
        group.bench_function(BenchmarkId::new("exact_bounded", size), |b| {
            b.iter(|| {
                index.references_bounded(black_box(TYPE_NAME), black_box(&exact), 3, &excluded)
            })
        });
        group.bench_function(BenchmarkId::new("ci_bounded", size), |b| {
            b.iter(|| {
                index.references_ci_bounded(black_box(TYPE_NAME), black_box(&ci), 3, &excluded)
            })
        });
    }
    group.finish();
}

fn bench_clear_file(c: &mut Criterion) {
    let mut group = c.benchmark_group("info_service/clear_file");
    group.sample_size(20);
    let table = StringTable::new();
    let rules = ast_to_ruleset(&parse_string(RULES, &table), &table);
    for size in SIZES {
        let mut svc = InfoService::new();
        let target = target_file(size);
        let mut target_ast = None;
        for file in 0..file_count(size) {
            let body: String = file_names(file, size)
                .iter()
                .map(|name| format!("    my_ref = {name}\n"))
                .collect();
            let ast = parse_string(&format!("test = {{\n{body}}}\n"), &table);
            svc.index_file_with_path(&file_uri(file), &ast, &table, &rules, &logical_path(file));
            if file == target {
                target_ast = Some(ast);
            }
        }
        let target_ast = target_ast.expect("target file is in range");
        let (uri, path) = (file_uri(target), logical_path(target));
        assert_eq!(
            svc.reference_index
                .references(TYPE_NAME, &query_name(size))
                .len(),
            2
        );

        group.bench_function(BenchmarkId::from_parameter(size), |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let start = Instant::now();
                    svc.clear_file(black_box(&uri));
                    total += start.elapsed();
                    svc.index_file_with_path(&uri, &target_ast, &table, &rules, &path);
                }
                total
            })
        });
    }
    group.finish();
}

fn logical_path(file: usize) -> String {
    format!("common/decisions/{file}.txt")
}

criterion_group!(
    benches,
    bench_merge,
    bench_remove_file,
    bench_lookup,
    bench_clear_file
);
criterion_main!(benches);
