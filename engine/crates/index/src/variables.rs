use cwtools_parser::ast::{Arena, Child, ParsedFile, SourceRange, Value};
use cwtools_rules::rules_types::{NewField, NewRule, RootRule, RuleSet, RuleType};
use cwtools_string_table::string_table::StringTable;
use std::collections::{HashMap, HashSet};

use crate::{
    NormalizedPath, SourceLocation, check_path_dir_norm, get_string_or_empty, leaf_value_string,
    with_leaf_value_str,
};

#[derive(Debug, Clone)]
pub struct DefinedVariable {
    pub name: String,
    pub namespace: Option<String>, // value_set namespace, if any
    pub location: SourceLocation,
    pub value: Option<String>,
}

pub fn collect_defined_variables_from_rules(
    ruleset: &RuleSet,
    file: &ParsedFile,
    logical_path: &str,
    table: &StringTable,
    at_vars: Option<Vec<DefinedVariable>>,
) -> HashMap<String, Vec<DefinedVariable>> {
    let mut result: HashMap<String, Vec<DefinedVariable>> = HashMap::new();

    match at_vars {
        Some(vars) if !vars.is_empty() => {
            result.insert("@".to_string(), vars);
        }
        _ => {
            collect_at_vars(&file.root_children, &file.arena, table, &mut result);
        }
    }

    let np = NormalizedPath::new(logical_path);
    let root_type_rules = ruleset.root_type_rules_idx();
    let mut applicable_rules: Vec<&[NewRule]> = Vec::new();
    for td in &ruleset.types {
        if !check_path_dir_norm(&td.path_options, &np) {
            continue;
        }
        let Some(root_indices) = root_type_rules.get(td.name.as_str()) else {
            continue;
        };
        for &root_index in root_indices {
            let Some(RootRule::TypeRule(_, (RuleType::NodeRule { rules, .. }, _))) =
                ruleset.root_rules.get(root_index)
            else {
                continue;
            };
            applicable_rules.push(rules.as_ref());
        }
    }
    if !applicable_rules.is_empty() {
        for child in &file.root_children {
            if let Some(root_clause) = file.arena.keyed_clause(child) {
                scan_children_for_varset(
                    root_clause.children,
                    &file.arena,
                    table,
                    &applicable_rules,
                    &mut result,
                );
            }
        }
    }

    result
}

fn collect_at_vars(
    children: &[Child],
    arena: &Arena,
    table: &StringTable,
    out: &mut HashMap<String, Vec<DefinedVariable>>,
) {
    for child in children {
        if let Child::Leaf(idx) = child {
            let leaf = &arena.leaves[*idx as usize];
            let is_at_var = table
                .with_string(leaf.key.normal, |k| k.starts_with('@'))
                .unwrap_or(false);
            if is_at_var {
                let key = get_string_or_empty(table, leaf.key.normal);
                let value = leaf_value_string(&leaf.value, table);
                out.entry("@".to_string())
                    .or_default()
                    .push(DefinedVariable {
                        name: key.clone(),
                        namespace: None,
                        location: SourceLocation {
                            line: leaf.pos.start.line,
                            col: leaf.pos.start.col,
                            end: (leaf.pos.end.line, leaf.pos.end.col),
                        },
                        value: (!value.is_empty()).then_some(value),
                    });
            }
            if let Value::Clause(ch) = &leaf.value {
                collect_at_vars(ch, arena, table, out);
            }
        }
    }
}

