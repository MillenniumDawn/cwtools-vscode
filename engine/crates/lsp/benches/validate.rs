//! The per-edit work behind diagnostics: what a keystroke pays in a script
//! buffer or a localisation buffer, and what indexing a parsed file costs.
//!
//! `keystroke_validate`, `loc_edit_parse` and `loc_ref_names` run on fixtures
//! and need no checkout. `loc_edit_parse` and `loc_ref_names` keep a `before`
//! and `after` pair for the change (#87) that made them cheaper.
//!
//! `index_parsed_file` indexes three large Millennium-Dawn files (a focus
//! tree, an events file and an equipment file) against the real hoi4 rules:
//!
//!   CWTOOLS_CORPUS=/path/to/Millennium-Dawn \
//!   CWTOOLS_RULES=/path/to/cwtools-hoi4-config/Config \
//!     cargo bench -p cwtools_lsp --bench validate
//!
//! Both are also found under `CWTOOLS_PROJECTS`, or as siblings of this repo.
//! When either is missing that group prints why and measures nothing.

use std::collections::{HashMap, HashSet};
use std::hint::black_box;
use std::path::PathBuf;

use criterion::{Criterion, criterion_group, criterion_main};
use cwtools_lsp::bench_support::{
    collect_doc_tokens, loc_extra_valid_refs, logical_path_from_uri, parse_loc_buffer,
    workspace_prefix_of,
};
use cwtools_parser::parser::parse_string;
use cwtools_string_table::string_table::StringTable;

const KEYSTROKE_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../testfiles/performancetest2/events/cc_colony_events.txt"
);

/// The same three kinds of file this once ran on from a base-game install: a
/// focus tree, an events file and a plane equipment file.
const CORPUS_FILES: [(&str, &str); 3] = [
    ("focus_tree", "common/national_focus/05_iran.txt"),
    ("events", "events/France.txt"),
    ("equipment", "common/units/equipment/MD_plane_airframes.txt"),
];

/// A checkout named by `var`, else found under `CWTOOLS_PROJECTS`, else beside
/// this repo.
fn checkout(var: &str, name: &str) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var(var) {
        return Some(PathBuf::from(dir));
    }
    let projects = match std::env::var("CWTOOLS_PROJECTS") {
        Ok(dir) => PathBuf::from(dir),
        // crates/lsp -> repo root is ../../.., siblings sit next to it.
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .ok()?,
    };
    let dir = projects.join(name);
    dir.is_dir().then_some(dir)
}

fn bench_keystroke_validate(c: &mut Criterion) {
    let text = std::fs::read_to_string(KEYSTROKE_FIXTURE)
        .expect("keystroke fixture")
        .repeat(8);
    eprintln!("keystroke_validate: {} bytes", text.len());
    let table = StringTable::new();
    let parsed = parse_string(&text, &table);
    let workspace = Some(workspace_prefix_of("file:///mnt/mods/millennium_dawn"));
    let uri = "file:///mnt/mods/millennium_dawn/events/some_event_file.txt";

    let mut group = c.benchmark_group("keystroke_validate");
    group.bench_function("collect_doc_tokens", |b| {
        b.iter(|| collect_doc_tokens(black_box(&parsed), &table).len())
    });
    group.bench_function("logical_path_from_uri_x1000", |b| {
        b.iter(|| {
            (0..1000)
                .map(|_| logical_path_from_uri(black_box(uri), &workspace).len())
                .sum::<usize>()
        })
    });
    group.finish();
}

fn bench_loc_edit_parse(c: &mut Criterion) {
    const ENTRIES: usize = 4_000;
    let path = "localisation/bench_l_english.yml";
    let mut text = String::from("\u{FEFF}l_english:\n");
    for i in 0..ENTRIES {
        text.push_str(&format!(
            " key_{i:05}:0 \"Localised text for $other_key_{i:05}$ with [GetName] in it\"\n"
        ));
    }
    eprintln!("loc_edit_parse: {} bytes, {ENTRIES} entries", text.len());

    let mut group = c.benchmark_group("loc_edit_parse");
    group.bench_function("after_x1", |b| {
        b.iter(|| parse_loc_buffer(black_box(&text), path).len())
    });
    group.bench_function("before_x3_with_2_copies", |b| {
        b.iter(|| {
            let owned = text.to_string();
            let first = parse_loc_buffer(&owned, path);
            let second = parse_loc_buffer(black_box(&text), path);
            let owned = text.to_string();
            let third = parse_loc_buffer(&owned, path);
            first.len() + second.len() + third.len()
        })
    });
    group.finish();
}

