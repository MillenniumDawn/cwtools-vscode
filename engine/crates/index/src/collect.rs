//! Collecting type instances from parsed files, and building a [`TypeIndex`]
//! from discovered files.

use cwtools_parser::ast::{Arena, Child, ParsedFile, Value};
use cwtools_rules::rules_types::{RuleSet, SkipRootKey, TypeDefinition};
use cwtools_string_table::string_table::StringTable;
use std::collections::{HashMap, HashSet};

use crate::dynamic_values;
use crate::{
    DefinedVariable, NormalizedPath, SourceLocation, TypeIndex, TypeInstance, check_path_dir_norm,
    get_string_or_empty, leaf_value_string, unquote,
};

/// Does this `skip_root_key` rule match `key`? Case-insensitive (matching the
/// engine), and honours the `should_match` negation flag on `MultipleKeys`.
/// Shared with the validator (cwtools_validation::resolve) so indexing and
/// validation agree on which root keys to skip.
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
        // Paradox keys/prefixes are ASCII identifiers; an ASCII case-insensitive
        // prefix test matches `to_lowercase().starts_with(to_lowercase())` without
        // allocating a lowercased copy of either string per call.
        Some(prefix) => {
            key.len() >= prefix.len()
                && key.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
        }
    }
}

// F# `type_key_prefix` compares the type's prefix against a node's own KeyPrefix
// token from Imperator-style prefixed nodes (`prefix key = { .. }`), which this
// AST doesn't model. We take the conservative reading: the key must carry the
// declared prefix (ASCII case-insensitive, like `starts_with`), name unchanged.
fn key_prefix_matches(td: &TypeDefinition, key: &str) -> bool {
    match &td.key_prefix {
        None => true,
        Some(prefix) => {
            key.len() >= prefix.len()
                && key.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
        }
    }
}

/// The field name an instance's `## primary` localisation is taken from, when it
/// is an explicit field (e.g. an event's `title = title` → `Some("title")`).
/// `None` for name-derived (`$`-pattern) primary keys or types with no primary
/// localisation — those need nothing captured at index time.
fn primary_explicit_loc_field(td: &TypeDefinition) -> Option<&str> {
    td.localisation
        .iter()
        .find(|l| l.primary && l.explicit_field.is_some())
        .and_then(|l| l.explicit_field.as_deref())
}

/// The child fields a type's `## required` localisation entries read their key
/// from (`## required title = title`). Empty for the usual `$`-pattern types, so
/// the per-instance resolution below costs nothing for them.
fn required_explicit_loc_fields(td: &TypeDefinition) -> impl Iterator<Item = &str> {
    td.localisation
        .iter()
        .filter_map(|l| l.required_explicit_field())
}

/// Read the value of the child leaf whose key equals `field_name` (case-
/// insensitive), unquoted. The shared lookup behind `name_field` and primary
/// explicit-field localisation.
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

/// Extract the instance name from a clause-typed element (honours `name_field`).
/// `children` is the list of children inside the clause.
fn instance_name_from_children(
    td: &TypeDefinition,
    node_key: &str,
    children: &[Child],
    arena: &Arena,
    table: &StringTable,
) -> Option<String> {
    match &td.name_field {
        None => Some(unquote(node_key).to_string()),
        // The instance name comes from a child leaf whose key equals `name_field`.
        // Quoted values (e.g. spriteType `name = "GFX_x"`) are stored with their
        // quotes, so strip them to match unquoted references like `icon = GFX_x`.
        Some(field_name) => field_value_from_children(field_name, children, arena, table),
    }
}

/// Recurse through skip_root_key layers, then visit each matching instance node.
/// `child` is a single top-level child (must be a keyed clause); at the instance
/// node the key must pass the `type_key_filter` + `starts_with` gates and yield a
/// name, then `visit` is invoked with the resolved name (owned), the node's own
/// key, its clause children, and its location. The single skip-root-key navigator
/// behind both [`collect_type_instances`] (builds `TypeInstance`s) and
/// [`for_each_instance_node`] (invokes a caller callback); they differ only in
/// what `visit` does at the leaf.
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
            // We are at the instance node.
            if type_key_filter_matches(td, &key)
                && starts_with_matches(td, &key)
                && key_prefix_matches(td, &key)
                && let Some(name) =
                    instance_name_from_children(td, &key, clause_children, arena, table)
            {
                visit(td, name, &key, clause_children, location);
            }
        }
        [head, tail @ ..] => {
            // Must match the skip-root layer; then descend into children.
            if skip_root_key_matches(head, &key) {
                for inner_child in clause_children {
                    walk_skip_root_child(td, tail, inner_child, arena, table, visit);
                }
            }
        }
    }
}

