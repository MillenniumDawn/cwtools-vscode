//! The per-edit work behind diagnostics: what a keystroke pays in a script
//! buffer or a localisation buffer, and what indexing a parsed file costs.
//!
//! `keystroke_validate`, `loc_edit_parse` and `loc_ref_names` run on fixtures
//! and need no checkout. `loc_edit_parse` and `loc_ref_names` keep a `before`
//! and `after` pair for the change (#87) that made them cheaper.
//!
//! `index_parsed_file` indexes three large Millennium-Dawn files (a focus
//! tree, an events file and an equipment file) against the real hoi4 rules.
//! Its collector cases include output drops, like clearing a prior file index:
//!
//!   CWTOOLS_CORPUS=/path/to/Millennium-Dawn \
//!   CWTOOLS_RULES=/path/to/cwtools-hoi4-config/Config \
//!     cargo bench -p cwtools_lsp --bench validate
//!
//! Both are also found under `CWTOOLS_PROJECTS`, or as siblings of this repo.
//! When either is missing that group prints why and measures nothing.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use cwtools_info::InfoService;
use cwtools_lsp::bench_support::{
    collect_doc_tokens, loc_extra_valid_refs, logical_path_from_uri, parse_loc_buffer,
    workspace_prefix_of,
};
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_types::{
    NewField, Options, PathOptions, RootRule, RuleSet, RuleType, TypeDefinition,
};
use cwtools_string_table::string_table::StringTable;
use cwtools_test_support::{checkout, testfiles};
use std::sync::Arc;

/// The same three kinds of file this once ran on from a base-game install: a
/// focus tree, an events file and a plane equipment file.
const CORPUS_FILES: [(&str, &str); 3] = [
    ("focus_tree", "common/national_focus/05_iran.txt"),
    ("events", "events/France.txt"),
    ("equipment", "common/units/equipment/MD_plane_airframes.txt"),
];