fn bench_loc_ref_names(c: &mut Criterion) {
    const MODIFIER_KEYS: usize = 50_000;
    const TYPE_INSTANCES: usize = 190_000;

    let modifier_keys: HashSet<String> = (0..MODIFIER_KEYS)
        .map(|i| format!("modifier_key_{:06}", i))
        .collect();
    let mut type_index = cwtools_info::TypeIndex::new();
    let mut per_type: HashMap<String, Vec<cwtools_info::TypeInstance>> = HashMap::new();
    for i in 0..TYPE_INSTANCES {
        per_type
            .entry(format!("type_{:02}", i % 40))
            .or_default()
            .push(cwtools_info::TypeInstance {
                name: format!("instance_name_{:06}", i),
                location: cwtools_info::SourceLocation {
                    line: 1,
                    col: 0,
                    end: (1, 0),
                },
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            });
    }
    type_index.merge("file:///bench/defs.txt", per_type);
    eprintln!(
        "loc_ref_names: {} modifier keys, {TYPE_INSTANCES} type instances",
        modifier_keys.len(),
    );
    let base = loc_extra_valid_refs(&modifier_keys, &type_index);
    let overlay: HashSet<String> = (0..4_000).map(|i| format!("overlay_key_{i:06}")).collect();

    let mut group = c.benchmark_group("loc_ref_names");
    group.bench_function("loc_extra_valid_refs", |b| {
        b.iter(|| loc_extra_valid_refs(black_box(&modifier_keys), &type_index).len())
    });
    group.bench_function("overlay_merge_before", |b| {
        b.iter(|| {
            let mut combined = base.clone();
            combined.extend(overlay.iter().cloned());
            combined.len()
        })
    });
    group.bench_function("overlay_rebuild_after", |b| {
        b.iter(|| {
            let mut keys = HashSet::new();
            keys.extend(overlay.iter().cloned());
            keys.len()
        })
    });
    group.finish();
}

fn bench_index_parsed_file(c: &mut Criterion) {
    let (Some(rules_dir), Some(corpus)) = (
        checkout("CWTOOLS_RULES", "cwtools-hoi4-config/Config"),
        checkout("CWTOOLS_CORPUS", "Millennium-Dawn"),
    ) else {
        eprintln!(
            "index_parsed_file: no corpus. Set CWTOOLS_RULES to a cwtools-hoi4-config/Config \
             checkout and CWTOOLS_CORPUS to a Millennium-Dawn checkout (or CWTOOLS_PROJECTS \
             to the folder holding both)"
        );
        return;
    };

    let table = StringTable::new();
    let (ruleset, _errors) = cwtools_rules::ruleset_loader::load_ruleset_from_dir(
        &rules_dir,
        &table,
        cwtools_file_manager::file_manager::ScanBudget::default(),
    );
    eprintln!(
        "index_parsed_file: ruleset has {} types / {} aliases from {}",
        ruleset.types.len(),
        ruleset.aliases.len(),
        rules_dir.display()
    );

    for (label, logical_path) in CORPUS_FILES {
        let game_file = corpus.join(logical_path);
        let Ok(text) = std::fs::read_to_string(&game_file) else {
            eprintln!(
                "index_parsed_file: skipping missing fixture {}",
                game_file.display()
            );
            continue;
        };
        let parsed = parse_string(&text, &table);
        let uri = format!("file:///bench/{logical_path}");
        eprintln!(
            "index_parsed_file/{label}: {logical_path} ({} bytes)",
            text.len()
        );

        let mut group = c.benchmark_group(format!("index_parsed_file/{label}"));
        group.bench_function("collect_type_instances_with_subtypes", |b| {
            b.iter(|| {
                let collected = cwtools_info::collect_type_instances_with_subtypes(
                    &ruleset,
                    &parsed,
                    logical_path,
                    &table,
                    cwtools_validation::subtype_membership_for_instance,
                );
                collected.instances.len() + collected.subtype_instances.len()
            })
        });
        let mut info = cwtools_info::InfoService::new();
        group.bench_function("collect_clear_file_index_file", |b| {
            b.iter(|| {
                let collected = cwtools_info::collect_type_instances_with_subtypes(
                    &ruleset,
                    &parsed,
                    logical_path,
                    &table,
                    cwtools_validation::subtype_membership_for_instance,
                );
                info.clear_file(&uri);
                info.index_file_with_precomputed_instances(
                    &uri,
                    &parsed,
                    &table,
                    &ruleset,
                    logical_path,
                    collected.instances,
                    collected.subtype_instances,
                    false,
                );
                info.export_fingerprint(&uri)
            })
        });

        let mut type_index = cwtools_info::TypeIndex::new();
        type_index.merge(
            &uri,
            cwtools_info::collect_type_instances(&ruleset, &parsed, logical_path, &table),
        );
        group.bench_function("cw100_recollect_check", |b| {
            b.iter(|| {
                let per_type =
                    cwtools_info::collect_type_instances(&ruleset, &parsed, logical_path, &table);
                let instances: Vec<_> = per_type
                    .iter()
                    .flat_map(|(type_name, values)| {
                        values
                            .iter()
                            .map(move |instance| (type_name.as_str(), instance))
                    })
                    .collect();
                cwtools_validation::missing_loc::check_missing_localisation(
                    &instances,
                    logical_path,
                    &logical_path.into(),
                    &ruleset,
                    |_| true,
                )
                .len()
            })
        });
        group.bench_function("cw100_indexed_check", |b| {
            b.iter(|| {
                let instances = type_index.instances_in_file(&uri);
                cwtools_validation::missing_loc::check_missing_localisation(
                    &instances,
                    logical_path,
                    &logical_path.into(),
                    &ruleset,
                    |_| true,
                )
                .len()
            })
        });
        group.finish();
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = bench_keystroke_validate, bench_loc_edit_parse, bench_loc_ref_names, bench_index_parsed_file
}
criterion_main!(benches);