/// One type *instance node* as seen during the type-instance walk: the matched
/// type, the resolved instance name, the node's own key, its clause children, and
/// its source location. Passed to a [`SubtypeCollector`] so the injected subtype
/// matcher can compute per-instance facts without re-navigating the file.
pub struct InstanceNode<'a> {
    pub td: &'a TypeDefinition,
    pub name: &'a str,
    pub node_key: &'a str,
    pub children: &'a [Child],
    pub location: SourceLocation,
}

/// A per-instance-node hook that appends a node's subtype-qualified membership
/// (`"type.subtype" -> instances`) into `out`. Implemented in the `validation`
/// crate (it needs the subtype matcher) and injected into
/// [`index_discovered_files`] so the index crate stays free of a validation
/// dependency. Called once per instance node of a subtype-declaring type during
/// [`collect_type_instances`]' own walk, so type-instance and subtype-membership
/// collection share a single navigation instead of two.
pub type SubtypeCollector =
    fn(&RuleSet, &ParsedFile, &InstanceNode, &StringTable, &mut HashMap<String, Vec<TypeInstance>>);

/// Per-file type instances collected from one AST navigation. Base instances
/// and subtype-qualified membership stay separate so callers that track exports
/// do not treat subtype membership as an additional exported symbol.
#[derive(Default)]
pub struct CollectedTypeInstances {
    pub instances: HashMap<String, Vec<TypeInstance>>,
    pub subtype_instances: HashMap<String, Vec<TypeInstance>>,
}

/// Visit every type *instance node* in `file` whose type declares subtypes,
/// invoking `f` with the matched type, the resolved instance name, the node's own
/// key, and the node's clause children. Mirrors [`collect_type_instances`]'s
/// navigation (path filter + skip_root_key + type_key_filter + name_field) but
/// exposes the node body so a caller can compute per-instance facts (e.g. which
/// subtypes are active).
///
/// Types with no subtypes are skipped: the sole purpose here is computing
/// subtype membership, so walking (and resolving the name of) instances that can
/// have no subtype facts is wasted work — and most types declare no subtypes, so
/// the skip avoids a second full instance navigation across the corpus on top of
/// [`collect_type_instances`].
///
/// `type_per_file` types are also skipped — the file *is* the instance, so there
/// is no node body to inspect for subtypes.
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

/// Hash one exported symbol's identity, with separators so distinct parts can't
/// run together (`a|bc` vs `ab|c`).
pub fn mix_export_symbol(parts: &[&str]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for p in parts {
        p.hash(&mut h);
        0xffu8.hash(&mut h);
    }
    h.finish()
}

/// Order-independent hash of a file's exported type instances, computed from the
/// per-file `type -> instances` map produced at index time. Mirrors the symbol
/// mixing used for variables/event targets in
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

/// Collect all type instances defined in `file` for the given `logical_path`,
/// applying skip_root_key navigation. Returns a map from type name to the list
/// of instances found in this file.
pub fn collect_type_instances(
    ruleset: &RuleSet,
    file: &ParsedFile,
    logical_path: &str,
    table: &StringTable,
) -> HashMap<String, Vec<TypeInstance>> {
    collect_type_instances_inner(ruleset, file, logical_path, table, None, None)
}

/// Collect base type instances and subtype-qualified membership in one walk.
/// The maps stay separate because subtype entries are not additional exported
/// symbols, even though both are merged into [`TypeIndex`] for reference lookup.
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

/// Directory segments a game keeps its scripted localisations in: HOI4 spells it
/// `scripted_localisation` (and tolerates the American spelling), the Jomini
/// games `scripted_loc`.
const SCRIPTED_LOC_DIRS: [&str; 3] = [
    "scripted_localisation",
    "scripted_localization",
    "scripted_loc",
];

/// The scripted-localisation names (`defined_text = { name = X }`) defined in
/// `file`, or an empty vec when its path is not a scripted-loc folder.
///
/// Read straight off the file rather than through a ruleset type: the HOI4
/// config points `type[scripted_loc]` at Stellaris's `game/common/scripted_loc`,
/// so nothing under `common/scripted_localisation` is ever typed and every use
/// of one read as an unknown command (#348). The node key is not checked — HOI4
/// uses `defined_text`, and a game that spells it differently still names the
/// definition in a `name` field.
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

/// The direct callback keys under each scripted GUI's `effects` and `triggers`
/// containers, or an empty vec outside `common/scripted_guis`.
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

