//! A workspace scan must leave the same index as indexing each file in turn.
//! The reference here is the plain serial loop over `InfoService`, in
//! discovery order, so a scan that collects in parallel or merges out of order
//! shows up as a diff.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tower_lsp::lsp_types::Url;

use cwtools_info::{InfoService, variable_defining_effects};
use cwtools_parser::parser::{parse_string, parse_string_without_comments};
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_rules::rules_types::RuleSet;
use cwtools_string_table::string_table::StringTable;

use crate::Backend;
use crate::command_progress::{CommandProgress, ScanOutcome};
use crate::paths::{path_to_uri, workspace_prefix_of};
use crate::testing::test_backend;

const FILES_PER_KIND: usize = 24;

const RULES: &str = r#"
types = {
    type[effect_def] = { path = "game/common/scripted_effects" }
    type[decision] = { path = "game/common/decisions" }
}
enums = {
    complex_enum[effect_marker] = {
        path = "game/common/scripted_effects"
        start_from_root = yes
        name = {
            enum_name = scalar
        }
    }
}
decision = {
    requires = <effect_def>
    complete_effect = { alias_name[effect] = alias_match_left[effect] }
}
effect_def = { alias_name[effect] = alias_match_left[effect] }
alias[effect:<effect_def>] = yes
alias[effect:set_variable] = {
    var = value_set[variable]
    value = int_variable_field
}
alias[effect:set_country_flag] = value_set[country_flag]
alias[effect:save_event_target_as] = scalar
alias[effect:log] = scalar
"#;

fn ruleset() -> RuleSet {
    let table = StringTable::new();
    ast_to_ruleset(&parse_string(RULES, &table), &table)
}

/// Writes `FILES_PER_KIND` files of each kind under `root` and returns their
/// root-relative paths in discovery (sorted) order.
fn write_workspace(root: &std::path::Path) -> Vec<String> {
    type Content = fn(usize) -> String;
    let kinds: [(&str, &str, Content); 4] = [
        ("common/decisions", "dec", |i| {
            let next = (i + 1) % FILES_PER_KIND;
            format!(
                "dec_{i:02} = {{\n    requires = se_{next:02}\n    \
                 complete_effect = {{ se_{next:02} = yes }}\n    \
                 inline_script = {{ script = shared/block_{i:02} }}\n}}\n"
            )
        }),
        ("common/scripted_effects", "se", |i| {
            format!(
                "@c_{i:02} = {i}\nse_{i:02} = {{\n    \
                 set_variable = {{ var = v_{i:02} value = {i} }}\n    \
                 set_country_flag = flag_{i:02}\n    \
                 save_event_target_as = tgt_{i:02}\n    log = \"hello\"\n}}\n"
            )
        }),
        ("common/scripted_guis", "sg", |i| {
            format!(
                "scripted_gui = {{\n    sg_{i:02} = {{\n        \
                 effects = {{ click_{i:02} = {{ }} }}\n        \
                 triggers = {{ visible_{i:02} = {{ }} }}\n    }}\n}}\n"
            )
        }),
        ("common/scripted_localisation", "sl", |i| {
            format!("defined_text = {{\n    name = sl_{i:02}\n    text = {{ }}\n}}\n")
        }),
    ];
    let mut relative = Vec::new();
    for (dir, stem, content) in kinds {
        std::fs::create_dir_all(root.join(dir)).unwrap();
        for i in 0..FILES_PER_KIND {
            let rel = format!("{dir}/{stem}_{i:02}.txt");
            std::fs::write(root.join(&rel), content(i)).unwrap();
            relative.push(rel);
        }
    }
    relative.sort();
    relative
}

