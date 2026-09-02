// stripped to WHY-only — see git history for full docs
use cwtools_parser::ast::{Arena, Child, ParsedFile, Value};
use cwtools_rules::rules_types::RuleSet;
use cwtools_string_table::string_table::StringTable;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub use cwtools_index::vanilla_cache;
pub use cwtools_index::*;

mod position;
mod references;

pub use position::{PositionElement, ReferenceHint, element_at_position};
pub use references::ReferenceIndex;
use references::{TypeRefRule, build_type_ref_keys, collect_type_ref_uses};

#[derive(Debug, Clone, Default)]
pub struct FileInfo {
    pub type_definitions: HashMap<String, Vec<SourceLocation>>,
    pub type_references: HashMap<String, Vec<SourceLocation>>,
    pub defined_variables_ns: HashMap<String, Vec<DefinedVariable>>,
    pub defined_variables: HashMap<String, SourceLocation>,
    pub saved_event_targets_detailed: Vec<SavedEventTarget>,
    pub saved_event_targets: HashSet<String>,
    pub inline_scripts: HashMap<String, SourceLocation>,
    pub export_instances_hash: u64,
    pub export_loc_registry_hash: u64,
    pub export_instance_names: HashSet<String>,
}

pub struct InfoService {
    pub files: HashMap<String, FileInfo>,
    pub all_type_defs: HashMap<String, Vec<(String, SourceLocation)>>,
    pub type_index: Arc<TypeIndex>,
    pub event_target_counts: HashMap<String, usize>,
    pub variable_counts: HashMap<String, usize>,
    pub inline_script_counts: HashMap<String, usize>,
    var_effects: HashSet<String>,
    pub reference_index: ReferenceIndex,
    type_ref_keys: Option<HashMap<String, Vec<TypeRefRule>>>,
}

impl Default for InfoService {
    fn default() -> Self {
        Self::new()
    }
}

