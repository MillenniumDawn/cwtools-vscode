use cwtools_parser::ast::{Arena, Child, ParsedFile, Value};
use cwtools_parser::unquote;
use cwtools_rules::rules_types::{RuleSet, SkipRootKey, TypeDefinition};
use cwtools_string_table::string_table::StringTable;
use std::collections::{HashMap, HashSet};

use crate::dynamic_values;
use crate::{
    DefinedVariable, NormalizedPath, SourceLocation, TypeIndex, TypeInstance, check_path_dir_norm,
    get_string_or_empty, leaf_value_string,
};

pub fn skip_root_key_matches(srk: &SkipRootKey, key: &str) -> bool {
    match srk {
        SkipRootKey::SpecificKey(k) => k.eq_ignore_ascii_case(key),
        SkipRootKey::AnyKey => true,
        SkipRootKey::MultipleKeys(keys, match_kind) => {
            keys.iter().any(|k| k.eq_ignore_ascii_case(key)) == match_kind.is_equals()
        }
    }
}

fn type_key_filter_matches(td: &TypeDefinition, key: &str) -> bool {
    match &td.type_key_filter {
        None => true,
        Some((keys, negate)) => {
            let hit = keys.iter().any(|k| k.eq_ignore_ascii_case(key));
            if *negate { !hit } else { hit }
        }
    }
}

fn starts_with_matches(td: &TypeDefinition, key: &str) -> bool {
    match &td.starts_with {
        None => true,
        Some(prefix) => {
            key.len() >= prefix.len()
                && key.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
        }
    }
}

// F# `type_key_prefix` compares the type's prefix against a node's own KeyPrefix
fn key_prefix_matches(td: &TypeDefinition, key: &str) -> bool {
    match &td.key_prefix {
        None => true,
        Some(prefix) => {
            key.len() >= prefix.len()
                && key.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
        }
    }
}

pub fn type_key_matches(td: &TypeDefinition, key: &str) -> bool {
    type_key_filter_matches(td, key) && starts_with_matches(td, key) && key_prefix_matches(td, key)
}

/// Explicit child field supplying an instance's primary localisation key; pattern-based keys need no capture.
fn primary_explicit_loc_field(td: &TypeDefinition) -> Option<&str> {
    td.localisation
        .iter()
        .find(|l| l.primary && l.explicit_field.is_some())
        .and_then(|l| l.explicit_field.as_deref())
}

/// Child fields read by explicit-key `## required` localisation entries; `$`-pattern entries need no capture.
fn required_explicit_loc_fields(td: &TypeDefinition) -> impl Iterator<Item = &str> {
    td.localisation
        .iter()
        .filter_map(|l| l.required_explicit_field())
}