fn backend_for(root: &std::path::Path, cache_dir: &std::path::Path) -> Backend {
    let backend = test_backend();
    let ws_uri = Url::from_file_path(root).unwrap();
    {
        let mut cfg = backend.state.config.write();
        cfg.workspace_uri = Some(ws_uri.as_str().into());
        cfg.workspace_prefix = Some(workspace_prefix_of(ws_uri.as_str()));
        cfg.cache_dir = Some(cache_dir.to_path_buf());
    }
    backend.set_ruleset(ruleset());
    backend
}

async fn scan(backend: &Backend) {
    let progress =
        CommandProgress::for_tests(backend.state.clone(), Arc::new(AtomicBool::new(false)));
    assert_eq!(
        backend
            .validate_entire_workspace_tracked(false, Some(&progress))
            .await,
        ScanOutcome::Ran
    );
}

/// Indexes every file in order through `InfoService` alone.
fn serial_reference(root: &std::path::Path, relative: &[String]) -> InfoService {
    let ruleset = ruleset();
    let table = StringTable::new();
    let mut info = InfoService::new();
    info.update_ruleset_data(variable_defining_effects(&ruleset));
    for rel in relative {
        let path = root.join(rel);
        let uri = path_to_uri(&path);
        let parsed =
            parse_string_without_comments(&std::fs::read_to_string(&path).unwrap(), &table);
        let collected = cwtools_info::collect_type_instances_with_subtypes(
            &ruleset,
            &parsed,
            rel,
            &table,
            cwtools_validation::subtype_membership_for_instance,
        );
        info.begin_file_replacement(&uri);
        info.clear_file(&uri);
        info.index_file_with_precomputed_instances(
            &uri,
            &parsed,
            &table,
            &ruleset,
            rel,
            collected.instances,
            collected.subtype_instances,
            false,
        );
        info.finish_file_replacement(&uri);
    }
    info.rebuild_alias_key_index(&ruleset);
    info
}

type Snapshot = BTreeMap<String, Vec<String>>;