impl InfoService {
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
            all_type_defs: HashMap::new(),
            type_index: Arc::new(TypeIndex::new()),
            event_target_counts: HashMap::new(),
            variable_counts: HashMap::new(),
            inline_script_counts: HashMap::new(),
            var_effects: HashSet::new(),
            reference_index: ReferenceIndex::default(),
            type_ref_keys: None,
        }
    }

    pub fn update_ruleset_data(&mut self, effects: HashSet<String>) {
        self.var_effects = effects;
        self.type_ref_keys = None;
    }

    pub fn profile_summary(&self) -> String {
        let cross_file: usize = self.type_index.map.values().map(|v| v.len()).sum();
        format!(
            "info: {} files | type_index {} instances / {} types | {} vars | {} targets | {} type_defs",
            self.files.len(),
            cross_file,
            self.type_index.map.len(),
            self.variable_counts.len(),
            self.event_target_counts.len(),
            self.all_type_defs.len(),
        )
    }

    pub fn index_file(
        &mut self,
        uri: &str,
        ast: &ParsedFile,
        table: &StringTable,
        ruleset: &RuleSet,
    ) {
        self.index_file_with_path(uri, ast, table, ruleset, uri);
    }

    pub fn index_file_with_path(
        &mut self,
        uri: &str,
        ast: &ParsedFile,
        table: &StringTable,
        ruleset: &RuleSet,
        logical_path: &str,
    ) {
        let instances = collect_type_instances(ruleset, ast, logical_path, table);
        self.index_file_with_precomputed_instances(
            uri,
            ast,
            table,
            ruleset,
            logical_path,
            instances,
            HashMap::new(),
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn index_file_with_precomputed_instances(
        &mut self,
        uri: &str,
        ast: &ParsedFile,
        table: &StringTable,
        ruleset: &RuleSet,
        logical_path: &str,
        instances: HashMap<String, Vec<TypeInstance>>,
        subtype_instances: HashMap<String, Vec<TypeInstance>>,
    ) {
        let mut info = FileInfo::default();

        let type_names = ruleset.type_by_name();
        for child in &ast.root_children {
            Self::index_child_heuristic(child, &ast.arena, table, type_names, &mut info);
        }

        Arc::make_mut(&mut self.type_index)
            .complex_enum_values
            .merge_file(
                uri,
                cwtools_index::dynamic_values::collect_complex_enum_values(
                    ruleset,
                    ast,
                    logical_path,
                    table,
                ),
            );
        Arc::make_mut(&mut self.type_index)
            .value_set_values
            .merge_file(
                uri,
                cwtools_index::dynamic_values::collect_value_set_members(ruleset, ast, table),
            );

        let scripted_locs = cwtools_index::collect_scripted_loc_names(ast, logical_path, table);
        let scripted_guis =
            cwtools_index::collect_scripted_gui_callback_names(ast, logical_path, table);
        for name in &scripted_locs {
            info.export_loc_registry_hash = info
                .export_loc_registry_hash
                .wrapping_add(mix_export_symbol(&["l", &name.to_ascii_lowercase()]));
        }
        for name in &scripted_guis {
            info.export_loc_registry_hash = info
                .export_loc_registry_hash
                .wrapping_add(mix_export_symbol(&["g", &name.to_ascii_lowercase()]));
        }
        Arc::make_mut(&mut self.type_index)
            .scripted_loc_index
            .merge_file(uri, scripted_locs);
        Arc::make_mut(&mut self.type_index)
            .scripted_gui_index
            .merge_file(uri, scripted_guis);

        info.export_instances_hash = hash_instance_exports(&instances);
        info.export_instance_names = instances
            .values()
            .flat_map(|v| v.iter())
            .map(|inst| inst.name.to_ascii_lowercase())
            .collect();
        Arc::make_mut(&mut self.type_index).merge(uri, instances);
        if !subtype_instances.is_empty() {
            Arc::make_mut(&mut self.type_index).merge(uri, subtype_instances);
        }

        let at_vars: Vec<DefinedVariable> = info
            .defined_variables
            .iter()
            .map(|(name, loc)| DefinedVariable {
                name: name.clone(),
                namespace: None,
                location: *loc,
                value: None,
            })
            .collect();
        info.defined_variables_ns =
            collect_defined_variables_from_rules(ruleset, ast, logical_path, table, Some(at_vars));
        if !self.var_effects.is_empty() {
            let mut set_vars: Vec<DefinedVariable> = Vec::new();
            collect_set_variable_defs(ast, table, &self.var_effects, &mut set_vars);
            if !set_vars.is_empty() {
                info.defined_variables_ns
                    .entry("variable".to_string())
                    .or_default()
                    .extend(set_vars);
            }
        }
        for vars in info.defined_variables_ns.values() {
            for v in vars {
                info.defined_variables.insert(v.name.clone(), v.location);
            }
        }

        info.saved_event_targets.extend(
            info.saved_event_targets_detailed
                .iter()
                .map(|e| e.name.clone()),
        );

        for (type_name, locs) in &info.type_definitions {
            self.all_type_defs
                .entry(type_name.clone())
                .or_default()
                .extend(locs.iter().map(|l| (uri.to_string(), *l)));
        }
        for et in &info.saved_event_targets {
            *self.event_target_counts.entry(et.clone()).or_insert(0) += 1;
        }
        for (ns, vars) in &info.defined_variables_ns {
            for v in vars {
                *self.variable_counts.entry(v.name.clone()).or_insert(0) += 1;
                if ns != "@" {
                    Arc::make_mut(&mut self.type_index)
                        .var_index
                        .add_name(&v.name);
                }
            }
        }
        for script in info.inline_scripts.keys() {
            *self.inline_script_counts.entry(script.clone()).or_insert(0) += 1;
        }

        let type_ref_keys = self
            .type_ref_keys
            .get_or_insert_with(|| build_type_ref_keys(ruleset));
        if !type_ref_keys.is_empty() {
            let mut refs = Vec::new();
            collect_type_ref_uses(
                &ast.root_children,
                &ast.arena,
                table,
                type_ref_keys,
                ruleset,
                logical_path,
                &mut refs,
            );
            self.reference_index.merge(uri, refs);
        }

        self.files.insert(uri.to_string(), info);
    }

    pub fn export_fingerprint(&self, uri: &str) -> u64 {
        let Some(fi) = self.files.get(uri) else {
            return 0;
        };
        let mut acc: u64 = fi
            .export_instances_hash
            .wrapping_add(fi.export_loc_registry_hash);
        for (ns, vars) in &fi.defined_variables_ns {
            for v in vars {
                acc = acc.wrapping_add(mix_export_symbol(&["v", ns, &v.name]));
            }
        }
        for et in &fi.saved_event_targets {
            acc = acc.wrapping_add(mix_export_symbol(&["e", et]));
        }
        acc
    }

    pub fn export_names(&self, uri: &str) -> HashSet<String> {
        let mut names = HashSet::new();
        if let Some(fi) = self.files.get(uri) {
            names.extend(fi.export_instance_names.iter().cloned());
            for vars in fi.defined_variables_ns.values() {
                for v in vars {
                    names.insert(v.name.to_ascii_lowercase());
                }
            }
            for et in &fi.saved_event_targets {
                names.insert(et.to_ascii_lowercase());
            }
        }
        names
    }

    pub fn clear_file(&mut self, uri: &str) {
        if let Some(info) = self.files.remove(uri) {
            for type_name in info.type_definitions.keys() {
                if let Some(locs) = self.all_type_defs.get_mut(type_name) {
                    locs.retain(|(u, _)| u != uri);
                    if locs.is_empty() {
                        self.all_type_defs.remove(type_name);
                    }
                }
            }
            Arc::make_mut(&mut self.type_index).remove_file(uri);
            self.reference_index.remove_file(uri);
            for et in &info.saved_event_targets {
                if let Some(count) = self.event_target_counts.get_mut(et) {
                    *count -= 1;
                    if *count == 0 {
                        self.event_target_counts.remove(et);
                    }
                }
            }
            for (ns, vars) in &info.defined_variables_ns {
                for v in vars {
                    if let Some(count) = self.variable_counts.get_mut(&v.name) {
                        *count -= 1;
                        if *count == 0 {
                            self.variable_counts.remove(&v.name);
                        }
                    }
                    if ns != "@" {
                        Arc::make_mut(&mut self.type_index)
                            .var_index
                            .remove_name(&v.name);
                    }
                }
            }
            for script in info.inline_scripts.keys() {
                if let Some(count) = self.inline_script_counts.get_mut(script) {
                    *count -= 1;
                    if *count == 0 {
                        self.inline_script_counts.remove(script);
                    }
                }
            }
        }
    }

    pub fn find_definitions(&self, name: &str) -> Option<&Vec<(String, SourceLocation)>> {
        self.all_type_defs.get(name)
    }

    pub fn find_variable_definitions(&self, name: &str) -> Vec<(String, SourceLocation)> {
        let mut out = Vec::new();
        for (uri, fi) in &self.files {
            for vars in fi.defined_variables_ns.values() {
                for v in vars {
                    if v.name.eq_ignore_ascii_case(name) {
                        out.push((uri.clone(), v.location));
                    }
                }
            }
        }
        out
    }

    pub fn variable_values(&self, name: &str, limit: usize) -> (Vec<String>, bool) {
        let mut values: Vec<String> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut truncated = false;
        for fi in self.files.values() {
            for vars in fi.defined_variables_ns.values() {
                for v in vars {
                    if !v.name.eq_ignore_ascii_case(name) {
                        continue;
                    }
                    if let Some(val) = &v.value
                        && seen.insert(val.as_str())
                    {
                        if values.len() >= limit {
                            truncated = true;
                        } else {
                            values.push(val.clone());
                        }
                    }
                }
            }
        }
        (values, truncated)
    }

    pub fn find_references(&self, name: &str) -> Option<Vec<(String, SourceLocation)>> {
        let mut result = Vec::new();
        for (uri, info) in &self.files {
            if let Some(locs) = info.type_references.get(name) {
                for loc in locs {
                    result.push((uri.clone(), *loc));
                }
            }
        }
        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    }

    fn index_child_heuristic<S: std::hash::BuildHasher>(
        child: &Child,
        arena: &Arena,
        table: &StringTable,
        type_names: &HashMap<String, usize, S>,
        info: &mut FileInfo,
    ) {
        if let Child::Leaf(idx) = child {
            let leaf = &arena.leaves[*idx as usize];
            let key = table.get_string(leaf.key.normal).unwrap_or_default();

            Self::record_top_level_key(leaf, &key, type_names, info);

            let value_str = leaf_value_string(&leaf.value, table);

            Self::record_type_reference(leaf, &value_str, info);

            if let Value::Clause(children) = &leaf.value {
                for c in children {
                    Self::index_child_heuristic(c, arena, table, type_names, info);
                }
            }

            Self::record_saved_event_target(leaf, &key, &value_str, info);
            Self::record_inline_script(leaf, arena, table, &key, info);

            Self::record_defined_variable(leaf, key, info);
        }
    }

    fn record_top_level_key<S: std::hash::BuildHasher>(
        leaf: &cwtools_parser::ast::Leaf,
        key: &str,
        type_names: &HashMap<String, usize, S>,
        info: &mut FileInfo,
    ) {
        if let Value::Clause(_) = &leaf.value
            && type_names.contains_key(key)
        {
            info.type_definitions
                .entry(key.to_string())
                .or_default()
                .push(SourceLocation {
                    line: leaf.pos.start.line,
                    col: leaf.pos.start.col,
                    end: (leaf.pos.end.line, leaf.pos.end.col),
                });
        }
    }

    fn record_type_reference(
        leaf: &cwtools_parser::ast::Leaf,
        value_str: &str,
        info: &mut FileInfo,
    ) {
        if value_str.starts_with('<') && value_str.ends_with('>') {
            let inner = &value_str[1..value_str.len() - 1];
            info.type_references
                .entry(inner.to_string())
                .or_default()
                .push(SourceLocation {
                    line: leaf.pos.start.line,
                    col: leaf.pos.start.col,
                    end: (leaf.pos.end.line, leaf.pos.end.col),
                });
        }
    }

    fn record_saved_event_target(
        leaf: &cwtools_parser::ast::Leaf,
        key: &str,
        value_str: &str,
        info: &mut FileInfo,
    ) {
        if key.starts_with("event_target:") {
            let target = key.strip_prefix("event_target:").unwrap_or("");
            if !target.is_empty() {
                info.saved_event_targets.insert(target.to_string());
            }
        }

        if (key == "save_event_target_as" || key == "save_global_event_target_as")
            && !value_str.is_empty()
        {
            info.saved_event_targets_detailed.push(SavedEventTarget {
                name: value_str.to_string(),
                location: SourceLocation {
                    line: leaf.pos.start.line,
                    col: leaf.pos.start.col,
                    end: (leaf.pos.end.line, leaf.pos.end.col),
                },
                is_global: key == "save_global_event_target_as",
            });
        }
    }

    fn record_inline_script(
        leaf: &cwtools_parser::ast::Leaf,
        arena: &Arena,
        table: &StringTable,
        key: &str,
        info: &mut FileInfo,
    ) {
        if key == "inline_script"
            && let Value::Clause(children) = &leaf.value
        {
            for c in children {
                if let Child::Leaf(script_idx) = c {
                    let script_leaf = &arena.leaves[*script_idx as usize];
                    let script_key = table.get_string(script_leaf.key.normal).unwrap_or_default();
                    if script_key == "script" {
                        let script_name = leaf_value_string(&script_leaf.value, table);
                        if !script_name.is_empty() {
                            info.inline_scripts.insert(
                                script_name,
                                SourceLocation {
                                    line: script_leaf.pos.start.line,
                                    col: script_leaf.pos.start.col,
                                    end: (script_leaf.pos.end.line, script_leaf.pos.end.col),
                                },
                            );
                        }
                    }
                }
            }
        }
    }

    fn record_defined_variable(leaf: &cwtools_parser::ast::Leaf, key: String, info: &mut FileInfo) {
        if key.starts_with('@') {
            info.defined_variables.insert(
                key,
                SourceLocation {
                    line: leaf.pos.start.line,
                    col: leaf.pos.start.col,
                    end: (leaf.pos.end.line, leaf.pos.end.col),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_parser::parser::parse_string;
    use cwtools_rules::rules_types::{PathOptions, SkipRootKey, TypeDefinition};

    fn empty_type_def(name: &str, paths: Vec<&str>) -> TypeDefinition {
        TypeDefinition {
            name: name.to_string(),
            name_field: None,
            path_options: PathOptions {
                paths: paths.into_iter().map(|s| s.to_string()).collect(),
                path_strict: false,
                path_file: None,
                path_extension: None,
                paths_lower: Vec::new(),
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

    fn make_ruleset_with_type(td: TypeDefinition) -> RuleSet {
        let mut rs = RuleSet::new();
        rs.types.push(td);
        rs
    }

    fn make_info_heuristic(source: &str) -> (FileInfo, StringTable) {
        let table = StringTable::new();
        let parsed = parse_string(source, &table);
        let mut info = FileInfo::default();
        let type_names = HashMap::new();
        for child in &parsed.root_children {
            InfoService::index_child_heuristic(
                child,
                &parsed.arena,
                &table,
                &type_names,
                &mut info,
            );
        }
        (info, table)
    }

    #[test]
    fn test_defined_variables() {
        let source = "@my_var = 5\nfoo = { bar = @my_var }";
        let (info, _) = make_info_heuristic(source);
        assert!(info.defined_variables.contains_key("@my_var"));
    }

    #[test]
    fn test_type_references() {
        let source = "create_country = { ethos = <ethos> }";
        let (info, _) = make_info_heuristic(source);
        assert!(info.type_references.contains_key("ethos"));
    }

    #[test]
    fn test_event_targets() {
        let source = "event_target:my_target = { foo = bar }";
        let (info, _) = make_info_heuristic(source);
        assert!(info.saved_event_targets.contains("my_target"));
    }

    #[test]
    fn test_inline_scripts() {
        let source = "inline_script = { script = my_inline_script }";
        let (info, _) = make_info_heuristic(source);
        assert!(info.inline_scripts.contains_key("my_inline_script"));
    }

    #[test]
    fn test_type_instance_simple() {
        let source = "my_ethos = { tradition = foo }";
        let table = StringTable::new();
        let parsed = parse_string(source, &table);

        let td = empty_type_def("ethoses", vec!["common/ethics"]);
        let rs = make_ruleset_with_type(td);

        let result = collect_type_instances(&rs, &parsed, "common/ethics/00_ethics.txt", &table);
        let instances = result.get("ethoses").expect("should find ethoses");
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].name, "my_ethos");
    }

    #[test]
    fn test_type_instance_path_mismatch() {
        let source = "my_ethos = { tradition = foo }";
        let table = StringTable::new();
        let parsed = parse_string(source, &table);

        let td = empty_type_def("ethoses", vec!["common/ethics"]);
        let rs = make_ruleset_with_type(td);

        let result = collect_type_instances(&rs, &parsed, "events/my_events.txt", &table);
        assert!(result.get("ethoses").is_none_or(|v| v.is_empty()));
    }

    #[test]
    fn test_type_instance_skip_root_key() {
        let source = "technologies = { my_tech = { } another_tech = { } }";
        let table = StringTable::new();
        let parsed = parse_string(source, &table);

        let mut td = empty_type_def("technology", vec!["common/technologies"]);
        td.skip_root_key = vec![SkipRootKey::AnyKey];
        let rs = make_ruleset_with_type(td);

        let result =
            collect_type_instances(&rs, &parsed, "common/technologies/00_techs.txt", &table);
        let instances = result.get("technology").expect("should find technology");
        let names: Vec<&str> = instances.iter().map(|i| i.name.as_str()).collect();
        assert!(
            names.contains(&"my_tech"),
            "expected my_tech in {:?}",
            names
        );
        assert!(
            names.contains(&"another_tech"),
            "expected another_tech in {:?}",
            names
        );
    }

    #[test]
    fn test_type_instance_name_field() {
        let source = "some_event = { id = my_event_001 }";
        let table = StringTable::new();
        let parsed = parse_string(source, &table);

        let mut td = empty_type_def("event", vec!["events"]);
        td.name_field = Some("id".to_string());
        let rs = make_ruleset_with_type(td);

        let result = collect_type_instances(&rs, &parsed, "events/my_events.txt", &table);
        let instances = result.get("event").expect("should find event");
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].name, "my_event_001");
    }

    #[test]
    fn test_type_instance_name_field_quoted() {
        let source = "spriteTypes = { spriteType = { name = \"GFX_test_icon\" } }";
        let table = StringTable::new();
        let parsed = parse_string(source, &table);

        let mut td = empty_type_def("spriteType", vec!["game/interface"]);
        td.name_field = Some("name".to_string());
        td.skip_root_key = vec![SkipRootKey::SpecificKey("spriteTypes".to_string())];
        let rs = make_ruleset_with_type(td);

        let result = collect_type_instances(&rs, &parsed, "game/interface/x.gfx", &table);
        let instances = result.get("spriteType").expect("should find spriteType");
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].name, "GFX_test_icon");
    }

    #[test]
    fn test_type_per_file_backslash_path() {
        let source = "MY_OOB = { y = yes }\n";
        let table = StringTable::new();
        let parsed = parse_string(source, &table);

        let mut td = empty_type_def("oob", vec!["history/units"]);
        td.type_per_file = true;
        let rs = make_ruleset_with_type(td);

        let result = collect_type_instances(&rs, &parsed, "history\\units\\MY_OOB.txt", &table);
        let instances = result.get("oob").expect("should find oob");
        assert_eq!(instances.len(), 1);
        assert_eq!(
            instances[0].name, "MY_OOB",
            "type_per_file name must be the file stem, got {:?}",
            instances[0].name
        );
    }

    #[test]
    fn test_type_instance_key_filter() {
        let source = "country_event = { id = foo }\nsome_other = { id = bar }";
        let table = StringTable::new();
        let parsed = parse_string(source, &table);

        let mut td = empty_type_def("event", vec!["events"]);
        td.type_key_filter = Some((vec!["country_event".to_string()], false));
        td.name_field = Some("id".to_string());
        let rs = make_ruleset_with_type(td);

        let result = collect_type_instances(&rs, &parsed, "events/test.txt", &table);
        let instances = result.get("event").expect("should find event");
        let names: Vec<&str> = instances.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"foo"), "should have foo: {:?}", names);
        assert!(!names.contains(&"bar"), "should not have bar: {:?}", names);
    }

    #[test]
    fn test_type_index_contains() {
        let mut idx = TypeIndex::new();
        let mut map = HashMap::new();
        map.insert(
            "event".to_string(),
            vec![TypeInstance {
                name: "my_event".to_string(),
                location: SourceLocation {
                    line: 1,
                    col: 0,
                    end: (1, 0),
                },
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            }],
        );
        idx.merge("file://test.txt", map);

        assert!(idx.contains("event", "my_event"));
        assert!(!idx.contains("event", "nonexistent"));
        assert!(!idx.contains("other_type", "my_event"));
    }

    #[test]
    fn test_type_index_remove_file() {
        let mut idx = TypeIndex::new();
        let mut map = HashMap::new();
        map.insert(
            "event".to_string(),
            vec![TypeInstance {
                name: "ev1".to_string(),
                location: SourceLocation {
                    line: 1,
                    col: 0,
                    end: (1, 0),
                },
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            }],
        );
        idx.merge("file://a.txt", map.clone());
        idx.merge("file://b.txt", map);

        idx.remove_file("file://a.txt");
        assert!(idx.contains("event", "ev1"));

        idx.remove_file("file://b.txt");
        assert!(!idx.contains("event", "ev1"));
    }

    #[test]
    fn info_service_type_index_snapshot_is_copy_on_write() {
        let uri = "file://a.txt";
        let mut service = InfoService::new();
        service.files.insert(uri.to_string(), FileInfo::default());
        Arc::make_mut(&mut service.type_index).merge(
            uri,
            HashMap::from([(
                "event".to_string(),
                vec![TypeInstance {
                    name: "ev1".to_string(),
                    location: SourceLocation {
                        line: 1,
                        col: 0,
                        end: (1, 0),
                    },
                    primary_loc_key: None,
                    required_loc_keys: Vec::new(),
                }],
            )]),
        );
        let snapshot = Arc::clone(&service.type_index);

        service.clear_file(uri);

        assert!(snapshot.contains("event", "ev1"));
        assert!(!service.type_index.contains("event", "ev1"));
        assert!(!Arc::ptr_eq(&snapshot, &service.type_index));
    }

    #[test]
    fn test_is_any_instance_refcount() {
        let mut idx = TypeIndex::new();
        let mut map = HashMap::new();
        map.insert(
            "character".to_string(),
            vec![TypeInstance {
                name: "GER_some_char".to_string(),
                location: SourceLocation {
                    line: 1,
                    col: 0,
                    end: (1, 0),
                },
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            }],
        );
        idx.merge("file://a.txt", map.clone());
        idx.merge("file://b.txt", map);
        assert!(idx.is_any_instance("GER_some_char"));
        assert!(!idx.is_any_instance("unknown_name"));

        idx.remove_file("file://a.txt");
        assert!(idx.is_any_instance("GER_some_char"));

        idx.remove_file("file://b.txt");
        assert!(!idx.is_any_instance("GER_some_char"));
    }

    #[test]
    fn test_contains_case_insensitive() {
        let mut idx = TypeIndex::new();
        let mut map = HashMap::new();
        map.insert(
            "ai_behavior".to_string(),
            vec![TypeInstance {
                name: "LBA_ai_behavior".to_string(),
                location: SourceLocation {
                    line: 1,
                    col: 0,
                    end: (1, 0),
                },
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            }],
        );
        idx.merge("file://a.txt", map);
        assert!(idx.contains("ai_behavior", "LBA_AI_BEHAVIOR"));
        assert!(idx.contains("ai_behavior", "lba_ai_behavior"));
        assert!(idx.is_any_instance("LBA_AI_BEHAVIOR"));
        idx.remove_file("file://a.txt");
        assert!(!idx.contains("ai_behavior", "LBA_ai_behavior"));
        assert!(!idx.is_any_instance("lba_ai_behavior"));
    }

    #[test]
    fn test_at_vars_collected() {
        let source = "@min_manpower = 100\n@max_tech = 5";
        let table = StringTable::new();
        let parsed = parse_string(source, &table);

        let rs = RuleSet::new();
        let vars = collect_defined_variables_from_rules(&rs, &parsed, "", &table, None);
        let at_vars = vars.get("@").expect("should have @-namespace vars");
        let names: Vec<&str> = at_vars.iter().map(|v| v.name.as_str()).collect();
        assert!(names.contains(&"@min_manpower"));
        assert!(names.contains(&"@max_tech"));
    }

    #[test]
    fn test_saved_event_targets() {
        let source = "
effect = {
    save_event_target_as = my_target
    save_global_event_target_as = global_target
}";
        let table = StringTable::new();
        let parsed = parse_string(source, &table);
        let mut service = InfoService::new();
        let rs = RuleSet::new();
        service.index_file_with_path("test.txt", &parsed, &table, &rs, "");
        let fi = service.files.get("test.txt").expect("file indexed");
        let targets = &fi.saved_event_targets_detailed;

        let names: Vec<&str> = targets.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"my_target"),
            "missing my_target: {:?}",
            names
        );
        assert!(
            names.contains(&"global_target"),
            "missing global_target: {:?}",
            names
        );

        let global = targets.iter().find(|t| t.name == "global_target").unwrap();
        assert!(global.is_global);
        let local = targets.iter().find(|t| t.name == "my_target").unwrap();
        assert!(!local.is_global);
    }

    #[test]
    fn test_element_at_position_leaf() {
        let source = "foo = bar\n";
        let table = StringTable::new();
        let parsed = parse_string(source, &table);

        let element = element_at_position(&parsed, 1, 6, &table);
        match element {
            Some(PositionElement::Leaf { key, value }) => {
                assert_eq!(key, "foo");
                assert_eq!(value, "bar");
            }
            other => panic!("expected Leaf, got {:?}", other),
        }
    }

    #[test]
    fn test_collect_set_variable_names() {
        const RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = { alias_name[effect] = alias_match_left[effect] }
alias[effect:set_variable] = {
    var = value_set[variable]
    value = int_variable_field
}
alias[effect:set_temp_variable] = {
    value_set[variable] = int_variable_field
}
"#;
        use cwtools_rules::rules_converter::ast_to_ruleset;
        let table = StringTable::new();
        let parsed_cwt = parse_string(RULES, &table);
        let ruleset = ast_to_ruleset(&parsed_cwt, &table);

        let effects = variable_defining_effects(&ruleset);
        assert!(effects.contains("set_variable"), "got: {:?}", effects);
        assert!(effects.contains("set_temp_variable"), "got: {:?}", effects);

        let script = "foo = { set_variable = { var = my_explicit value = 3 } set_temp_variable = { my_shorthand = 5 } }";
        let parsed = parse_string(script, &table);
        let mut names = Vec::new();
        collect_set_variable_names(&parsed, &table, &effects, &mut names);
        assert!(
            names.contains(&"my_explicit".to_string()),
            "got: {:?}",
            names
        );
        assert!(
            names.contains(&"my_shorthand".to_string()),
            "got: {:?}",
            names
        );
        assert!(!names.contains(&"value".to_string()), "got: {:?}", names);
    }

    #[test]
    fn test_collect_array_variable_names() {
        const RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = { alias_name[effect] = alias_match_left[effect] }
alias[effect:add_to_array] = {
    array = value_set[array]
    value = int_variable_field
}
alias[effect:resize_array] = {
    array = value_set[array]
    size = int_variable_field
}
"#;
        use cwtools_rules::rules_converter::ast_to_ruleset;
        let table = StringTable::new();
        let ruleset = ast_to_ruleset(&parse_string(RULES, &table), &table);

        let effects = variable_defining_effects(&ruleset);
        assert!(effects.contains("add_to_array"), "got: {:?}", effects);
        assert!(effects.contains("resize_array"), "got: {:?}", effects);

        let script = "foo = { add_to_array = { array = my_arr value = 3 } resize_array = { array = other_arr size = 2 } }";
        let parsed = parse_string(script, &table);
        let mut names = Vec::new();
        collect_set_variable_names(&parsed, &table, &effects, &mut names);
        assert!(names.contains(&"my_arr".to_string()), "got: {:?}", names);
        assert!(names.contains(&"other_arr".to_string()), "got: {:?}", names);
        assert!(!names.contains(&"array".to_string()), "got: {:?}", names);
    }

    #[test]
    fn test_collect_set_variable_defs_values() {
        use cwtools_rules::rules_converter::ast_to_ruleset;
        const RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = { alias_name[effect] = alias_match_left[effect] }
alias[effect:set_variable] = {
    var = value_set[variable]
    value = int_variable_field
}
alias[effect:set_temp_variable] = {
    value_set[variable] = int_variable_field
}
"#;
        let table = StringTable::new();
        let ruleset = ast_to_ruleset(&parse_string(RULES, &table), &table);
        let effects = variable_defining_effects(&ruleset);

        let script = "foo = { set_variable = { var = my_explicit value = 3 } set_temp_variable = { my_shorthand = 5 } }";
        let parsed = parse_string(script, &table);
        let mut defs = Vec::new();
        collect_set_variable_defs(&parsed, &table, &effects, &mut defs);

        let explicit = defs
            .iter()
            .find(|d| d.name == "my_explicit")
            .expect("my_explicit");
        assert_eq!(explicit.value.as_deref(), Some("3"));
        let shorthand = defs
            .iter()
            .find(|d| d.name == "my_shorthand")
            .expect("my_shorthand");
        assert_eq!(shorthand.value.as_deref(), Some("5"));
    }

    #[test]
    fn ruleset_change_invalidates_type_reference_keys() {
        use cwtools_rules::rules_converter::ast_to_ruleset;

        let table = StringTable::new();
        let old_rules = ast_to_ruleset(
            &parse_string(
                "types = { type[decision] = { path = \"common/decisions\" } }\
                 decision = { old_ref = <focus> }",
                &table,
            ),
            &table,
        );
        let new_rules = ast_to_ruleset(
            &parse_string(
                "types = { type[decision] = { path = \"common/decisions\" } }\
                 decision = { new_ref = <focus> }",
                &table,
            ),
            &table,
        );
        assert_eq!(old_rules.root_rules.len(), new_rules.root_rules.len());
        assert_eq!(old_rules.types.len(), new_rules.types.len());

        let mut service = InfoService::new();
        service.index_file_with_path(
            "old.txt",
            &parse_string("test = { old_ref = OLD }", &table),
            &table,
            &old_rules,
            "common/decisions/old.txt",
        );

        service.update_ruleset_data(HashSet::new());
        service.index_file_with_path(
            "new.txt",
            &parse_string("test = { new_ref = NEW }", &table),
            &table,
            &new_rules,
            "common/decisions/new.txt",
        );
        assert_eq!(service.reference_index.references("focus", "NEW").len(), 1);
    }

    #[test]
    fn value_set_var_cleared_on_file_clear() {
        let mut svc = InfoService::new();
        let uri = "file://test.txt";

        let mut file_info = FileInfo::default();
        file_info.defined_variables.insert(
            "my_var".to_string(),
            cwtools_index::SourceLocation {
                line: 1,
                col: 0,
                end: (1, 0),
            },
        );
        file_info.defined_variables_ns.insert(
            "@".to_string(),
            vec![cwtools_index::DefinedVariable {
                name: "my_var".to_string(),
                namespace: None,
                location: cwtools_index::SourceLocation {
                    line: 1,
                    col: 0,
                    end: (1, 0),
                },
                value: None,
            }],
        );
        svc.files.insert(uri.to_string(), file_info);
        svc.variable_counts.insert("my_var".to_string(), 1);
        Arc::make_mut(&mut svc.type_index)
            .var_index
            .add_name("unrelated");

        assert!(svc.variable_counts.contains_key("my_var"));
        assert!(svc.type_index.var_index.contains("unrelated"));
        assert!(!svc.type_index.var_index.contains("my_var"));

        svc.clear_file(uri);
        assert!(
            !svc.variable_counts.contains_key("my_var"),
            "my_var should be gone after clear_file"
        );
        assert!(
            svc.type_index.var_index.contains("unrelated"),
            "@-namespace clear must not strip an unrelated var_index name"
        );
    }

    const SET_VARIABLE_RULES: &str = r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = { alias_name[effect] = alias_match_left[effect] }