/// Shared implementation for base collection and the fused subtype path.
/// When `subtype_hook` is present, each instance node of a subtype-declaring
/// type invokes it during the same skip-root navigation. Per-key instance order
/// remains identical to the separate walks.
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
        // Path filter (mirrors CheckPathDir)
        if !check_path_dir_norm(&td.path_options, &np) {
            continue;
        }

        let mut instances: Vec<TypeInstance> = Vec::new();

        if td.type_per_file {
            // The file itself is the instance; the name is the file stem.
            // Normalise separators first: the LSP on Windows derives logical
            // paths with backslashes (`check_path_dir` already normalises, this
            // must too), else the stem becomes the whole path and references
            // like `load_oob = "MY_OOB"` flag as false positives.
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
                // The file itself is the instance: no single node span is the
                // definition, so a deliberately degenerate span marks it rather
                // than borrowing some root child's range (root_children IS in
                // scope, but any node's range would be a fabrication here).
                location: SourceLocation {
                    line: 1,
                    col: 0,
                    end: (1, 0),
                },
                // type_per_file types have no node body to read a field from.
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            });
            // `type_per_file` types carry no node body, so they contribute no
            // subtype membership (`for_each_instance_node` skips them too).
        } else {
            // Walk the file's top-level keyed clauses. A subtype-declaring type
            // additionally computes membership at each instance node via the
            // injected hook, sharing this one navigation.
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
                // Capture the explicit-field primary loc key (e.g. an event's
                // `title`) so hover can resolve the localised title cross-file.
                let primary_loc_key = primary_explicit_loc_field(td).and_then(|field| {
                    field_value_from_children(field, clause_children, arena, table)
                });
                // Same read for the `## required` explicit-field entries, whose
                // keys CW100 checks for existence.
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

/// Collect both set-variable names and `value_set[...]` members from one file in
/// a single pre-order walk, replacing the two separate whole-tree traversals
/// ([`crate::collect_set_variable_names`] +
/// [`dynamic_values::collect_value_set_members`]) that `index_discovered_files`
/// used to run back-to-back over the same arena.
///
/// The two collectors visit the identical set of nodes (every `Child::Leaf`,
/// descending into every clause value) and key off different rule structures, so
/// each node dispatches to both, writing to its own output. They differ in one
/// descent rule: the value-set walk does not look inside a `variable`-namespace
/// block (`set_variable = { .. }`), whose members are collected on the dedicated
/// variable path, whereas the set-variable walk does descend there. The fused
/// walk descends into every clause when the set-variable collector is active
/// (suppressing the value-set logic for that subtree via `vs_active`), and
/// otherwise descends exactly where the value-set walk would — reproducing both
/// walks' output byte-for-byte.
///
/// `var_effects` mirrors `collect_set_variable_names`' gate: `Some(non_empty)`
/// enables the set-variable collector, `None` disables it (the caller has already
/// filtered out an empty set).
fn collect_variables_and_value_sets(
    file: &ParsedFile,
    ruleset: &RuleSet,
    table: &StringTable,
    var_effects: Option<&HashSet<String>>,
    var_names_out: &mut Vec<String>,
    value_sets_out: &mut HashMap<String, Vec<String>>,
) {
    let vs_active = !ruleset.value_set_effects().is_empty();
    if var_effects.is_none() && !vs_active {
        return;
    }
    let mut var_defs: Vec<DefinedVariable> = Vec::new();
    walk_variables_and_value_sets(
        &file.root_children,
        file,
        ruleset,
        table,
        var_effects,
        vs_active,
        &mut var_defs,
        value_sets_out,
    );
    var_names_out.extend(var_defs.into_iter().map(|d| d.name));
}

#[allow(clippy::too_many_arguments)]
fn walk_variables_and_value_sets(
    children: &[Child],
    file: &ParsedFile,
    ruleset: &RuleSet,
    table: &StringTable,
    var_effects: Option<&HashSet<String>>,
    vs_active: bool,
    var_defs: &mut Vec<DefinedVariable>,
    value_sets: &mut HashMap<String, Vec<String>>,
) {
    let arena = &file.arena;
    for child in children {
        let Child::Leaf(li) = child else { continue };
        let leaf = &arena.leaves[*li as usize];

        // Set-variable collector: a clause whose key is a variable-defining effect
        // defines names in its body (mirrors `collect_set_variable_defs`' walk).
        if let Some(effects) = var_effects
            && let Value::Clause(ch) = &leaf.value
        {
            let in_effects = table
                .with_string(leaf.key.normal, |k| {
                    effects.contains(k.to_ascii_lowercase().as_str())
                })
                .unwrap_or(false);
            if in_effects {
                crate::variables::extract_set_variable_defs_block(ch, arena, table, var_defs);
            }
        }

        // Value-set collector: per-leaf member capture. A `variable`-namespace
        // block returns `ns == "variable"`, marking a subtree the value-set walk
        // must not descend into.
        let ns = if vs_active {
            dynamic_values::value_set_leaf(leaf, file, ruleset, table, value_sets)
        } else {
            None
        };

        if let Value::Clause(ch) = &leaf.value {
            let is_var_ns = ns.as_deref() == Some("variable");
            // The set-variable walk descends into every clause; the value-set walk
            // descends into every clause but a `variable`-namespace block.
            let descend = var_effects.is_some() || (vs_active && !is_var_ns);
            let vs_child = vs_active && !is_var_ns;
            if descend {
                walk_variables_and_value_sets(
                    ch,
                    file,
                    ruleset,
                    table,
                    var_effects,
                    vs_child,
                    var_defs,
                    value_sets,
                );
            }
        }
    }
}

/// Build a [`TypeIndex`] from already-discovered+parsed files. Shared by the CLI
/// (`index_game_dir`) and LSP (`index_vanilla_dir`) base-game indexing paths so
/// the per-file merge loop lives in one place. Each file's AST is consumed in
/// place (no re-parse) and its type instances are stream-merged.
///
/// When `var_effects` is `Some(non_empty)`, base-game variable definitions are
/// also folded into `index.var_index` (so a mod referencing a vanilla variable
/// isn't flagged as unset, CW246). Pass `None` to skip variable collection.
///
/// When `subtype_collector` is `Some`, each file's subtype-qualified membership
/// (`"type.subtype" -> instances`) is also merged, so `<type.subtype>` references
/// into base-game content resolve. The collector lives in the `validation` crate
/// (it needs the subtype matcher); see [`SubtypeCollector`].
pub fn index_discovered_files(
    files: impl IntoIterator<Item = cwtools_file_manager::file_manager::ParsedFile>,
    ruleset: &RuleSet,
    table: &StringTable,
    var_effects: Option<&HashSet<String>>,
    subtype_collector: Option<SubtypeCollector>,
) -> TypeIndex {
    use rayon::prelude::*;

    let var_effects = var_effects.filter(|e| !e.is_empty());

    // Collect into a Vec so rayon can split it across threads. The Vec is then
    // consumed by into_par_iter() so we don't need Clone on the AST types.
    let files: Vec<cwtools_file_manager::file_manager::ParsedFile> = files.into_iter().collect();

    // Parallel collection: all collector functions take only &-borrows of the
    // shared ruleset/table, so each file's work is independent. into_par_iter()
    // on a Vec preserves input order in the output Vec after collect().
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
            let path = file.path.to_str().unwrap_or("").to_string();
            let pf = ParsedFile {
                arena: file.arena,
                root_children: file.root_children,
                errors: vec![],
            };
            // Fused: type instances and subtype-qualified membership share one
            // skip-root walk. Keep the maps separate until merge so subtype
            // membership never affects base-instance bookkeeping.
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
            // Fused: set-variable names and value-set members share one pre-order
            // walk over the arena instead of two back-to-back traversals.
            let mut var_names: Vec<String> = Vec::new();
            let mut value_sets: HashMap<String, Vec<String>> = HashMap::new();
            collect_variables_and_value_sets(
                &pf,
                ruleset,
                table,
                var_effects,
                &mut var_names,
                &mut value_sets,
            );
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

    // Sequential merge in original file order — preserves TypeIndex.merge call
    // order so goto-def "first match" and refcount semantics are unchanged.
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

    // A type declaring `type_key_prefix` collects only prefixed keys (case-
    // insensitive), and the instance name keeps the prefix intact.
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

    // A type with no `type_key_prefix` is unaffected — every key is collected.
    #[test]
    fn no_key_prefix_collects_all() {
        let source = "MY_thing = { } NOPE_thing = { }";
        let table = StringTable::new();
        let parsed = parse_string(source, &table);

        let rs = ruleset_with(type_def("thing", "common/things"));

        let result = collect_type_instances(&rs, &parsed, "common/things/00_things.txt", &table);
        assert_eq!(names(&result, "thing"), vec!["MY_thing", "NOPE_thing"]);
    }

    // An instance's location spans its whole definition: the start is the key,
    // the end is the spot just past the closing brace (the parser's
    // `SourceRange.end`). Cleanup features (rename/delete a definition) need the
    // full extent, so a multi-line clause must record an end on the brace's line.
    #[test]
    fn instance_location_end_is_closing_brace() {
        // `}` is the last char, on line 3 col 0; the range end lands one past it.
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
        // A filename carrying the word is not the folder, and neither is a
        // longer directory that merely starts with it.
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
        // Same reason `path_contains_segment` exists: base-game content sits
        // under `dlc/<id>/…` and must still be found.
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
}
