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
use cwtools_test_support::checkout;
use std::hint::black_box;
use std::io::Write;
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
    if std::env::var_os("CWTOOLS_ARCHIVE_BENCH_ONLY").is_some() {
        return;
    }
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
    if std::env::var_os("CWTOOLS_ARCHIVE_BENCH_ONLY").is_some() {
        return;
    }
    if !workspace::PATH_METADATA_CACHE_SUPPORTED {
        eprintln!("parse_cache: path-keyed cache lookups are unsupported on this platform");
        return;
    }
    let Some(root) = checkout("CWTOOLS_CORPUS", "Millennium-Dawn") else {
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
                let source = workspace::SourceMeta::new(&text, Default::default());
                workspace::store_path(
                    cache.path(),
                    fingerprint,
                    path,
                    &source_key,
                    &parsed,
                    &source,
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

fn collect_cwb(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_cwb(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "cwb") {
            out.push(path);
        }
    }
}

fn prepare_archive_fixtures(root: &Path) {
    assert!(
        !root.exists(),
        "refusing to overwrite archive fixture directory {}",
        root.display()
    );
    let Some(corpus) = checkout("CWTOOLS_CORPUS", "Millennium-Dawn") else {
        panic!("set CWTOOLS_CORPUS to the pinned Millennium-Dawn checkout");
    };
    let mut files = Vec::new();
    for sub in ["common", "events", "history"] {
        collect_txt(&corpus.join(sub), &mut files);
    }
    eprintln!("archive fixture setup: {} corpus .txt files", files.len());

    let native_cache = root.join("generated-cache");
    std::fs::create_dir_all(&native_cache).expect("create generated cache directory");
    let table = StringTable::new();
    let fingerprint = 0xabc;
    workspace::validate_or_clear(&native_cache, fingerprint).expect("fresh generated cache");
    let mut stored = 0usize;
    for path in &files {
        let Some(key) = workspace::source_cache_key(path) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let parsed = parse_string(&text, &table);
        let source = workspace::SourceMeta::new(&text, Default::default());
        workspace::store_path(
            &native_cache,
            fingerprint,
            path,
            &key,
            &parsed,
            &source,
            &table,
        );
        stored += 1;
    }

    let mut native_paths = Vec::new();
    collect_cwb(&native_cache, &mut native_paths);
    assert_eq!(
        native_paths.len(),
        stored,
        "every corpus file should yield a cache"
    );
    let native_dir = root.join("undeclared");
    let declared_dir = root.join("declared");
    std::fs::create_dir_all(&native_dir).expect("create undeclared fixture directory");
    std::fs::create_dir_all(&declared_dir).expect("create declared fixture directory");
    let mut native_bytes = 0u64;
    let mut declared_bytes = 0u64;
    let mut decoded_bytes = 0u64;
    for (index, path) in native_paths.iter().enumerate() {
        let archive = std::fs::read(path).expect("read generated native archive");
        assert!(archive.len() >= 5, "cache archive header must be present");
        let (header, native_frame) = archive.split_at(5);
        assert_eq!(
            zstd::zstd_safe::get_frame_content_size(native_frame).unwrap(),
            None,
            "native streaming cache frame must have unknown content size"
        );
        let decoded = zstd::stream::decode_all(native_frame).expect("decode native archive");
        let mut encoder = zstd::stream::Encoder::new(Vec::new(), io::ZSTD_LEVEL)
            .expect("create declared-size zstd encoder");
        encoder
            .include_checksum(true)
            .expect("enable frame checksum");
        encoder
            .set_pledged_src_size(Some(decoded.len() as u64))
            .expect("pledge decoded archive size");
        encoder
            .write_all(&decoded)
            .expect("compress declared-size archive");
        let declared_frame = encoder.finish().expect("finish declared-size frame");
        assert_eq!(
            zstd::zstd_safe::get_frame_content_size(&declared_frame).unwrap(),
            Some(decoded.len() as u64),
            "declared fixture must advertise its decoded size"
        );
        let filename = format!("{index:05}.cwb");
        let native_fixture = native_dir.join(&filename);
        let declared_fixture = declared_dir.join(&filename);
        std::fs::write(&native_fixture, &archive).expect("write native fixture");
        let mut declared_archive = header.to_vec();
        declared_archive.extend_from_slice(&declared_frame);
        std::fs::write(&declared_fixture, &declared_archive).expect("write declared fixture");
        native_bytes += archive.len() as u64;
        declared_bytes += declared_archive.len() as u64;
        decoded_bytes += decoded.len() as u64;
    }
    eprintln!(
        "archive fixtures ready: {stored} files; decoded {decoded_bytes} bytes; native {native_bytes} bytes; declared {declared_bytes} bytes"
    );
}

fn bench_archive_decode(c: &mut Criterion) {
    if let Some(root) = std::env::var_os("CWTOOLS_ARCHIVE_BENCH_SETUP") {
        prepare_archive_fixtures(Path::new(&root));
        std::process::exit(0);
    }
    if std::env::var_os("CWTOOLS_ARCHIVE_BENCH_ONLY").is_none() {
        return;
    }
    let root = std::env::var_os("CWTOOLS_ARCHIVE_BENCH_FIXTURES")
        .map(PathBuf::from)
        .expect("set CWTOOLS_ARCHIVE_BENCH_FIXTURES to prepared fixtures");
    let native_dir = root.join("undeclared");
    let declared_dir = root.join("declared");
    let mut native_paths = Vec::new();
    let mut declared_paths = Vec::new();
    collect_cwb(&native_dir, &mut native_paths);
    collect_cwb(&declared_dir, &mut declared_paths);
    assert!(!native_paths.is_empty(), "fixture corpus must not be empty");
    assert_eq!(native_paths.len(), declared_paths.len());
    for path in &native_paths {
        let bytes = std::fs::read(path).expect("read native fixture");
        assert_eq!(
            zstd::zstd_safe::get_frame_content_size(&bytes[5..]).unwrap(),
            None
        );
    }
    for path in &declared_paths {
        let bytes = std::fs::read(path).expect("read declared fixture");
        assert!(
            zstd::zstd_safe::get_frame_content_size(&bytes[5..])
                .unwrap()
                .is_some(),
            "declared fixture must select declared-size decode branch"
        );
    }

    let validate = |paths: &[PathBuf]| {
        for path in paths {
            io::with_archived_file(path, |archived| black_box(archived.root_children.len()))
                .expect("fixture must decode and validate as a cache archive");
        }
    };
    validate(&native_paths);
    validate(&declared_paths);

    let mut group = c.benchmark_group("archive_decode");
    group.sample_size(10);
    group.bench_function("undeclared_warm_load", |b| {
        b.iter(|| validate(&native_paths))
    });
    group.bench_function("declared_warm_load", |b| {
        b.iter(|| validate(&declared_paths))
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_cache,
    bench_parse_cache,
    bench_archive_decode
);
criterion_main!(benches);