alias[effect:set_variable] = {
    var = value_set[variable]
    value = int_variable_field
}
"#;

    fn set_variable_ruleset(table: &StringTable) -> RuleSet {
        use cwtools_rules::rules_converter::ast_to_ruleset;
        ast_to_ruleset(&parse_string(SET_VARIABLE_RULES, table), table)
    }

    fn index_set_variable(
        svc: &mut InfoService,
        uri: &str,
        var: &str,
        table: &StringTable,
        ruleset: &RuleSet,
    ) {
        let script = format!("foo = {{ set_variable = {{ var = {var} value = 3 }} }}");
        svc.index_file_with_path(
            uri,
            &parse_string(&script, table),
            table,
            ruleset,
            "game/common/foo/f.txt",
        );
    }

    #[test]
    fn var_index_tracks_set_variable_across_index_and_clear() {
        let table = StringTable::new();
        let ruleset = set_variable_ruleset(&table);
        let mut svc = InfoService::new();
        svc.update_ruleset_data(variable_defining_effects(&ruleset));
        index_set_variable(&mut svc, "f.txt", "my_explicit", &table, &ruleset);
        assert!(
            svc.type_index.var_index.contains("my_explicit"),
            "LSP index path must populate var_index"
        );

        svc.clear_file("f.txt");
        assert!(
            !svc.type_index.var_index.contains("my_explicit"),
            "clear_file must drop the name from var_index"
        );
    }

    #[test]
    fn var_index_contains_is_case_insensitive() {
        let table = StringTable::new();
        let ruleset = set_variable_ruleset(&table);
        let mut svc = InfoService::new();
        svc.update_ruleset_data(variable_defining_effects(&ruleset));
        index_set_variable(&mut svc, "f.txt", "My_Explicit", &table, &ruleset);
        assert!(svc.type_index.var_index.contains("my_explicit"));
        assert!(svc.type_index.var_index.contains("MY_EXPLICIT"));
    }

    #[test]
    fn var_index_ignores_at_vars() {
        let table = StringTable::new();
        let ruleset = set_variable_ruleset(&table);
        let mut svc = InfoService::new();
        svc.update_ruleset_data(variable_defining_effects(&ruleset));
        svc.index_file_with_path(
            "f.txt",
            &parse_string(
                "@my_at = 5\nfoo = { set_variable = { var = real_var value = 1 } }",
                &table,
            ),
            &table,
            &ruleset,
            "game/common/foo/f.txt",
        );
        assert!(svc.type_index.var_index.contains("real_var"));
        assert!(!svc.type_index.var_index.contains("@my_at"));
        assert!(!svc.type_index.var_index.contains("my_at"));
    }

    #[test]
    fn var_index_refcount_keeps_a_name_defined_in_another_file() {
        let table = StringTable::new();
        let ruleset = set_variable_ruleset(&table);
        let mut svc = InfoService::new();
        svc.update_ruleset_data(variable_defining_effects(&ruleset));
        index_set_variable(&mut svc, "a.txt", "shared_var", &table, &ruleset);
        index_set_variable(&mut svc, "b.txt", "shared_var", &table, &ruleset);
        svc.clear_file("a.txt");
        assert!(
            svc.type_index.var_index.contains("shared_var"),
            "clearing one file must keep a name still defined in another"
        );
        svc.clear_file("b.txt");
        assert!(!svc.type_index.var_index.contains("shared_var"));
    }

    #[test]
    fn var_index_reindex_replaces_a_files_names() {
        let table = StringTable::new();
        let ruleset = set_variable_ruleset(&table);
        let mut svc = InfoService::new();
        svc.update_ruleset_data(variable_defining_effects(&ruleset));
        index_set_variable(&mut svc, "f.txt", "old_var", &table, &ruleset);
        svc.clear_file("f.txt");
        index_set_variable(&mut svc, "f.txt", "new_var", &table, &ruleset);
        assert!(!svc.type_index.var_index.contains("old_var"));
        assert!(svc.type_index.var_index.contains("new_var"));
    }

    #[test]
    fn var_index_stays_empty_until_var_effects_are_set() {
        let table = StringTable::new();
        let ruleset = set_variable_ruleset(&table);
        let mut svc = InfoService::new();
        index_set_variable(&mut svc, "f.txt", "my_explicit", &table, &ruleset);
        assert!(
            !svc.type_index.var_index.contains("my_explicit"),
            "set_variable names are not collected before update_ruleset_data"
        );
    }

    #[test]
    fn reference_index_clear_file_removes_only_that_files_sites() {
        use cwtools_rules::rules_converter::ast_to_ruleset;
        let table = StringTable::new();
        let rules = ast_to_ruleset(
            &parse_string(
                "types = { type[decision] = { path = \"common/decisions\" } }\
                 decision = { my_ref = <focus> }",
                &table,
            ),
            &table,
        );
        let script = |t: &StringTable| parse_string("test = { my_ref = SHARED }", t);

        let mut svc = InfoService::new();
        svc.index_file_with_path(
            "a.txt",
            &script(&table),
            &table,
            &rules,
            "common/decisions/a.txt",
        );
        svc.index_file_with_path(
            "b.txt",
            &script(&table),
            &table,
            &rules,
            "common/decisions/b.txt",
        );
        assert_eq!(svc.reference_index.references("focus", "SHARED").len(), 2);

        svc.clear_file("a.txt");
        let refs = svc.reference_index.references("focus", "SHARED");
        assert_eq!(refs.len(), 1, "only b's site should remain");
        assert_eq!(refs[0].0.as_ref(), "b.txt");

        svc.index_file_with_path(
            "a.txt",
            &script(&table),
            &table,
            &rules,
            "common/decisions/a.txt",
        );
        assert_eq!(svc.reference_index.references("focus", "SHARED").len(), 2);
        svc.clear_file("never.txt");
        svc.clear_file("b.txt");
        svc.clear_file("a.txt");
        assert!(svc.reference_index.references("focus", "SHARED").is_empty());
    }

    #[test]
    fn dynamic_values_consistent_through_clear_file() {
        use cwtools_rules::rules_converter::ast_to_ruleset;
        let table = StringTable::new();
        let mut rules = ast_to_ruleset(
            &parse_string(
                "alias[effect:set_country_flag] = value_set[country_flag]",
                &table,
            ),
            &table,
        );
        rules.reindex();

        let mut svc = InfoService::new();
        svc.index_file_with_path(
            "f.txt",
            &parse_string("my_effect = { set_country_flag = my_flag }", &table),
            &table,
            &rules,
            "common/f.txt",
        );
        assert!(
            svc.type_index
                .value_set_values
                .contains("country_flag", "my_flag"),
            "value_set member must be collected on index"
        );

        svc.clear_file("f.txt");
        assert!(
            !svc.type_index
                .value_set_values
                .contains("country_flag", "my_flag"),
            "value_set member must be gone after clear_file"
        );
    }

    #[test]
    fn scripted_locs_tracked_across_index_and_clear() {
        use cwtools_rules::rules_converter::ast_to_ruleset;
        let table = StringTable::new();
        let mut rules = ast_to_ruleset(&parse_string("", &table), &table);
        rules.reindex();

        let mut svc = InfoService::new();
        svc.index_file_with_path(
            "f.txt",
            &parse_string("defined_text = { name = Western_Autocracy_L }", &table),
            &table,
            &rules,
            "common/scripted_localisation/f.txt",
        );
        assert!(
            svc.type_index
                .scripted_loc_index
                .contains("western_autocracy_l"),
            "the LSP index path must collect scripted localisations"
        );

        svc.clear_file("f.txt");
        assert!(
            svc.type_index.scripted_loc_index.is_empty(),
            "clear_file must drop the file's names"
        );
    }

    #[test]
    fn scripted_gui_callbacks_tracked_across_reindex_and_clear() {
        use cwtools_rules::rules_converter::ast_to_ruleset;
        let table = StringTable::new();
        let mut rules = ast_to_ruleset(&parse_string("", &table), &table);
        rules.reindex();
        let mut svc = InfoService::new();

        svc.index_file_with_path(
            "f.txt",
            &parse_string(
                "scripted_gui = { gui = { effects = { Old_Click = { } } } }",
                &table,
            ),
            &table,
            &rules,
            "common/scripted_guis/f.txt",
        );
        assert!(svc.type_index.scripted_gui_index.contains("old_click"));
        let old_fingerprint = svc.export_fingerprint("f.txt");
        assert_ne!(old_fingerprint, 0);
        assert!(svc.export_names("f.txt").is_empty());

        svc.clear_file("f.txt");
        svc.index_file_with_path(
            "f.txt",
            &parse_string(
                "scripted_gui = { gui = { triggers = { New_Enabled = { } } } }",
                &table,
            ),
            &table,
            &rules,
            "common/scripted_guis/f.txt",
        );
        assert!(!svc.type_index.scripted_gui_index.contains("old_click"));
        assert!(svc.type_index.scripted_gui_index.contains("new_enabled"));
        assert_ne!(svc.export_fingerprint("f.txt"), old_fingerprint);
        assert!(svc.export_names("f.txt").is_empty());

        svc.clear_file("f.txt");
        assert!(svc.type_index.scripted_gui_index.is_empty());
        assert_eq!(svc.export_fingerprint("f.txt"), 0);
    }
}