fn sibling_value_in_children(
    children: &[Child],
    arena: &Arena,
    table: &StringTable,
) -> Option<String> {
    for child in children {
        if let Child::Leaf(li) = child {
            let leaf = &arena.leaves[*li as usize];
            let is_value_key = table
                .with_string(leaf.key.normal, |k| {
                    ["value", "amount", "add"]
                        .iter()
                        .any(|w| k.eq_ignore_ascii_case(w))
                })
                .unwrap_or(false);
            if is_value_key {
                let v = leaf_value_string(&leaf.value, table);
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}

fn scan_children_for_varset(
    children: &[Child],
    arena: &Arena,
    table: &StringTable,
    rule_bodies: &[&[NewRule]],
    out: &mut HashMap<String, Vec<DefinedVariable>>,
) {
    // the assigned value lives in a sibling `value` leaf of the same block.
    // Computed lazily: most blocks never hit the arm that needs it.
    let sibling_value = std::cell::OnceCell::<Option<String>>::new();
    // Reuse these small work lists for siblings instead of allocating per AST
    // child. Their entries borrow from immutable rule bodies.
    let mut captures = Vec::<&str>::new();
    let mut nested_bodies = Vec::<&[NewRule]>::new();
    let mut key_captures = Vec::<&str>::new();
    let mut value_captures = Vec::<&str>::new();
    for child in children {
        if let Some(kc) = arena.keyed_clause(child) {
            captures.clear();
            nested_bodies.clear();
            let key_name = match table.with_string(kc.key.normal, |child_key| {
                for rules in rule_bodies {
                    for (rule_type, _) in *rules {
                        let RuleType::NodeRule { left, rules } = rule_type else {
                            continue;
                        };
                        match left {
                            NewField::VariableSetField(namespace) => {
                                if !child_key.is_empty() {
                                    captures.push(namespace.as_str());
                                }
                            }
                            NewField::SpecificField(expected)
                                if child_key.eq_ignore_ascii_case(expected) =>
                            {
                                nested_bodies.push(rules.as_ref());
                            }
                            NewField::SpecificField(_) => {}
                            _ => nested_bodies.push(rules.as_ref()),
                        }
                    }
                }
                (!captures.is_empty()).then(|| child_key.to_string())
            }) {
                Some(name) => name,
                None => {
                    // A missing interned key still follows generic node rules,
                    // as the previous recursive scan did with an empty key.
                    for rules in rule_bodies {
                        for (rule_type, _) in *rules {
                            if let RuleType::NodeRule { left, rules } = rule_type
                                && !matches!(
                                    left,
                                    NewField::VariableSetField(_) | NewField::SpecificField(_)
                                )
                            {
                                nested_bodies.push(rules.as_ref());
                            }
                        }
                    }
                    None
                }
            };
            if let Some(name) = key_name {
                let location = SourceLocation {
                    line: kc.pos.start.line,
                    col: kc.pos.start.col,
                    end: (kc.pos.end.line, kc.pos.end.col),
                };
                for &namespace in &captures {
                    out.entry(namespace.to_string())
                        .or_default()
                        .push(DefinedVariable {
                            name: name.clone(),
                            namespace: Some(namespace.to_string()),
                            location,
                            value: None,
                        });
                }
            }
            if !nested_bodies.is_empty() {
                scan_children_for_varset(kc.children, arena, table, &nested_bodies, out);
            }
            continue;
        }
        match child {
            Child::Leaf(li) => {
                let leaf = &arena.leaves[*li as usize];
                key_captures.clear();
                value_captures.clear();
                let key_name = table
                    .with_string(leaf.key.normal, |key| {
                        for rules in rule_bodies {
                            for (rule_type, _) in *rules {
                                match rule_type {
                                    RuleType::LeafRule {
                                        left: NewField::VariableSetField(namespace),
                                        ..
                                    } => key_captures.push(namespace.as_str()),
                                    RuleType::LeafRule {
                                        left: NewField::SpecificField(expected),
                                        right: NewField::VariableSetField(namespace),
                                    } if key.eq_ignore_ascii_case(expected) => {
                                        value_captures.push(namespace.as_str());
                                    }
                                    _ => {}
                                }
                            }
                        }
                        (!key_captures.is_empty()).then(|| key.to_string())
                    })
                    .flatten();
                if key_captures.is_empty() && value_captures.is_empty() {
                    continue;
                }
                let leaf_value = leaf_value_string(&leaf.value, table);
                let location = SourceLocation {
                    line: leaf.pos.start.line,
                    col: leaf.pos.start.col,
                    end: (leaf.pos.end.line, leaf.pos.end.col),
                };
                for &namespace in &key_captures {
                    if let Some(name) = key_name.as_ref() {
                        out.entry(namespace.to_string())
                            .or_default()
                            .push(DefinedVariable {
                                name: name.clone(),
                                namespace: Some(namespace.to_string()),
                                location,
                                value: (!leaf_value.is_empty()).then(|| leaf_value.clone()),
                            });
                    }
                }
                if !leaf_value.is_empty() {
                    for &namespace in &value_captures {
                        out.entry(namespace.to_string())
                            .or_default()
                            .push(DefinedVariable {
                                name: leaf_value.clone(),
                                namespace: Some(namespace.to_string()),
                                location,
                                value: sibling_value
                                    .get_or_init(|| {
                                        sibling_value_in_children(children, arena, table)
                                    })
                                    .clone(),
                            });
                    }
                }
            }
            // LeafValueRule(VariableSetField): a bare value inside a block is the
            // defined variable name (F# InfoService fLeafValue).
            Child::LeafValue(lvi) => {
                let lv = &arena.leaf_values[*lvi as usize];
                with_leaf_value_str(&lv.value, table, |val| {
                    if !val.is_empty() {
                        for rules in rule_bodies {
                            for (rule_type, _opts) in *rules {
                                if let RuleType::LeafValueRule {
                                    right: NewField::VariableSetField(ns),
                                } = rule_type
                                {
                                    out.entry(ns.clone()).or_default().push(DefinedVariable {
                                        name: val.to_string(),
                                        namespace: Some(ns.clone()),
                                        location: SourceLocation {
                                            line: lv.pos.start.line,
                                            col: lv.pos.start.col,
                                            end: (lv.pos.end.line, lv.pos.end.col),
                                        },
                                        value: None,
                                    });
                                }
                            }
                        }
                    }
                });
            }
            _ => {}
        }
    }
}

pub fn variable_defining_effects(ruleset: &RuleSet) -> HashSet<String> {
    fn is_var_set(f: &NewField) -> bool {
        matches!(f, NewField::VariableSetField(ns) if ns == "variable" || ns == "array")
    }
    fn defines(rule: &RuleType) -> bool {
        match rule {
            RuleType::LeafRule { left, right } => is_var_set(left) || is_var_set(right),
            RuleType::LeafValueRule { right } => is_var_set(right),
            RuleType::NodeRule { left, rules } => {
                is_var_set(left) || rules.iter().any(|(rt, _)| defines(rt))
            }
            RuleType::ValueClauseRule { rules } | RuleType::SubtypeRule { rules, .. } => {
                rules.iter().any(|(rt, _)| defines(rt))
            }
        }
    }
    let mut out = HashSet::new();
    for (name, (rule, _opts)) in &ruleset.aliases {
        if let Some((cat, key)) = name.split_once(':')
            && (cat == "effect" || cat == "trigger")
            && defines(rule)
        {
            out.insert(key.to_ascii_lowercase());
        }
    }
    out
}

/// For every block whose key is a variable-defining effect, the defined name is
pub fn collect_set_variable_names(
    file: &ParsedFile,
    table: &StringTable,
    effects: &HashSet<String>,
    out: &mut Vec<String>,
) {
    let mut defs = Vec::new();
    collect_set_variable_defs(file, table, effects, &mut defs);
    out.extend(defs.into_iter().map(|d| d.name));
}

/// location and, where the block provides one, its assigned value (the `value`
pub fn collect_set_variable_defs(
    file: &ParsedFile,
    table: &StringTable,
    effects: &HashSet<String>,
    out: &mut Vec<DefinedVariable>,
) {
    fn walk(
        children: &[Child],
        arena: &Arena,
        table: &StringTable,
        effects: &HashSet<String>,
        out: &mut Vec<DefinedVariable>,
    ) {
        for child in children {
            if let Child::Leaf(li) = child {
                let leaf = &arena.leaves[*li as usize];
                if let Value::Clause(ch) = &leaf.value {
                    let in_effects = table
                        .with_string(leaf.key.normal, |k| {
                            effects.contains(k.to_ascii_lowercase().as_str())
                        })
                        .unwrap_or(false);
                    if in_effects {
                        extract_set_variable_defs_block(ch, arena, table, out);
                    }
                    walk(ch, arena, table, effects, out);
                }
            }
        }
    }

    walk(&file.root_children, &file.arena, table, effects, out);
}

fn variable_def(name: String, value: Option<String>, pos: SourceRange) -> DefinedVariable {
    DefinedVariable {
        name,
        namespace: Some("variable".to_string()),
        location: SourceLocation {
            line: pos.start.line,
            col: pos.start.col,
            end: (pos.end.line, pos.end.col),
        },
        value,
    }
}

/// effect block (`set_variable = { ... }` and friends): the explicit
/// blocks stays with the caller.
pub(crate) fn extract_set_variable_defs_block(
    children: &[Child],
    arena: &Arena,
    table: &StringTable,
    out: &mut Vec<DefinedVariable>,
) {
    let mut explicit = false;
    let sibling_value = sibling_value_in_children(children, arena, table);
    for child in children {
        if let Child::Leaf(li) = child {
            let leaf = &arena.leaves[*li as usize];
            let is_var_key = table
                .with_string(leaf.key.normal, |k| {
                    k.eq_ignore_ascii_case("var")
                        || k.eq_ignore_ascii_case("variable")
                        || k.eq_ignore_ascii_case("array")
                })
                .unwrap_or(false);
            if is_var_key {
                let v = leaf_value_string(&leaf.value, table);
                if !v.is_empty() {
                    out.push(variable_def(v, sibling_value.clone(), leaf.pos));
                }
                explicit = true;
            }
        }
    }
    if explicit {
        return;
    }
    for child in children {
        let (key, value, pos) = match child {
            Child::Leaf(li) => {
                let leaf = &arena.leaves[*li as usize];
                let k = get_string_or_empty(table, leaf.key.normal);
                let v = leaf_value_string(&leaf.value, table);
                (k, (!v.is_empty()).then_some(v), leaf.pos)
            }
            _ => continue,
        };
        const SKIP_KEYS: &[&str] = &["value", "tooltip", "var", "variable", "amount", "which"];
        if !SKIP_KEYS.iter().any(|k| key.eq_ignore_ascii_case(k)) {
            out.push(variable_def(key, value, pos));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cwtools_parser::parser::parse_string;
    use cwtools_rules::rules_types::{Options, PathOptions, TypeDefinition};

    use super::*;

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

    fn node(left: NewField, rules: Vec<NewRule>) -> NewRule {
        (
            RuleType::NodeRule {
                left,
                rules: Arc::from(rules),
            },
            Options::default(),
        )
    }

    fn leaf(left: NewField, right: NewField) -> NewRule {
        (RuleType::LeafRule { left, right }, Options::default())
    }

    fn root_type(name: &str, nested: Vec<NewRule>) -> RootRule {
        RootRule::TypeRule(name.to_string(), node(NewField::ScalarField, nested))
    }

    #[test]
    fn matching_root_types_share_one_walk_and_keep_path_scoped_definitions() {
        let table = StringTable::new();
        let parsed = parse_string(
            "root = {\n\
                section_a = {\n\
                variable = leaf_name\n\
                array = { element_name = {} }\n\
                other = { bare_name }\n\
                }\n\
                section_b = { plain_name = {} }\n\
                section_c = { wrong_path_name = yes }\n\
            }\n",
            &table,
        );

        let variable = NewField::VariableSetField("variable".to_string());
        let array = NewField::VariableSetField("array".to_string());
        let mut rules = RuleSet::new();
        rules.types.extend([
            type_def("first", "common/test"),
            type_def("second", "common/test"),
            type_def("other_path", "common/elsewhere"),
        ]);
        rules.root_rules.extend([
            root_type(
                "first",
                vec![node(
                    NewField::SpecificField("section_a".to_string()),
                    vec![
                        leaf(
                            NewField::SpecificField("variable".to_string()),
                            variable.clone(),
                        ),
                        node(
                            NewField::SpecificField("array".to_string()),
                            vec![node(array.clone(), Vec::new())],
                        ),
                        node(
                            NewField::ScalarField,
                            vec![(
                                RuleType::LeafValueRule {
                                    right: variable.clone(),
                                },
                                Options::default(),
                            )],
                        ),
                    ],
                )],
            ),
            root_type(
                "second",
                vec![node(
                    NewField::SpecificField("section_b".to_string()),
                    vec![node(variable, Vec::new())],
                )],
            ),
            root_type(
                "other_path",
                vec![node(
                    NewField::SpecificField("section_c".to_string()),
                    vec![node(array, Vec::new())],
                )],
            ),
        ]);
        rules.reindex();

        assert_eq!(rules.root_type_rules_idx()["first"], [0]);
        assert_eq!(rules.root_type_rules_idx()["second"], [1]);

        let vars = collect_defined_variables_from_rules(
            &rules,
            &parsed,
            "common/test/example.txt",
            &table,
            None,
        );
        let mut definitions: Vec<_> = vars
            .iter()
            .flat_map(|(namespace, values)| {
                values
                    .iter()
                    .map(move |value| (namespace.as_str(), value.name.as_str()))
            })
            .collect();
        definitions.sort_unstable();
        assert_eq!(
            definitions,
            [
                ("array", "element_name"),
                ("variable", "bare_name"),
                ("variable", "leaf_name"),
                ("variable", "plain_name"),
            ]
        );
    }
}