fn bench_keystroke_validate(c: &mut Criterion) {
    let text =
        std::fs::read_to_string(testfiles().join("performancetest2/events/cc_colony_events.txt"))
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
        group.bench_function("collect_defined_variables", |b| {
            b.iter(|| {
                black_box(cwtools_info::collect_defined_variables_from_rules(
                    &ruleset,
                    &parsed,
                    logical_path,
                    &table,
                    None,
                ))
            })
        });
        group.bench_function("collect_value_sets", |b| {
            b.iter(|| {
                black_box(cwtools_info::collect_variables_and_value_sets(
                    &parsed, &ruleset, &table, None,
                ))
            })
        });
        group.bench_function("collect_complex_enums", |b| {
            b.iter(|| {
                black_box(cwtools_info::dynamic_values::collect_complex_enum_values(
                    &ruleset,
                    &parsed,
                    logical_path,
                    &table,
                ))
            })
        });
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
                    collected,
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
        let mut collectors = cwtools_info::bench_support::FileCollectors::new(&ruleset);
        let mut reference_check = InfoService::new();
        cwtools_info::bench_support::merge(
            &mut reference_check.reference_index,
            &uri,
            collectors.type_refs(&parsed, &table, &ruleset, logical_path),
        );
        eprintln!(
            "index_parsed_file/{label}: type-reference fingerprint {:016x}",
            reference_check.reference_fingerprint(&uri)
        );
        group.bench_function("collect_type_refs", |b| {
            b.iter(|| black_box(collectors.type_refs(&parsed, &table, &ruleset, logical_path)))
        });
        group.bench_function("replace_alias_candidates", |b| {
            b.iter(|| collectors.replace_alias_candidates(&uri, &parsed, &table))
        });
        collectors.replace_alias_candidates(&uri, &parsed, &table);
        eprintln!(
            "index_parsed_file/{label}: {}",
            collectors.alias_summary(&uri)
        );
        group.bench_function("classify_alias_keys", |b| {
            b.iter(|| black_box(collectors.alias_refs(&uri, &type_index)))
        });
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

/// Isolate the edit-time variable-set collector with many root types matching
/// one path and many root children. The real MD corpus bench above remains the
/// representative end-to-end measurement; this synthetic case makes the
/// repeated-traversal workload stable and easy to reproduce.
fn bench_synthetic_many_type_index_edit(c: &mut Criterion) {
    const MATCHING_TYPES: usize = 64;
    let table = StringTable::new();
    let mut ruleset = RuleSet::new();
    let mut source = String::from("object = {\n");
    for i in 0..MATCHING_TYPES {
        let type_name = format!("synthetic_{i}");
        let section_name = format!("section_{i}");
        let variable_name = format!("variable_{i}");
        source.push_str(&format!(
            "  {section_name} = {{ {variable_name} = {{}} }}\n"
        ));
        ruleset.types.push(TypeDefinition {
            name: type_name.clone(),
            name_field: None,
            path_options: PathOptions {
                paths: vec!["common/synthetic".to_string()],
                ..Default::default()
            },
            subtypes: Vec::new(),
            type_key_filter: None,
            skip_root_key: Vec::new(),
            starts_with: None,
            type_per_file: false,
            key_prefix: None,
            warning_only: false,
            unique: false,
            should_be_referenced: false,
            localisation: Vec::new(),
            graph_related_types: Vec::new(),
            modifiers: Vec::new(),
        });
        let variable_rule = (
            RuleType::NodeRule {
                left: NewField::VariableSetField("variable".to_string()),
                rules: Arc::from([]),
            },
            Options::default(),
        );
        let section_rule = (
            RuleType::NodeRule {
                left: NewField::SpecificField(section_name),
                rules: Arc::from([variable_rule]),
            },
            Options::default(),
        );
        ruleset.root_rules.push(RootRule::TypeRule(
            type_name,
            (
                RuleType::NodeRule {
                    left: NewField::ScalarField,
                    rules: Arc::from([section_rule]),
                },
                Options::default(),
            ),
        ));
    }
    source.push_str("}\n");
    ruleset.reindex();
    let parsed = parse_string(&source, &table);
    let uri = "file:///bench/synthetic/common/example.txt";
    let mut info = InfoService::new();
    let logical_path = "common/synthetic/example.txt";

    let mut group = c.benchmark_group("index_parsed_file/synthetic_many_types");
    group.bench_function("collect_clear_file_index_file", |b| {
        b.iter(|| {
            info.clear_file(uri);
            info.index_file_with_precomputed_instances(
                uri,
                &parsed,
                &table,
                &ruleset,
                logical_path,
                cwtools_info::CollectedTypeInstances::default(),
                false,
            );
            black_box(&info);
        })
    });
    group.finish();
}

/// The pre-#1153 implementation of `cwtools_lsp`'s scan stat signature, kept
/// here so the candidate is measured against the code it replaces. Bench-only.
fn baseline_stat_signature(files: &[std::path::PathBuf]) -> u64 {
    let mut sorted: Vec<&std::path::Path> = files.iter().map(|p| p.as_path()).collect();
    sorted.sort_unstable();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for path in sorted {
        path.to_string_lossy().hash(&mut hasher);
        if let Ok(meta) = std::fs::metadata(path) {
            meta.len().hash(&mut hasher);
            if let Ok(modified) = meta.modified()
                && let Ok(since_epoch) = modified.duration_since(std::time::UNIX_EPOCH)
            {
                since_epoch.as_nanos().hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// One `stat_signature` workload: either generated files in a temp dir (kept
/// alive so the stats keep resolving) or paths walked from a real checkout.
struct StatSignatureFixture {
    _dir: Option<tempfile::TempDir>,
    paths: Vec<std::path::PathBuf>,
    note: String,
}

/// The size sweep doubles as the crossover record for the parallel threshold:
/// sizes left below it measure serial vs serial and must not move, sizes above
/// it are where the parallel metadata sweep has to win.
const STAT_SIGNATURE_SIZES: [usize; 5] = [3, 64, 256, 512, 1024];

fn stat_signature_fixture(size: usize) -> &'static StatSignatureFixture {
    static FIXTURES: std::sync::OnceLock<Vec<StatSignatureFixture>> = std::sync::OnceLock::new();
    FIXTURES
        .get_or_init(|| {
            STAT_SIGNATURE_SIZES
                .iter()
                .map(|&size| {
                    let dir = tempfile::TempDir::new().expect("stat_signature tempdir");
                    let mut paths = Vec::with_capacity(size);
                    for i in 0..size {
                        let path = dir.path().join(format!("f_{i:05}.yml"));
                        std::fs::write(&path, format!("l_english:\n key_{i:05}:0 \"value\"\n"))
                            .expect("stat_signature fixture file");
                        paths.push(path);
                    }
                    StatSignatureFixture {
                        note: format!("{size} generated files in a temp dir"),
                        paths,
                        _dir: Some(dir),
                    }
                })
                .collect()
        })
        .iter()
        .find(|fixture| fixture.paths.len() == size)
        .expect("fixture registered for every sweep size")
}

fn corpus_stat_signature_fixture(cap: usize) -> &'static StatSignatureFixture {
    static FIXTURE: std::sync::OnceLock<StatSignatureFixture> = std::sync::OnceLock::new();
    FIXTURE.get_or_init(|| {
        let Some(corpus) = checkout("CWTOOLS_CORPUS", "Millennium-Dawn") else {
            return StatSignatureFixture {
                note: "no corpus; set CWTOOLS_CORPUS or CWTOOLS_PROJECTS".to_string(),
                paths: Vec::new(),
                _dir: None,
            };
        };
        let mut paths = Vec::new();
        let mut stack = vec![corpus];
        while let Some(dir) = stack.pop() {
            if paths.len() >= cap {
                break;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_dir() {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    if !name.starts_with('.') {
                        stack.push(path);
                    }
                } else if paths.len() < cap {
                    paths.push(path);
                }
                if paths.len() >= cap {
                    break;
                }
            }
        }
        StatSignatureFixture {
            note: format!("{} Millennium-Dawn paths (cap {cap})", paths.len()),
            paths,
            _dir: None,
        }
    })
}

/// Median without the advisory-`unwrap` partial_cmp sort. Bench-only.
fn bench_median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    let mid = samples.len() / 2;
    if samples.len().is_multiple_of(2) {
        (samples[mid - 1] + samples[mid]) / 2.0
    } else {
        samples[mid]
    }
}

/// True interleaved paired comparison. Criterion's separate bench functions
/// run strictly one after another, so their per-run medians never interleave
/// and a load burst can land on just one side. This harness alternates which
/// strategy is measured first on every pair over the same fixture and reports
/// the paired results, so order and moment-to-moment load cancel out (#1153).
/// Bench-only.
fn paired_stat_signature_comparison(
    label: &str,
    paths: &[std::path::PathBuf],
    pairs: usize,
    calls_per_sample: usize,
) {
    use std::time::Instant;

    use cwtools_lsp::bench_support::stat_signature_for;

    // Warm both strategies so neither pays first-touch costs inside the pairs.
    for _ in 0..4 {
        black_box(baseline_stat_signature(paths));
        black_box(stat_signature_for(paths));
    }
    let mut baseline_us: Vec<f64> = Vec::with_capacity(pairs);
    let mut candidate_us: Vec<f64> = Vec::with_capacity(pairs);
    let mut baseline_first = true;
    for _ in 0..pairs {
        let (baseline, candidate) = if baseline_first {
            let start = Instant::now();
            for _ in 0..calls_per_sample {
                black_box(baseline_stat_signature(paths));
            }
            (start.elapsed(), {
                let start = Instant::now();
                for _ in 0..calls_per_sample {
                    black_box(stat_signature_for(paths));
                }
                start.elapsed()
            })
        } else {
            let candidate = {
                let start = Instant::now();
                for _ in 0..calls_per_sample {
                    black_box(stat_signature_for(paths));
                }
                start.elapsed()
            };
            let start = Instant::now();
            for _ in 0..calls_per_sample {
                black_box(baseline_stat_signature(paths));
            }
            (start.elapsed(), candidate)
        };
        baseline_us.push(baseline.as_nanos() as f64 / calls_per_sample as f64 / 1_000.0);
        candidate_us.push(candidate.as_nanos() as f64 / calls_per_sample as f64 / 1_000.0);
        baseline_first = !baseline_first;
    }

    let paired_diffs: Vec<f64> = candidate_us
        .iter()
        .zip(&baseline_us)
        .map(|(candidate, baseline)| candidate - baseline)
        .collect();
    let wins = paired_diffs.iter().filter(|diff| **diff < 0.0).count();
    eprintln!(
        "stat_signature paired ({label}, {pairs} pairs x {calls_per_sample} calls/sample, alternating order): \
         baseline {:.3} µs/call, candidate {:.3} µs/call, paired diff median {:+.3} µs/call, \
         candidate faster in {wins}/{pairs} pairs",
        bench_median(baseline_us.clone()),
        bench_median(candidate_us.clone()),
        bench_median(paired_diffs),
    );
}

/// The paired cases: n3 (tiny, serial path), n512 (the threshold boundary,
/// parallel path) and the 20k-path corpus when a checkout is available.
fn paired_stat_signature_comparisons() {
    for (size, pairs, calls_per_sample) in [(3, 64, 400), (512, 48, 20)] {
        let fixture = stat_signature_fixture(size);
        paired_stat_signature_comparison(
            &format!("n{size}: {}", fixture.note),
            &fixture.paths,
            pairs,
            calls_per_sample,
        );
    }
    let corpus = corpus_stat_signature_fixture(20_000);
    if corpus.paths.is_empty() {
        eprintln!("stat_signature paired (corpus): skipped, {}", corpus.note);
    } else {
        paired_stat_signature_comparison(&format!("corpus: {}", corpus.note), &corpus.paths, 40, 1);
    }
}

fn bench_stat_signature(c: &mut Criterion) {
    use cwtools_lsp::bench_support::stat_signature_for;

    paired_stat_signature_comparisons();
    let mut group = c.benchmark_group("stat_signature");
    let corpus = corpus_stat_signature_fixture(20_000);
    eprintln!("stat_signature: corpus fixture uses {}", corpus.note);
    for size in STAT_SIGNATURE_SIZES {
        let fixture = stat_signature_fixture(size);
        eprintln!("stat_signature: {size} files = {}", fixture.note);
        group.bench_function(format!("n{size}/baseline_serial"), |b| {
            b.iter(|| baseline_stat_signature(black_box(&fixture.paths)))
        });
        group.bench_function(format!("n{size}/candidate"), |b| {
            b.iter(|| stat_signature_for(black_box(&fixture.paths)))
        });
    }
    group.bench_function("corpus/baseline_serial", |b| {
        b.iter(|| baseline_stat_signature(black_box(&corpus.paths)))
    });
    group.bench_function("corpus/candidate", |b| {
        b.iter(|| stat_signature_for(black_box(&corpus.paths)))
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = bench_keystroke_validate, bench_loc_edit_parse, bench_loc_ref_names, bench_index_parsed_file, bench_synthetic_many_type_index_edit, bench_stat_signature
}
criterion_main!(benches);
