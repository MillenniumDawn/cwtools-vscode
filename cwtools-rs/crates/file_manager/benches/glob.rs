use criterion::{Criterion, criterion_group, criterion_main};
use cwtools_file_manager::file_manager::glob_match;
use std::hint::black_box;

// Patterns with embedded wildcards bypass the *.ext / prefix* fast paths and
// hit the general greedy matcher. Mix of matching and non-matching, realistic
// lengths.
const CASES: &[(&str, &str)] = &[
    (
        "common/*/scripted_effects/*.txt",
        "common/ai_strategy/scripted_effects/foo.txt",
    ),
    ("events/*_events.txt", "events/germany_events.txt"),
    ("*/ideas/*_ideas.txt", "common/ideas/usa_ideas.txt"),
    ("gfx/**/*.dds", "gfx/interface/goals/focus_generic.dds"),
    (
        "history/countries/*-*.txt",
        "history/countries/GER-Germany.txt",
    ),
    ("common/*/*/*.txt", "common/units/equipment/infantry.txt"),
    ("*no*match*here*", "completely/unrelated/path/file.yml"),
];

fn bench_glob(c: &mut Criterion) {
    c.bench_function("glob_match/general", |b| {
        b.iter(|| {
            for (pat, text) in CASES {
                black_box(glob_match(black_box(pat), black_box(text)));
            }
        })
    });
}

criterion_group!(benches, bench_glob);
criterion_main!(benches);
