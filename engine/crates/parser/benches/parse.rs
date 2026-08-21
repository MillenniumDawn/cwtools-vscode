use criterion::{Criterion, criterion_group, criterion_main};
use cwtools_parser::parser::parse_string;
use cwtools_string_table::string_table::StringTable;
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

criterion_group!(benches, bench_parse);
criterion_main!(benches);
