//! Directly measures InfoService's per-file collection passes. Parsing,
//! ruleset setup, type-instance collection, and workspace merge are excluded.

use std::hint::black_box;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use cwtools_index::{CollectedTypeInstances, variable_defining_effects};
use cwtools_info::InfoService;
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;

const RULES: &str = r#"
types = { type[foo] = { path = "game/common/scripted_guis" } }
foo = { my_ref = <scripted_effect> }
alias[effect:set_variable] = {
    var = value_set[variable]
    value = int_variable_field
}
enums = {
    complex_enum[root_scalar] = {
        path = "game/common/scripted_guis"
        start_from_root = yes
        name = { enum_name = scalar }
    }
}
"#;

const RECORDS: usize = 256;
const PATH: &str = "game/common/scripted_guis/large.gfx";
const URI: &str = "file:///game/common/scripted_guis/large.gfx";

fn large_file(records: usize) -> String {
    let mut source = String::with_capacity(records * 320);
    source.push_str("root_scalar = yes\nscripted_gui = {\n");
    for n in 0..records {
        source.push_str(&format!(
            "  gui_{n:04} = {{\n    effects = {{\n      Click_{n:04} = {{\n        set_variable = {{ var = value_{n:04} value = {n} }}\n        my_ref = effect_{n:04}\n        event_target:target_{n:04} = {{ scope = country }}\n      }}\n    }}\n    triggers = {{\n      Visible_{n:04} = {{ has_country_flag = flag_{n:04} }}\n    }}\n  }}\n"
        ));
    }
    source.push_str("}\n");
    source
}

fn bench_residual_collection(c: &mut Criterion) {
    let table = StringTable::new();
    let rules = ast_to_ruleset(&parse_string(RULES, &table), &table);
    let source = large_file(RECORDS);
    let bytes = source.len();
    let parsed = parse_string(&source, &table);
    let mut service = InfoService::new();
    service.update_ruleset_data(variable_defining_effects(&rules));
    let collector = service.file_collector(&rules);

    // Confirm collection is stable and retain a compact output signature for
    // comparing the baseline and candidate binaries.
    let mut check = InfoService::new();
    check.update_ruleset_data(variable_defining_effects(&rules));
    let check_collector = check.file_collector(&rules);
    let first = check_collector.collect(
        URI,
        &parsed,
        &table,
        &rules,
        PATH,
        CollectedTypeInstances::default(),
    );
    assert!(check.merge_collected_file(first, false));
    let signature = format!(
        "bytes={bytes} file_info={} type_refs={} variables={} complex_values={} scripted_guis={}",
        check.files[URI]
            .type_references
            .values()
            .map(Vec::len)
            .sum::<usize>(),
        check
            .reference_index
            .references("scripted_effect", "effect_0000")
            .len(),
        check.files[URI]
            .defined_variables_ns
            .values()
            .map(Vec::len)
            .sum::<usize>(),
        check
            .type_index
            .complex_enum_values
            .values("root_scalar")
            .count(),
        check.type_index.scripted_gui_index.contains("click_0000"),
    );
    eprintln!("RESIDUAL_WALK_SIGNATURE {signature}");

    // Make each measurement the collector call itself: no parse, type-index
    // walk, clear, merge, or driver/CLI/LSP validation in the timed closure.
    let mut group = c.benchmark_group("info_service/residual_collection");
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(10));
    group.bench_function(BenchmarkId::new("bytes", bytes), |b| {
        b.iter(|| {
            black_box(collector.collect(
                URI,
                black_box(&parsed),
                &table,
                &rules,
                PATH,
                black_box(CollectedTypeInstances::default()),
            ));
        })
    });
}

criterion_group!(benches, bench_residual_collection);
criterion_main!(benches);