/// Everything the scan writes into the index, as sorted sections. Sections
/// whose order is part of the contract (instances per type, definitions per
/// type, variables per namespace) keep it.
fn snapshot(info: &InfoService) -> Snapshot {
    let mut out = Snapshot::new();
    let sorted = |mut lines: Vec<String>| {
        lines.sort();
        lines
    };
    for (type_name, instances) in info.type_index.map() {
        let lines = instances
            .iter()
            .map(|(uri, inst)| {
                format!(
                    "{uri} {} {}:{}",
                    inst.name, inst.location.line, inst.location.col
                )
            })
            .collect();
        out.insert(format!("instances:{type_name}"), lines);
    }
    for (type_name, defs) in &info.all_type_defs {
        let lines = defs
            .iter()
            .map(|(uri, loc)| format!("{uri} {}:{}", loc.line, loc.col))
            .collect();
        out.insert(format!("type_defs:{type_name}"), lines);
    }
    for (uri, file) in &info.files {
        let mut lines = vec![format!(
            "export {} references {}",
            info.export_fingerprint(uri),
            info.reference_fingerprint(uri)
        )];
        let mut namespaces: Vec<_> = file.defined_variables_ns.iter().collect();
        namespaces.sort_by_key(|(ns, _)| ns.as_str());
        for (ns, vars) in namespaces {
            lines.extend(vars.iter().map(|v| {
                format!(
                    "var {ns} {} {:?} {}:{}",
                    v.name, v.value, v.location.line, v.location.col
                )
            }));
        }
        lines.extend(file.saved_event_targets_detailed.iter().map(|t| {
            format!(
                "target {} {}:{} {}",
                t.name, t.location.line, t.location.col, t.is_global
            )
        }));
        lines.extend(sorted(
            file.saved_event_targets
                .iter()
                .map(|t| format!("saved {t}"))
                .collect(),
        ));
        lines.extend(sorted(
            file.inline_scripts
                .iter()
                .map(|(name, loc)| format!("inline {name} {}:{}", loc.line, loc.col))
                .collect(),
        ));
        lines.extend(sorted(
            file.type_definitions
                .iter()
                .flat_map(|(ty, locs)| {
                    locs.iter()
                        .map(move |loc| format!("typedef {ty} {}:{}", loc.line, loc.col))
                })
                .collect(),
        ));
        lines.extend(sorted(
            file.type_references
                .iter()
                .flat_map(|(ty, locs)| {
                    locs.iter()
                        .map(move |loc| format!("typeref {ty} {}:{}", loc.line, loc.col))
                })
                .collect(),
        ));
        lines.extend(sorted(
            file.defined_variables
                .keys()
                .map(|name| format!("at {name}"))
                .collect(),
        ));
        lines.extend(sorted(
            file.export_instance_names
                .iter()
                .map(|name| format!("export_name {name}"))
                .collect(),
        ));
        lines.extend(sorted(
            info.file_reference_sites(uri)
                .iter()
                .map(|site| {
                    format!(
                        "use {} {} {}:{} {}",
                        site.type_name,
                        site.name,
                        site.site.key.line,
                        site.site.key.col,
                        site.case_insensitive
                    )
                })
                .collect(),
        ));
        out.insert(format!("file:{uri}"), lines);
    }
    for (section, counts) in [
        ("variables", &info.variable_counts),
        ("event_targets", &info.event_target_counts),
        ("inline_scripts", &info.inline_script_counts),
    ] {
        let lines = counts
            .iter()
            .map(|(name, count)| format!("{name}={count}"))
            .collect();
        out.insert(format!("counts:{section}"), sorted(lines));
    }
    let type_index = &info.type_index;
    for (kind, sets) in [
        ("value_set", type_index.value_set_values.export()),
        ("complex_enum", type_index.complex_enum_values.export()),
    ] {
        for (name, values) in sets {
            out.insert(format!("{kind}:{name}"), sorted(values));
        }
    }
    out.insert(
        "scripted_loc".into(),
        sorted(
            type_index
                .scripted_loc_index
                .names()
                .map(str::to_string)
                .collect(),
        ),
    );
    out.insert(
        "scripted_gui".into(),
        sorted(
            type_index
                .scripted_gui_index
                .names()
                .map(str::to_string)
                .collect(),
        ),
    );
    out.insert(
        "var_index".into(),
        sorted(type_index.var_index.names().cloned().collect()),
    );
    out
}

fn section<'a>(snapshot: &'a Snapshot, key: &str) -> &'a [String] {
    snapshot
        .get(key)
        .unwrap_or_else(|| panic!("no section {key}; have {:?}", snapshot.keys()))
}

fn numbered(format: impl Fn(usize) -> String) -> Vec<String> {
    (0..FILES_PER_KIND).map(format).collect()
}

