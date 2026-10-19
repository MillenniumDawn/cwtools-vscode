use cwtools_parser::ast::{Arena, Child, ParsedFile, SourceRange, Value};
use cwtools_rules::rules_types::{NewField, NewRule, RootRule, RuleSet, RuleType};
use cwtools_string_table::string_table::StringTable;
use smallvec::SmallVec;
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

/// `at_vars` are the file's `@` definitions when the caller already walked for
/// them, even if there were none; `None` walks the file here.
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
        Some(_) => {}
        None => collect_at_vars(&file.root_children, &file.arena, table, &mut result),
    }

    let np = NormalizedPath::new(logical_path);
    let root_type_rules = ruleset.root_type_rules_idx();
    let mut applicable_rules: Vec<RuleBody<'_>> = Vec::new();
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
            let mut order = SmallVec::new();
            order.push(applicable_rules.len());
            applicable_rules.push(RuleBody {
                rules: rules.as_ref(),
                order,
            });
        }
    }
    if !applicable_rules.is_empty() {
        let mut ordered = Vec::new();
        for (root_clause_index, child) in file.root_children.iter().enumerate() {
            if let Some(root_clause) = file.arena.keyed_clause(child) {
                let root_clause_rules: Vec<_> = applicable_rules
                    .iter()
                    .map(|group| {
                        let mut order = group.order.clone();
                        order.push(root_clause_index);
                        RuleBody {
                            rules: group.rules,
                            order,
                        }
                    })
                    .collect();
                scan_children_for_varset(
                    root_clause.children,
                    &file.arena,
                    table,
                    &root_clause_rules,
                    &mut ordered,
                );
            }
        }
        ordered.sort_by(|a, b| a.order.cmp(&b.order));
        for definition in ordered {
            result
                .entry(definition.variable.namespace.clone().unwrap_or_default())
                .or_default()
                .push(definition.variable);
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

struct RuleBody<'a> {
    rules: &'a [NewRule],
    /// Legacy recursive traversal order: each parent rule completes before
    /// the next one, even though the shared AST is visited only once.
    order: SmallVec<[usize; 8]>,
}

struct OrderedDefinition {
    order: SmallVec<[usize; 8]>,
    variable: DefinedVariable,
}

enum NodeAction<'a> {
    Capture(&'a str, SmallVec<[usize; 8]>),
    Scan(&'a [NewRule], SmallVec<[usize; 8]>),
}

fn scan_children_for_varset(
    children: &[Child],
    arena: &Arena,
    table: &StringTable,
    rule_bodies: &[RuleBody<'_>],
    out: &mut Vec<OrderedDefinition>,
) {
    // the assigned value lives in a sibling `value` leaf of the same block.
    // Computed lazily: most blocks never hit the arm that needs it.
    let sibling_value = std::cell::OnceCell::<Option<String>>::new();
    // Reuse these small work lists for siblings instead of allocating per AST
    // child. Their entries borrow from immutable rule bodies.
    let mut actions = Vec::<NodeAction<'_>>::new();
    let mut nested_groups = Vec::<RuleBody<'_>>::new();
    let mut leaf_actions = Vec::<(&str, bool, SmallVec<[usize; 8]>)>::new();
    for (child_index, child) in children.iter().enumerate() {
        if let Some(kc) = arena.keyed_clause(child) {
            actions.clear();
            nested_groups.clear();
            let key_exists = table.with_string(kc.key.normal, |child_key| {
                for group in rule_bodies {
                    for (rule_index, (rule_type, _)) in group.rules.iter().enumerate() {
                        let RuleType::NodeRule { left, rules } = rule_type else {
                            continue;
                        };
                        match left {
                            NewField::VariableSetField(namespace) => {
                                if !child_key.is_empty() {
                                    let mut order = group.order.clone();
                                    order.extend([child_index, rule_index]);
                                    actions.push(NodeAction::Capture(namespace, order));
                                }
                            }
                            NewField::SpecificField(expected)
                                if child_key.eq_ignore_ascii_case(expected) =>
                            {
                                let mut order = group.order.clone();
                                order.extend([child_index, rule_index]);
                                actions.push(NodeAction::Scan(rules.as_ref(), order));
                            }
                            NewField::SpecificField(_) => {}
                            _ => {
                                let mut order = group.order.clone();
                                order.extend([child_index, rule_index]);
                                actions.push(NodeAction::Scan(rules.as_ref(), order));
                            }
                        }
                    }
                }
            });
            if key_exists.is_none() {
                // A missing interned key still follows generic node rules,
                // as the previous recursive scan did with an empty key.
                for group in rule_bodies {
                    for (rule_index, (rule_type, _)) in group.rules.iter().enumerate() {
                        if let RuleType::NodeRule { left, rules } = rule_type
                            && !matches!(
                                left,
                                NewField::VariableSetField(_) | NewField::SpecificField(_)
                            )
                        {
                            let mut order = group.order.clone();
                            order.extend([child_index, rule_index]);
                            actions.push(NodeAction::Scan(rules.as_ref(), order));
                        }
                    }
                }
            }
            for action in actions.drain(..) {
                match action {
                    NodeAction::Capture(namespace, order) => {
                        if let Some(name) = table.with_string(kc.key.normal, str::to_owned)
                            && !name.is_empty()
                        {
                            out.push(OrderedDefinition {
                                order,
                                variable: DefinedVariable {
                                    name,
                                    namespace: Some(namespace.to_string()),
                                    location: SourceLocation {
                                        line: kc.pos.start.line,
                                        col: kc.pos.start.col,
                                        end: (kc.pos.end.line, kc.pos.end.col),
                                    },
                                    value: None,
                                },
                            });
                        }
                    }
                    NodeAction::Scan(rules, order) => {
                        nested_groups.push(RuleBody { rules, order });
                    }
                }
            }
            if !nested_groups.is_empty() {
                scan_children_for_varset(kc.children, arena, table, &nested_groups, out);
            }
            continue;
        }
        match child {
            Child::Leaf(li) => {
                let leaf = &arena.leaves[*li as usize];
                leaf_actions.clear();
                let key_name = table
                    .with_string(leaf.key.normal, |key| {
                        for group in rule_bodies {
                            for (rule_index, (rule_type, _)) in group.rules.iter().enumerate() {
                                match rule_type {
                                    RuleType::LeafRule {
                                        left: NewField::VariableSetField(namespace),
                                        ..
                                    } => {
                                        let mut order = group.order.clone();
                                        order.extend([child_index, rule_index]);
                                        leaf_actions.push((namespace.as_str(), false, order));
                                    }
                                    RuleType::LeafRule {
                                        left: NewField::SpecificField(expected),
                                        right: NewField::VariableSetField(namespace),
                                    } if key.eq_ignore_ascii_case(expected) => {
                                        let mut order = group.order.clone();
                                        order.extend([child_index, rule_index]);
                                        leaf_actions.push((namespace.as_str(), true, order));
                                    }
                                    _ => {}
                                }
                            }
                        }
                        (!leaf_actions.is_empty()).then(|| key.to_string())
                    })
                    .flatten();
                if leaf_actions.is_empty() {
                    continue;
                }
                let leaf_value = leaf_value_string(&leaf.value, table);
                let location = SourceLocation {
                    line: leaf.pos.start.line,
                    col: leaf.pos.start.col,
                    end: (leaf.pos.end.line, leaf.pos.end.col),
                };
                for (namespace, is_value_capture, order) in leaf_actions.drain(..) {
                    let (name, value) = if is_value_capture {
                        if leaf_value.is_empty() {
                            continue;
                        }
                        (
                            leaf_value.clone(),
                            sibling_value
                                .get_or_init(|| sibling_value_in_children(children, arena, table))
                                .clone(),
                        )
                    } else {
                        let Some(name) = key_name.as_ref() else {
                            continue;
                        };
                        (
                            name.clone(),
                            (!leaf_value.is_empty()).then(|| leaf_value.clone()),
                        )
                    };
                    out.push(OrderedDefinition {
                        order,
                        variable: DefinedVariable {
                            name,
                            namespace: Some(namespace.to_string()),
                            location,
                            value,
                        },
                    });
                }
            }
            // LeafValueRule(VariableSetField): a bare value inside a block is the
            // defined variable name (F# InfoService fLeafValue).
            Child::LeafValue(lvi) => {
                let lv = &arena.leaf_values[*lvi as usize];
                with_leaf_value_str(&lv.value, table, |val| {
                    if !val.is_empty() {
                        for group in rule_bodies {
                            for (rule_index, (rule_type, _opts)) in group.rules.iter().enumerate() {
                                if let RuleType::LeafValueRule {
                                    right: NewField::VariableSetField(ns),
                                } = rule_type
                                {
                                    let mut order = group.order.clone();
                                    order.extend([child_index, rule_index]);
                                    out.push(OrderedDefinition {
                                        order,
                                        variable: DefinedVariable {
                                            name: val.to_string(),
                                            namespace: Some(ns.clone()),
                                            location: SourceLocation {
                                                line: lv.pos.start.line,
                                                col: lv.pos.start.col,
                                                end: (lv.pos.end.line, lv.pos.end.col),
                                            },
                                            value: None,
                                        },
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

/// Nonrecursive core for one effect body: collect explicit `var`/`variable`
/// forms (and `array`) or shorthand assignments. The caller walks nested
/// clauses to find effect blocks.
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

    #[test]
    fn variable_definitions_keep_legacy_rule_and_root_order() {
        let table = StringTable::new();
        let parsed = parse_string(
            "root = {\n\
                first_scope = { same = same value = 10 }\n\
                second_scope = { other = other value = 20 }\n\
            }\n",
            &table,
        );
        let variable = NewField::VariableSetField("variable".to_string());
        let mut rules = RuleSet::new();
        rules.types.extend([
            type_def("first", "common/test"),
            type_def("second", "common/test"),
        ]);
        rules.root_rules.extend([
            root_type(
                "first",
                vec![node(
                    NewField::SpecificField("first_scope".to_string()),
                    vec![
                        leaf(
                            NewField::SpecificField("same".to_string()),
                            variable.clone(),
                        ),
                        leaf(variable.clone(), NewField::ScalarField),
                    ],
                )],
            ),
            root_type(
                "second",
                vec![node(
                    NewField::SpecificField("second_scope".to_string()),
                    vec![
                        leaf(
                            NewField::SpecificField("other".to_string()),
                            variable.clone(),
                        ),
                        leaf(variable, NewField::ScalarField),
                    ],
                )],
            ),
        ]);
        rules.reindex();

        let vars = collect_defined_variables_from_rules(
            &rules,
            &parsed,
            "common/test/example.txt",
            &table,
            None,
        );
        let definitions = &vars["variable"];
        assert_eq!(
            definitions
                .iter()
                .map(|definition| (
                    definition.name.as_str(),
                    definition.value.as_deref(),
                    definition.location.line,
                    definition.location.col,
                ))
                .collect::<Vec<_>>(),
            [
                ("same", Some("10"), 2, 16),
                ("same", Some("same"), 2, 16),
                ("value", Some("10"), 2, 28),
                ("other", Some("20"), 3, 17),
                ("other", Some("other"), 3, 17),
                ("value", Some("20"), 3, 31),
            ],
            "root groups and captures must retain the legacy encounter order"
        );

        let values = definitions
            .iter()
            .filter(|definition| definition.name == "same")
            .filter_map(|definition| definition.value.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(values, ["10", "same"]);
    }

    #[test]
    fn variable_definitions_keep_root_clause_order_with_multiple_rule_groups() {
        let table = StringTable::new();
        let parsed = parse_string(
            "root = { filler = {} first_scope = { same = same value = 10 } }\n\
             root = { first_scope = { other = other value = 20 } filler = {} }\n",
            &table,
        );
        let variable = NewField::VariableSetField("variable".to_string());
        let mut rules = RuleSet::new();
        rules.types.extend([
            type_def("first", "common/test"),
            type_def("second", "common/test"),
        ]);
        let scoped_rules = || {
            vec![node(
                NewField::SpecificField("first_scope".to_string()),
                vec![
                    leaf(
                        NewField::SpecificField("same".to_string()),
                        variable.clone(),
                    ),
                    leaf(
                        NewField::SpecificField("other".to_string()),
                        variable.clone(),
                    ),
                    leaf(variable.clone(), NewField::ScalarField),
                ],
            )]
        };
        rules.root_rules.extend([
            root_type("first", scoped_rules()),
            root_type("second", scoped_rules()),
        ]);
        rules.reindex();

        let vars = collect_defined_variables_from_rules(
            &rules,
            &parsed,
            "common/test/example.txt",
            &table,
            None,
        );
        assert_eq!(
            vars["variable"]
                .iter()
                .map(|definition| (definition.name.as_str(), definition.value.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("same", Some("10")),
                ("same", Some("same")),
                ("value", Some("10")),
                ("other", Some("20")),
                ("other", Some("other")),
                ("value", Some("20")),
                ("same", Some("10")),
                ("same", Some("same")),
                ("value", Some("10")),
                ("other", Some("20")),
                ("other", Some("other")),
                ("value", Some("20")),
            ],
            "each rule group scans top-level clauses in source order before the next group"
        );
    }
}
