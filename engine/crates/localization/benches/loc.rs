use criterion::{Criterion, criterion_group, criterion_main};
use cwtools_localization::yaml_parser::parse_loc_text;
use std::hint::black_box;

// A localisation .yml with a language header and entries carrying plain text,
// $refs$, [commands], and nested formatting. Exercises parse_loc_text (#24) and
// loc-element scanning (#25).
const SAMPLE: &str = r#"l_english:
 KEY_one:0 "Simple value"
 KEY_two:1 "Value with a $REPLACE$ token and another $ONE$"
 KEY_three:0 "Command chain [ROOT.GetName] did [From.Owner.GetLeader]"
 KEY_four:0 "Mixed §Yformatting§! with #bold text#! and a $var$"
 KEY_five:0 "Plain line number five"
 KEY_six:2 "Nested [GetCountry('GER').GetNameDefiniteCap] reference"
 KEY_seven:0 "Trailing whitespace and tabs   "
 KEY_eight:0 "Another $TOKEN$ here, and [SomeScope.GetThing]"
 KEY_nine:0 "Line nine"
 KEY_ten:0 "Final entry with a quote \" inside"
"#;

fn bench_parse_loc(c: &mut Criterion) {
    c.bench_function("parse_loc_text/english_10", |b| {
        b.iter(|| parse_loc_text(black_box(SAMPLE), black_box("bench_l_english.yml")))
    });
}

criterion_group!(benches, bench_parse_loc);
criterion_main!(benches);