/// The collectors must each have produced something, or the comparison below
/// proves nothing about them.
fn assert_collectors_ran(snapshot: &Snapshot, root: &std::path::Path) {
    let effect_names: Vec<String> = section(snapshot, "instances:effect_def")
        .iter()
        .map(|line| line.split(' ').nth(1).unwrap().to_string())
        .collect();
    assert_eq!(
        effect_names,
        numbered(|i| format!("se_{i:02}")),
        "instances merge in discovery order"
    );
    let decision_names: Vec<String> = section(snapshot, "instances:decision")
        .iter()
        .map(|line| line.split(' ').nth(1).unwrap().to_string())
        .collect();
    assert_eq!(decision_names, numbered(|i| format!("dec_{i:02}")));
    assert_eq!(
        section(snapshot, "value_set:country_flag"),
        numbered(|i| format!("flag_{i:02}"))
    );
    assert_eq!(
        section(snapshot, "complex_enum:effect_marker"),
        numbered(|i| format!("@c_{i:02}"))
    );
    assert_eq!(
        section(snapshot, "scripted_loc"),
        numbered(|i| format!("sl_{i:02}"))
    );
    let mut guis = numbered(|i| format!("click_{i:02}"));
    guis.extend(numbered(|i| format!("visible_{i:02}")));
    guis.sort();
    assert_eq!(section(snapshot, "scripted_gui"), guis);
    let vars = section(snapshot, "var_index");
    for i in 0..FILES_PER_KIND {
        assert!(vars.contains(&format!("v_{i:02}")), "v_{i:02} indexed");
    }
    assert_eq!(
        section(snapshot, "counts:inline_scripts").len(),
        FILES_PER_KIND
    );
    assert_eq!(
        section(snapshot, "counts:event_targets"),
        numbered(|i| format!("tgt_{i:02}=1")).as_slice()
    );
    let decision = path_to_uri(&root.join("common/decisions/dec_00.txt"));
    let decision_file = section(snapshot, &format!("file:{decision}"));
    assert!(
        decision_file
            .iter()
            .any(|line| line.starts_with("use effect_def se_01 ")),
        "a type reference is indexed: {decision_file:?}"
    );
    assert!(
        decision_file
            .iter()
            .any(|line| line.starts_with("use effect_def se_01 ") && line.ends_with(" true")),
        "an alias-key reference is classified: {decision_file:?}"
    );
    assert!(
        decision_file
            .iter()
            .any(|line| line.starts_with("inline shared/block_00 ")),
        "an inline script call is indexed: {decision_file:?}"
    );
    let effect = path_to_uri(&root.join("common/scripted_effects/se_00.txt"));
    let effect_file = section(snapshot, &format!("file:{effect}"));
    assert!(effect_file.iter().any(|l| l.starts_with("var @ @c_00 ")));
    assert!(
        effect_file
            .iter()
            .any(|l| l.starts_with("var variable v_00 "))
    );
    assert!(effect_file.iter().any(|l| l.starts_with("target tgt_00 ")));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scan_indexes_what_a_serial_pass_over_the_files_would() {
    let tmp = tempfile::TempDir::new().unwrap();
    let relative = write_workspace(tmp.path());
    let backend = backend_for(tmp.path(), &tmp.path().join("cache"));
    let reference = snapshot(&serial_reference(tmp.path(), &relative));
    assert_collectors_ran(&reference, tmp.path());

    scan(&backend).await;
    let cold = snapshot(&backend.state.info_service.read());
    assert_same(&cold, &reference, "cold scan");

    let restarted = backend_for(tmp.path(), &tmp.path().join("cache"));
    scan(&restarted).await;
    let warm = snapshot(&restarted.state.info_service.read());
    assert_same(&warm, &reference, "warm start, every file a cache hit");
    if cwtools_cache::workspace::PATH_METADATA_CACHE_SUPPORTED {
        assert_eq!(
            restarted
                .state
                .scan_source_reads
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "the warm start parsed nothing, so it indexed from the cache"
        );
    }
}

/// `assert_eq!` on two snapshots prints both in full. Name the sections that
/// differ and the first line where each does.
fn assert_same(actual: &Snapshot, expected: &Snapshot, what: &str) {
    let keys: std::collections::BTreeSet<_> = actual.keys().chain(expected.keys()).collect();
    let diffs: Vec<String> = keys
        .into_iter()
        .filter(|&key| actual.get(key) != expected.get(key))
        .take(5)
        .map(|key| {
            let (got, want) = (actual.get(key), expected.get(key));
            let first = got.into_iter().flatten().zip(want.into_iter().flatten());
            let line = first
                .enumerate()
                .find(|(_, (a, b))| a != b)
                .map(|(i, (a, b))| format!("line {i}: scan {a:?}, serial {b:?}"))
                .unwrap_or_else(|| {
                    format!(
                        "lengths scan {:?}, serial {:?}",
                        got.map(Vec::len),
                        want.map(Vec::len)
                    )
                });
            format!("{key}: {line}")
        })
        .collect();
    assert!(
        diffs.is_empty(),
        "{what} differs from serial:\n{}",
        diffs.join("\n")
    );
}
