//! Cache encode, write and read on one small script, and the parse cache over a
//! whole mod.
//!
//! `parse_cache` stores and then reloads every `.txt` under `common`, `events`
//! and `history` of a Millennium-Dawn checkout, the shape of a workspace scan.
//! `cold_parse_store` parses each file and writes its entry (over the previous
//! iteration's), and `warm_load` reads each entry back by path. It needs a
//! checkout:
//!
//!   CWTOOLS_CORPUS=/path/to/Millennium-Dawn cargo bench -p cwtools_cache
//!
//! It is also found under `CWTOOLS_PROJECTS`, or as a sibling of this repo.
//! When it is missing the case prints why and measures nothing; the `cache`
//! group always runs.

use criterion::{Criterion, criterion_group, criterion_main};
use cwtools_cache::{convert, io, workspace};
use cwtools_parser::parser::parse_string;
use cwtools_string_table::string_table::StringTable;
use std::hint::black_box;
use std::path::{Path, PathBuf};

const SAMPLE: &str = r#"
focus_tree = {
    id = cache_bench_tree
    focus = {
        id = first_focus
        text = "Representative cached focus"
        x = 1
        y = 2
        available = { has_country_flag = first_flag }
        completion_reward = { add_political_power = 100 }
    }
    focus = {
        id = second_focus
        prerequisite = { focus = first_focus }
        text = "Another focus"
        cost = 7
        ai_will_do = { factor = 1.5 }
    }
}
"#;

fn bench_cache(c: &mut Criterion) {
    let table = StringTable::new();
    let parsed = parse_string(SAMPLE, &table);
    let cached = convert::arena_to_cached(&parsed.arena, &parsed.root_children, &table);
    let directory = tempfile::tempdir().expect("cache benchmark temp directory");
    let path = directory.path().join("sample.cwb");
    io::serialize_to_file(&cached, &path).expect("seed cache benchmark archive");

    let mut group = c.benchmark_group("cache");
    group.bench_function("arena_to_cached", |b| {
        b.iter(|| {
            convert::arena_to_cached(
                black_box(&parsed.arena),
                black_box(&parsed.root_children),
                &table,
            )
        })
    });
    group.bench_function("serialize", |b| {
        b.iter(|| io::serialize_to_file(black_box(&cached), black_box(&path)).unwrap())
    });
    group.bench_function("read_convert", |b| {
        b.iter(|| {
            let table = StringTable::new();
            io::with_archived_file(black_box(&path), |archived| {
                convert::archived_to_arena(archived, &table).unwrap()
            })
            .unwrap()
        })
    });
    group.finish();
}

fn corpus_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CWTOOLS_CORPUS") {
        return Some(PathBuf::from(dir));
    }
    let projects = match std::env::var("CWTOOLS_PROJECTS") {
        Ok(dir) => PathBuf::from(dir),
        // crates/cache -> repo root is ../../.., siblings sit next to it.
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .ok()?,
    };
    let dir = projects.join("Millennium-Dawn");
    dir.is_dir().then_some(dir)
}

fn collect_txt(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_txt(&p, out);
        } else if p.extension().is_some_and(|e| e == "txt") {
            out.push(p);
        }
    }
}

fn bench_parse_cache(c: &mut Criterion) {
    if !workspace::PATH_METADATA_CACHE_SUPPORTED {
        eprintln!("parse_cache: path-keyed cache lookups are unsupported on this platform");
        return;
    }
    let Some(root) = corpus_dir() else {
        eprintln!(
            "parse_cache: no corpus. Set CWTOOLS_CORPUS to a Millennium-Dawn checkout (or \
             CWTOOLS_PROJECTS to the folder holding it)"
        );
        return;
    };
    let mut files = Vec::new();
    for sub in ["common", "events", "history"] {
        collect_txt(&root.join(sub), &mut files);
    }
    eprintln!(
        "parse_cache: {} .txt files under {}",
        files.len(),
        root.display()
    );

    let table = StringTable::new();
    let cache = tempfile::tempdir().expect("parse cache benchmark temp directory");
    let fingerprint = 0xabc;
    workspace::validate_or_clear(cache.path(), fingerprint).expect("fresh parse cache");

    let parse_and_store = || {
        let mut stored = 0usize;
        for path in &files {
            let Some(source_key) = workspace::source_cache_key(path) else {
                continue;
            };
            if let Ok(text) = std::fs::read_to_string(path) {
                let parsed = parse_string(&text, &table);
                workspace::store_path(
                    cache.path(),
                    fingerprint,
                    path,
                    &source_key,
                    &parsed,
                    &table,
                );
                stored += 1;
            }
        }
        stored
    };
    let load_all = || {
        files
            .iter()
            .filter(|path| workspace::load_path(cache.path(), fingerprint, path, &table).is_some())
            .count()
    };

    let stored = parse_and_store();
    assert_eq!(load_all(), stored, "every stored file should hit when warm");

    let mut group = c.benchmark_group("parse_cache");
    group.sample_size(10);
    group.bench_function("cold_parse_store", |b| b.iter(&parse_and_store));
    group.bench_function("warm_load", |b| b.iter(&load_all));
    group.finish();
}

criterion_group!(benches, bench_cache, bench_parse_cache);
criterion_main!(benches);
