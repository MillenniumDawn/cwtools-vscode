use criterion::{Criterion, criterion_group, criterion_main};
use cwtools_parser::ast::{SourcePos, SourceRange};
use cwtools_parser::fix::{EOF_POS, SpanEdit, apply_edits, plan_file_edits};
use cwtools_parser::format::{FormatOptions, format_edits_from_formatted_text, format_text};
use cwtools_parser::parser::parse_string;
use cwtools_string_table::string_table::StringTable;
use std::fmt::Write as _;
use std::hint::black_box;
use std::time::{Duration, Instant};

// Representative Paradox script: nested clauses, quoted strings, numbers,
// bool keywords, comments, @-variables. Exercises the parser/interner hot path.
const SAMPLE: &str = r#"
@cost = 10
focus_tree = {
    id = test_tree
    country = { factor = 1 }
    # a comment line
    focus = {
        id = test_focus
        x = 1
        y = 1
        cost = @cost
        text = "Quoted focus name"
        available = { has_country_flag = some_flag }
        completion_reward = {
            add_political_power = 100
            hidden_effect = { set_country_flag = done }
        }
        ai_will_do = { factor = 1.5 }
    }
    focus = {
        id = second_focus
        prerequisite = { focus = test_focus }
        relative_position_id = test_focus
        x = 2
        y = 1
        cost = 7
        available = { yes }
    }
}
"#;

fn bench_parse(c: &mut Criterion) {
    let table = StringTable::new();
    c.bench_function("parse_string/focus_tree", |b| {
        b.iter(|| parse_string(black_box(SAMPLE), black_box(&table)))
    });
}

fn old_workspace_shape(input: &str, table: &StringTable, options: &FormatOptions) -> Vec<SpanEdit> {
    let Some(formatted) = format_text(input, table, options) else {
        return Vec::new();
    };
    if formatted == input {
        return Vec::new();
    }
    let Some(formatted) = format_text(input, table, options) else {
        return Vec::new();
    };
    if formatted == input {
        return Vec::new();
    }
    let (edits, _) = plan_file_edits(
        input,
        vec![(
            (),
            SpanEdit {
                range: SourceRange {
                    start: SourcePos { line: 1, col: 0 },
                    end: EOF_POS,
                },
                replacement: formatted,
            },
        )],
    );
    edits
}

fn reused_workspace_shape(
    input: &str,
    table: &StringTable,
    options: &FormatOptions,
) -> Vec<SpanEdit> {
    format_text(input, table, options)
        .map(|formatted| format_edits_from_formatted_text(input, formatted))
        .unwrap_or_default()
}

fn changed_fixture() -> String {
    let mut input = String::with_capacity(16_384);
    input.push_str("root={\n");
    for index in 0..128 {
        writeln!(input, " item_{index}={{").expect("write fixture");
        writeln!(input, "  value={index}").expect("write fixture");
        input.push_str("  inner={\n   enabled=yes\n  }\n }\n");
    }
    input.push_str("}\n");
    input
}

fn time_call(call: &mut impl FnMut()) -> Duration {
    let start = Instant::now();
    call();
    start.elapsed()
}

fn interleaved_duration(
    iterations: u64,
    measure_old: bool,
    old: &mut impl FnMut(),
    reused: &mut impl FnMut(),
) -> Duration {
    let mut elapsed = Duration::ZERO;
    for iteration in 0..iterations {
        let old_first = iteration % 2 == 0;
        if measure_old {
            if old_first {
                elapsed += time_call(old);
                reused();
            } else {
                reused();
                elapsed += time_call(old);
            }
        } else if old_first {
            old();
            elapsed += time_call(reused);
        } else {
            elapsed += time_call(reused);
            old();
        }
    }
    elapsed
}

fn bench_format_workspace(c: &mut Criterion) {
    let input = changed_fixture();
    let table = StringTable::new();
    let options = FormatOptions::default();
    let expected = format_text(&input, &table, &options).expect("fixture parses");
    let old_edits = old_workspace_shape(&input, &table, &options);
    let reused_edits = reused_workspace_shape(&input, &table, &options);
    assert!(!reused_edits.is_empty(), "fixture must need formatting");
    assert_eq!(old_edits, reused_edits, "workspace paths must agree");
    assert_eq!(apply_edits(&input, &old_edits), expected);

    let mut group = c.benchmark_group("format_workspace");
    group.bench_function("old_reformat_and_plan", |b| {
        b.iter_custom(|iterations| {
            let mut old = || {
                black_box(old_workspace_shape(
                    black_box(&input),
                    black_box(&table),
                    black_box(&options),
                ));
            };
            let mut reused = || {
                black_box(reused_workspace_shape(
                    black_box(&input),
                    black_box(&table),
                    black_box(&options),
                ));
            };
            interleaved_duration(iterations, true, &mut old, &mut reused)
        })
    });
    group.bench_function("reuse_formatted_output", |b| {
        b.iter_custom(|iterations| {
            let mut old = || {
                black_box(old_workspace_shape(
                    black_box(&input),
                    black_box(&table),
                    black_box(&options),
                ));
            };
            let mut reused = || {
                black_box(reused_workspace_shape(
                    black_box(&input),
                    black_box(&table),
                    black_box(&options),
                ));
            };
            interleaved_duration(iterations, false, &mut old, &mut reused)
        })
    });
    group.finish();
}

criterion_group!(benches, bench_parse, bench_format_workspace);
criterion_main!(benches);
