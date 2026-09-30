use criterion::{Criterion, criterion_group, criterion_main};
use cwtools_parser::format::{FormatOptions, format_edits};
use cwtools_parser::parser::parse_string;
use cwtools_string_table::string_table::StringTable;
use std::fmt::Write as _;
use std::hint::black_box;

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

fn bench_format_edits(c: &mut Criterion) {
    let input = changed_fixture();
    let table = StringTable::new();
    let options = FormatOptions::default();
    assert!(
        !format_edits(&input, &table, &options).is_empty(),
        "fixture must need formatting"
    );
    c.bench_function("format_edits/changed_file", |b| {
        b.iter(|| format_edits(black_box(&input), black_box(&table), black_box(&options)))
    });
}

criterion_group!(benches, bench_parse, bench_format_edits);
criterion_main!(benches);