fn field_value_from_children(
    field_name: &str,
    children: &[Child],
    arena: &Arena,
    table: &StringTable,
) -> Option<String> {
    for child in children {
        if let Child::Leaf(li) = child {
            let leaf = &arena.leaves[*li as usize];
            let matches = table
                .with_string(leaf.key.normal, |k| k.eq_ignore_ascii_case(field_name))
                .unwrap_or(false);
            if matches {
                let v = leaf_value_string(&leaf.value, table);
                let v = unquote(&v);
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

fn instance_name_from_children(
    td: &TypeDefinition,
    node_key: &str,
    children: &[Child],
    arena: &Arena,
    table: &StringTable,
) -> Option<String> {
    match &td.name_field {
        None => Some(unquote(node_key).to_string()),
        Some(field_name) => field_value_from_children(field_name, children, arena, table),
    }
}

fn walk_skip_root_child<V>(
    td: &TypeDefinition,
    skip_stack: &[SkipRootKey],
    child: &Child,
    arena: &Arena,
    table: &StringTable,
    visit: &mut V,
) where
    V: FnMut(&TypeDefinition, String, &str, &[Child], SourceLocation),
{
    let Some(kc) = arena.keyed_clause(child) else {
        return; // not a keyed clause — skip
    };
    let clause_children = kc.children;
    let location = SourceLocation {
        line: kc.pos.start.line,
        col: kc.pos.start.col,
        end: (kc.pos.end.line, kc.pos.end.col),
    };

    let key = get_string_or_empty(table, kc.key.normal);
    match skip_stack {
        [] => {
            if type_key_matches(td, &key)
                && let Some(name) =
                    instance_name_from_children(td, &key, clause_children, arena, table)
            {
                visit(td, name, &key, clause_children, location);
            }
        }
        [head, tail @ ..] => {
            if skip_root_key_matches(head, &key) {
                for inner_child in clause_children {
                    walk_skip_root_child(td, tail, inner_child, arena, table, visit);
                }
            }
        }
    }
}

pub struct InstanceNode<'a> {
    pub td: &'a TypeDefinition,
    pub name: &'a str,
    pub node_key: &'a str,
    pub children: &'a [Child],
    pub location: SourceLocation,
}

pub type SubtypeCollector =
    fn(&RuleSet, &ParsedFile, &InstanceNode, &StringTable, &mut HashMap<String, Vec<TypeInstance>>);

#[derive(Default)]
pub struct CollectedTypeInstances {
    pub instances: HashMap<String, Vec<TypeInstance>>,
    pub subtype_instances: HashMap<String, Vec<TypeInstance>>,
}

pub fn for_each_instance_node<F>(
    ruleset: &RuleSet,
    file: &ParsedFile,
    logical_path: &str,
    table: &StringTable,
    f: &mut F,
) where
    F: FnMut(&TypeDefinition, &str, &str, &[Child], SourceLocation),
{
    let np = NormalizedPath::new(logical_path);
    for td in &ruleset.types {
        if td.type_per_file || td.subtypes.is_empty() || !check_path_dir_norm(&td.path_options, &np)
        {
            continue;
        }
        let mut visit =
            |td: &TypeDefinition, name: String, key: &str, children: &[Child], location| {
                f(td, &name, key, children, location);
            };
        for child in &file.root_children {
            walk_skip_root_child(td, &td.skip_root_key, child, &file.arena, table, &mut visit);
        }
    }
}

pub fn mix_export_symbol(parts: &[&str]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for p in parts {
        p.hash(&mut h);
        0xffu8.hash(&mut h);
    }
    h.finish()
}

/// [`InfoService::export_fingerprint`].
pub fn hash_instance_exports(per_type: &HashMap<String, Vec<TypeInstance>>) -> u64 {
    let mut acc: u64 = 0;
    for (ty, instances) in per_type {
        for inst in instances {
            acc = acc.wrapping_add(mix_export_symbol(&["t", ty, &inst.name]));
        }
    }
    acc
}

pub fn collect_type_instances(
    ruleset: &RuleSet,
    file: &ParsedFile,
    logical_path: &str,
    table: &StringTable,
) -> HashMap<String, Vec<TypeInstance>> {
    collect_type_instances_inner(ruleset, file, logical_path, table, None, None)
}

pub fn collect_type_instances_with_subtypes(
    ruleset: &RuleSet,
    file: &ParsedFile,
    logical_path: &str,
    table: &StringTable,
    subtype_hook: SubtypeCollector,
) -> CollectedTypeInstances {
    let mut subtype_instances = HashMap::new();
    let instances = collect_type_instances_inner(
        ruleset,
        file,
        logical_path,
        table,
        Some(subtype_hook),
        Some(&mut subtype_instances),
    );
    CollectedTypeInstances {
        instances,
        subtype_instances,
    }
}

const SCRIPTED_LOC_DIRS: [&str; 3] = [
    "scripted_localisation",
    "scripted_localization",
    "scripted_loc",
];

pub fn collect_scripted_loc_names(
    file: &ParsedFile,
    logical_path: &str,
    table: &StringTable,
) -> Vec<String> {
    let dir = logical_path.replace('\\', "/").to_ascii_lowercase();
    if !SCRIPTED_LOC_DIRS
        .iter()
        .any(|d| crate::path_contains_segment(&dir, d))
    {
        return Vec::new();
    }
    let arena = &file.arena;
    let mut names = Vec::new();
    for child in &file.root_children {
        let Some(kc) = arena.keyed_clause(child) else {
            continue;
        };
        if let Some(name) = field_value_from_children("name", kc.children, arena, table) {
            names.push(name);
        }
    }
    names
}

pub fn collect_scripted_gui_callback_names(
    file: &ParsedFile,
    logical_path: &str,
    table: &StringTable,
) -> Vec<String> {
    let path = logical_path.replace('\\', "/").to_ascii_lowercase();
    if !crate::path_contains_segment(&path, "common/scripted_guis") {
        return Vec::new();
    }

    let arena = &file.arena;
    let mut names = Vec::new();
    for root_child in &file.root_children {
        let Some(root) = arena.keyed_clause(root_child) else {
            continue;
        };
        let is_scripted_gui_root = table
            .with_string(root.key.normal, |key| {
                key.eq_ignore_ascii_case("scripted_gui")
            })
            .unwrap_or(false);
        if !is_scripted_gui_root {
            continue;
        }
        for gui_child in root.children {
            let Some(gui) = arena.keyed_clause(gui_child) else {
                continue;
            };
            for container_child in gui.children {
                let Some(container) = arena.keyed_clause(container_child) else {
                    continue;
                };
                let is_callback_container = table
                    .with_string(container.key.normal, |key| {
                        key.eq_ignore_ascii_case("effects") || key.eq_ignore_ascii_case("triggers")
                    })
                    .unwrap_or(false);
                if !is_callback_container {
                    continue;
                }
                for callback_child in container.children {
                    let Some(callback) = arena.keyed_clause(callback_child) else {
                        continue;
                    };
                    table.with_string(callback.key.normal, |key| {
                        let name = unquote(key);
                        if !name.is_empty() {
                            names.push(name.to_string());
                        }
                    });
                }
            }
        }
    }
    names
}

#[tracing::instrument(skip_all, name = "collect_type_instances")]
fn collect_type_instances_inner(
    ruleset: &RuleSet,
    file: &ParsedFile,
    logical_path: &str,
    table: &StringTable,
    subtype_hook: Option<SubtypeCollector>,
    mut subtype_instances: Option<&mut HashMap<String, Vec<TypeInstance>>>,
) -> HashMap<String, Vec<TypeInstance>> {
    let mut result: HashMap<String, Vec<TypeInstance>> = HashMap::new();

    let np = NormalizedPath::new(logical_path);
    for td in &ruleset.types {
        if !check_path_dir_norm(&td.path_options, &np) {
            continue;
        }

        let mut instances: Vec<TypeInstance> = Vec::new();

        if td.type_per_file {
            // Normalize separators before deriving the file name on Windows.
            let norm = logical_path.replace('\\', "/");
            let name = norm
                .rsplit('/')
                .next()
                .unwrap_or(norm.as_str())
                .trim_end_matches(".txt")
                .trim_end_matches(".gfx")
                .trim_end_matches(".gui")
                .to_string();
            instances.push(TypeInstance {
                name,
                location: SourceLocation {
                    line: 1,
                    col: 0,
                    end: (1, 0),
                },
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            });
        } else {
            let arena = &file.arena;
            let node_hook = if td.subtypes.is_empty() {
                None
            } else {
                subtype_hook
            };
            let mut visit = |td: &TypeDefinition,
                             name: String,
                             node_key: &str,
                             clause_children: &[Child],
                             location| {
                // Capture the explicit-field primary loc key (for example, an event's `title`).
                let primary_loc_key = primary_explicit_loc_field(td).and_then(|field| {
                    field_value_from_children(field, clause_children, arena, table)
                });
                // Capture required localisation keys from their declared child fields too.
                let required_loc_keys: Vec<String> = required_explicit_loc_fields(td)
                    .filter_map(|field| {
                        field_value_from_children(field, clause_children, arena, table)
                    })
                    .collect();
                if let (Some(hook), Some(out)) = (node_hook, subtype_instances.as_deref_mut()) {
                    let node = InstanceNode {
                        td,
                        name: &name,
                        node_key,
                        children: clause_children,
                        location,
                    };
                    hook(ruleset, file, &node, table, out);
                }
                instances.push(TypeInstance {
                    name,
                    location,
                    primary_loc_key,
                    required_loc_keys,
                });
            };
            for child in &file.root_children {
                walk_skip_root_child(td, &td.skip_root_key, child, arena, table, &mut visit);
            }
        }

        if !instances.is_empty() {
            result.entry(td.name.clone()).or_default().extend(instances);
        }
    }

    result
}

#[derive(Default)]
pub struct CollectedVariablesAndValueSets {
    pub variables: Vec<DefinedVariable>,
    pub value_sets: HashMap<String, Vec<String>>,
}

/// Collect a file's `set_variable`-style definitions and its value-set members
/// in one walk. `var_effects` names the variable-defining effects, and an empty
/// set means there are none. The definitions come in source order.
pub fn collect_variables_and_value_sets(
    file: &ParsedFile,
    ruleset: &RuleSet,
    table: &StringTable,
    var_effects: Option<&HashSet<String>>,
) -> CollectedVariablesAndValueSets {
    let var_effects = var_effects.filter(|effects| !effects.is_empty());
    let vs_active = !ruleset.value_set_effects().is_empty();
    let mut out = CollectedVariablesAndValueSets::default();
    if var_effects.is_some() || vs_active {
        walk_variables_and_value_sets(
            &file.root_children,
            file,
            ruleset,
            table,
            var_effects,
            vs_active,
            &mut out,
        );
    }
    out
}

fn walk_variables_and_value_sets(
    children: &[Child],
    file: &ParsedFile,
    ruleset: &RuleSet,
    table: &StringTable,
    var_effects: Option<&HashSet<String>>,
    vs_active: bool,
    out: &mut CollectedVariablesAndValueSets,
) {
    let arena = &file.arena;
    for child in children {
        let Child::Leaf(li) = child else { continue };
        let leaf = &arena.leaves[*li as usize];

        if let Some(effects) = var_effects
            && let Value::Clause(ch) = &leaf.value
        {
            let in_effects = table
                .with_string(leaf.key.normal, |k| {
                    effects.contains(k.to_ascii_lowercase().as_str())
                })
                .unwrap_or(false);
            if in_effects {
                crate::variables::extract_set_variable_defs_block(
                    ch,
                    arena,
                    table,
                    &mut out.variables,
                );
            }
        }

        // Value-set collection stops at variable-namespace blocks because variable definitions
        // use a separate collector; variable traversal can still proceed independently.
        let ns = if vs_active {
            dynamic_values::value_set_leaf(leaf, file, ruleset, table, &mut out.value_sets)
        } else {
            None
        };

        if let Value::Clause(ch) = &leaf.value {
            let is_var_ns = ns.as_deref() == Some("variable");
            let descend = var_effects.is_some() || (vs_active && !is_var_ns);
            let vs_child = vs_active && !is_var_ns;
            if descend {
                walk_variables_and_value_sets(ch, file, ruleset, table, var_effects, vs_child, out);
            }
        }
    }
}

pub fn index_discovered_files(
    files: impl IntoIterator<Item = cwtools_file_manager::file_manager::ParsedFile>,
    ruleset: &RuleSet,
    table: &StringTable,
    var_effects: Option<&HashSet<String>>,
    subtype_collector: Option<SubtypeCollector>,
) -> TypeIndex {
    use rayon::prelude::*;

    let files: Vec<cwtools_file_manager::file_manager::ParsedFile> = files.into_iter().collect();

    type PerFileData = (
        String,                             // path
        HashMap<String, Vec<TypeInstance>>, // base type instances
        HashMap<String, Vec<TypeInstance>>, // subtype membership
        Vec<String>,                        // variable names
        HashMap<String, Vec<String>>,       // complex enum values
        HashMap<String, Vec<String>>,       // value set members
        Vec<String>,                        // scripted-localisation names
        Vec<String>,                        // scripted-GUI callback names
    );
    let per_file: Vec<PerFileData> = files
        .into_par_iter()
        .map(|file| {
            let path = cwtools_file_manager::path_identity(&file.path).into_owned();
            // Workspace-scan files, always parsed on the base table.
            let pf = ParsedFile::new(file.arena, file.root_children, vec![]);
            let (instances, subtype_instances) = match subtype_collector {
                Some(hook) => {
                    let collected = collect_type_instances_with_subtypes(
                        ruleset,
                        &pf,
                        &file.logical_path,
                        table,
                        hook,
                    );
                    (collected.instances, collected.subtype_instances)
                }
                None => (
                    collect_type_instances(ruleset, &pf, &file.logical_path, table),
                    HashMap::new(),
                ),
            };
            let CollectedVariablesAndValueSets {
                variables,
                value_sets,
            } = collect_variables_and_value_sets(&pf, ruleset, table, var_effects);
            let var_names = variables.into_iter().map(|d| d.name).collect();
            let complex = dynamic_values::collect_complex_enum_values(
                ruleset,
                &pf,
                &file.logical_path,
                table,
            );
            let scripted_locs = collect_scripted_loc_names(&pf, &file.logical_path, table);
            let scripted_guis = collect_scripted_gui_callback_names(&pf, &file.logical_path, table);
            (
                path,
                instances,
                subtype_instances,
                var_names,
                complex,
                value_sets,
                scripted_locs,
                scripted_guis,
            )
        })
        .collect();

    let mut index = TypeIndex::new();
    for (
        path,
        instances,
        subtype_instances,
        var_names,
        complex,
        value_sets,
        scripted_locs,
        scripted_guis,
    ) in per_file
    {
        index.merge(&path, instances);
        if !subtype_instances.is_empty() {
            index.merge(&path, subtype_instances);
        }
        for n in &var_names {
            index.var_index.add_name(n);
        }
        index.complex_enum_values.merge_file(&path, complex);
        index.value_set_values.merge_file(&path, value_sets);
        index.scripted_loc_index.merge_file(&path, scripted_locs);
        index.scripted_gui_index.merge_file(&path, scripted_guis);
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_parser::parser::parse_string;
    use cwtools_rules::rules_types::PathOptions;

    fn type_def(name: &str, path: &str) -> TypeDefinition {
        TypeDefinition {
            name: name.to_string(),
            name_field: None,
            path_options: PathOptions {
                paths: vec![path.to_string()],
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
        }
    }

    fn ruleset_with(td: TypeDefinition) -> RuleSet {
        let mut rs = RuleSet::new();
        rs.types.push(td);
        rs
    }

    fn names(result: &HashMap<String, Vec<TypeInstance>>, ty: &str) -> Vec<String> {
        let mut v: Vec<String> = result
            .get(ty)
            .map(|is| is.iter().map(|i| i.name.clone()).collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    #[test]
    fn key_prefix_filters_and_keeps_name_intact() {
        let source = "MY_thing = { } my_other = { } NOPE_thing = { }";
        let table = StringTable::new();
        let parsed = parse_string(source, &table);

        let mut td = type_def("thing", "common/things");
        td.key_prefix = Some("MY_".to_string());
        let rs = ruleset_with(td);

        let result = collect_type_instances(&rs, &parsed, "common/things/00_things.txt", &table);
        assert_eq!(names(&result, "thing"), vec!["MY_thing", "my_other"]);
    }

    #[test]
    fn no_key_prefix_collects_all() {
        let source = "MY_thing = { } NOPE_thing = { }";
        let table = StringTable::new();
        let parsed = parse_string(source, &table);

        let rs = ruleset_with(type_def("thing", "common/things"));

        let result = collect_type_instances(&rs, &parsed, "common/things/00_things.txt", &table);
        assert_eq!(names(&result, "thing"), vec!["MY_thing", "NOPE_thing"]);
    }

    #[cfg(unix)]
    #[cfg_attr(
        target_os = "macos",
        ignore = "macOS rejects non-UTF-8 filenames with EILSEQ"
    )]
    #[test]
    fn non_utf8_paths_keep_distinct_file_index_entries() {
        use std::os::unix::ffi::OsStringExt;

        let root = tempfile::tempdir().unwrap();
        let first_path = root
            .path()
            .join(std::ffi::OsString::from_vec(b"same\x80.txt".to_vec()));
        let second_path = root
            .path()
            .join(std::ffi::OsString::from_vec(b"same\x81.txt".to_vec()));
        assert_ne!(first_path, second_path);
        assert_eq!(
            first_path.file_name().unwrap().to_string_lossy(),
            second_path.file_name().unwrap().to_string_lossy(),
            "the fixture paths must collide under lossy display",
        );
        std::fs::write(&first_path, "first = { }").unwrap();
        std::fs::write(&second_path, "second = { }").unwrap();

        let table = StringTable::new();
        let first = parse_string("first = { }", &table);
        let second = parse_string("second = { }", &table);
        let files = [
            cwtools_file_manager::file_manager::ParsedFile {
                path: first_path.clone(),
                logical_path: "common/things/first.txt".into(),
                arena: first.arena,
                root_children: first.root_children,
                errors: first.errors,
            },
            cwtools_file_manager::file_manager::ParsedFile {
                path: second_path.clone(),
                logical_path: "common/things/second.txt".into(),
                arena: second.arena,
                root_children: second.root_children,
                errors: second.errors,
            },
        ];
        let mut ruleset = ruleset_with(type_def("thing", "common/things"));
        ruleset.reindex();
        let index = index_discovered_files(files, &ruleset, &table, None, None);

        let first_key = cwtools_file_manager::path_identity(&first_path);
        let second_key = cwtools_file_manager::path_identity(&second_path);
        assert_ne!(first_key, second_key);
        let first_names: Vec<_> = index
            .instances_in_file(&first_key)
            .into_iter()
            .map(|(_, instance)| instance.name.as_str())
            .collect();
        let second_names: Vec<_> = index
            .instances_in_file(&second_key)
            .into_iter()
            .map(|(_, instance)| instance.name.as_str())
            .collect();
        assert_eq!(first_names, ["first"]);
        assert_eq!(second_names, ["second"]);
    }
    #[test]
    fn instance_location_end_is_closing_brace() {
        let source = "thing_a = {\n    x = 1\n}";
        let table = StringTable::new();
        let parsed = parse_string(source, &table);

        let rs = ruleset_with(type_def("thing", "common/things"));
        let result = collect_type_instances(&rs, &parsed, "common/things/00_things.txt", &table);

        let inst = &result.get("thing").expect("thing instances")[0];
        assert_eq!(inst.name, "thing_a");
        assert_eq!((inst.location.line, inst.location.col), (1, 0));
        assert_eq!(
            inst.location.end,
            (3, 1),
            "end must point just past the closing brace on line 3"
        );
        assert_ne!(
            (inst.location.line, inst.location.col),
            inst.location.end,
            "a multi-line definition has a non-degenerate span"
        );
    }

    // ── Scripted localisations, collected by path (#348) ──────────────────────

    const DEFINED_TEXT: &str = r#"
defined_text = {
	name = TUR_PKK_bases_name
	text = { localization_key = a_key }
}
defined_text = {
	name = "Western_Autocracy_L"
	text = { localization_key = b_key }
}
"#;

    fn scripted_locs_at(path: &str) -> Vec<String> {
        let table = StringTable::new();
        let parsed = parse_string(DEFINED_TEXT, &table);
        collect_scripted_loc_names(&parsed, path, &table)
    }

    #[test]
    fn scripted_loc_names_come_from_the_folder() {
        assert_eq!(
            scripted_locs_at("common/scripted_localisation/99_TUR.txt"),
            vec!["TUR_PKK_bases_name", "Western_Autocracy_L"],
            "the name field is the instance name, and quotes are stripped"
        );
    }

    #[test]
    fn scripted_loc_folder_spellings_all_match() {
        for dir in [
            "common/scripted_localisation",
            "common/scripted_localization",
            "common/scripted_loc",
        ] {
            assert_eq!(
                scripted_locs_at(&format!("{dir}/defs.txt")).len(),
                2,
                "{dir} must be recognised"
            );
        }
    }

    #[test]
    fn scripted_loc_matches_whole_segments_only() {
        for path in [
            "common/ideas/99_TUR_scripted_localization.txt",
            "common/scripted_localisations/defs.txt",
            "common/scripted_effects/00_effects.txt",
        ] {
            assert!(
                scripted_locs_at(path).is_empty(),
                "{path} must not be read as a scripted-loc folder"
            );
        }
    }

    #[test]
    fn scripted_loc_folder_nested_under_dlc_matches() {
        assert_eq!(
            scripted_locs_at("dlc/dlc042/common/scripted_localisation/defs.txt").len(),
            2
        );
    }

    #[test]
    fn scripted_loc_skips_a_definition_with_no_name() {
        let table = StringTable::new();
        let parsed = parse_string("defined_text = { text = { localization_key = a } }", &table);
        assert!(
            collect_scripted_loc_names(&parsed, "common/scripted_localisation/x.txt", &table)
                .is_empty()
        );
    }

    #[test]
    fn scripted_gui_collects_only_direct_callback_keys() {
        let source = r#"
scripted_gui = {
    outer_gui = {
        effects = {
            First_Click = { nested_effect = { unrelated = yes } }
        }
        triggers = {
            SECOND_ENABLED = { nested_trigger = yes }
        }
        visible = { unrelated_visible_key = yes }
        properties = { effects = { unrelated_nested_key = { } } }
    }
}
unrelated_root = {
    fake_gui = { effects = { unrelated_root_callback = { } } }
}
"#;
        let table = StringTable::new();
        let parsed = parse_string(source, &table);
        let names = collect_scripted_gui_callback_names(
            &parsed,
            "common/scripted_guis/example.txt",
            &table,
        );

        assert_eq!(names, vec!["First_Click", "SECOND_ENABLED"]);
    }

    #[test]
    fn scripted_gui_matches_nested_dlc_path_but_not_unrelated_folders() {
        let table = StringTable::new();
        let parsed = parse_string(
            "scripted_gui = { gui = { effects = { callback = { } } } }",
            &table,
        );
        assert_eq!(
            collect_scripted_gui_callback_names(
                &parsed,
                "dlc/dlc042/common/scripted_guis/example.txt",
                &table,
            ),
            vec!["callback"]
        );
        for path in [
            "common/scripted_gui/example.txt",
            "common/scripted_guis_extra/example.txt",
            "interface/scripted_guis/example.txt",
        ] {
            assert!(
                collect_scripted_gui_callback_names(&parsed, path, &table).is_empty(),
                "{path} must not be read as the scripted-GUI folder"
            );
        }
    }

    // ── Variables and value sets, one walk ────────────────────────────────────

    // The two separate walks the fused one replaced, kept as its reference.
    fn reference_variables(
        children: &[Child],
        arena: &Arena,
        table: &StringTable,
        effects: &HashSet<String>,
        out: &mut Vec<DefinedVariable>,
    ) {
        for child in children {
            let Child::Leaf(li) = child else { continue };
            let leaf = &arena.leaves[*li as usize];
            let Value::Clause(ch) = &leaf.value else {
                continue;
            };
            let in_effects = table
                .with_string(leaf.key.normal, |k| {
                    effects.contains(k.to_ascii_lowercase().as_str())
                })
                .unwrap_or(false);
            if in_effects {
                crate::variables::extract_set_variable_defs_block(ch, arena, table, out);
            }
            reference_variables(ch, arena, table, effects, out);
        }
    }

    fn reference_value_sets(
        children: &[Child],
        file: &ParsedFile,
        ruleset: &RuleSet,
        table: &StringTable,
        out: &mut HashMap<String, Vec<String>>,
    ) {
        for child in children {
            let Child::Leaf(li) = child else { continue };
            let leaf = &file.arena.leaves[*li as usize];
            let ns = dynamic_values::value_set_leaf(leaf, file, ruleset, table, out);
            if let Value::Clause(sub) = &leaf.value
                && ns.as_deref() != Some("variable")
            {
                reference_value_sets(sub, file, ruleset, table, out);
            }
        }
    }

    type VariableRow = (String, Option<String>, Option<String>, u32, u16, (u32, u16));

    fn variable_rows(defs: &[DefinedVariable]) -> Vec<VariableRow> {
        defs.iter()
            .map(|d| {
                let SourceLocation { line, col, end } = d.location;
                (
                    d.name.clone(),
                    d.namespace.clone(),
                    d.value.clone(),
                    line,
                    col,
                    end,
                )
            })
            .collect()
    }

    #[test]
    fn fused_walk_matches_the_separate_walks() {
        use crate::variable_defining_effects;
        use cwtools_rules::rules_converter::ast_to_ruleset;

        const RULES: &str = r#"
alias[effect:set_variable] = {
    var = value_set[variable]
    value = int_variable_field
}
alias[effect:set_temp_variable] = {
    value_set[variable] = int_variable_field
}
alias[effect:set_country_flag] = value_set[country_flag]
alias[effect:set_country_flag] = {
    flag = value_set[country_flag]
    value = int
}
"#;
        const SCRIPT: &str = r#"
first = {
    set_country_flag = plain_flag
    set_variable = { var = outer_var value = 1 }
    set_temp_variable = { shorthand_var = 2 }
    set_country_flag = { flag = block_flag value = 3 }
    nested = {
        set_country_flag = nested_flag
        set_variable = { var = nested_var }
    }
}
second = {
    set_variable = {
        var = holder
        set_country_flag = hidden_flag
        wrapper = { set_variable = { var = deep_var } }
    }
    set_country_flag = { flag = flagged set_variable = { var = in_flag_block } }
}
"#;
        let table = StringTable::new();
        let with_value_sets = ast_to_ruleset(&parse_string(RULES, &table), &table);
        let without_value_sets = RuleSet::new();
        let file = parse_string(SCRIPT, &table);
        let effects = variable_defining_effects(&with_value_sets);
        let no_effects = HashSet::new();

        for ruleset in [&with_value_sets, &without_value_sets] {
            for var_effects in [Some(&effects), Some(&no_effects), None] {
                let fused = collect_variables_and_value_sets(&file, ruleset, &table, var_effects);

                let mut variables = Vec::new();
                if let Some(effects) = var_effects {
                    reference_variables(
                        &file.root_children,
                        &file.arena,
                        &table,
                        effects,
                        &mut variables,
                    );
                }
                let mut value_sets = HashMap::new();
                if !ruleset.value_set_effects().is_empty() {
                    reference_value_sets(
                        &file.root_children,
                        &file,
                        ruleset,
                        &table,
                        &mut value_sets,
                    );
                }

                assert_eq!(
                    variable_rows(&fused.variables),
                    variable_rows(&variables),
                    "variables, in order"
                );
                assert_eq!(fused.value_sets, value_sets, "value sets, in order");
            }
        }

        let both =
            collect_variables_and_value_sets(&file, &with_value_sets, &table, Some(&effects));
        let names: Vec<_> = both.variables.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "outer_var",
                "shorthand_var",
                "nested_var",
                "holder",
                "deep_var",
                "in_flag_block"
            ]
        );
        assert_eq!(
            both.value_sets["country_flag"],
            ["plain_flag", "block_flag", "nested_flag", "flagged"],
            "hidden_flag sits inside a variable-namespace block"
        );
    }
}
