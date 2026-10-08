use criterion::{Criterion, criterion_group, criterion_main};
use cwtools_cache::{convert, io};
use cwtools_parser::parser::parse_string;
use cwtools_string_table::string_table::StringTable;
use std::hint::black_box;

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

criterion_group!(benches, bench_cache);
criterion_main!(benches);
