use cwtools_parser::ast::{Arena, Child, ParsedFile, SourceRange, Value};
use cwtools_rules::rules_types::{NewField, RuleSet, RuleType};
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

    let mut type_rules: HashMap<&str, Vec<&RuleType>> = HashMap::new();
    for root_rule in &ruleset.root_rules {
        if let cwtools_rules::rules_types::RootRule::TypeRule(name, (rule_type, _opts)) = root_rule
        {
            type_rules.entry(name.as_str()).or_default().push(rule_type);
        }
    }

    let np = NormalizedPath::new(logical_path);
    for td in &ruleset.types {
        if !check_path_dir_norm(&td.path_options, &np) {
            continue;
        }
        let Some(rules_for_type) = type_rules.get(td.name.as_str()) else {
            continue;
        };
        for rule_type in rules_for_type {
            if let RuleType::NodeRule { rules, .. } = rule_type {
                for child in &file.root_children {
                    if let Some(kc) = file.arena.keyed_clause(child) {
                        scan_children_for_varset(
                            kc.children,
                            &file.arena,
                            table,
                            rules,
                            &mut result,
                        );
                    }
                }
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
    rules: &[(
        cwtools_rules::rules_types::RuleType,
        cwtools_rules::rules_types::Options,
    )],
    out: &mut HashMap<String, Vec<DefinedVariable>>,
) {
    // the assigned value lives in a sibling `value` leaf of the same block.
    // Computed lazily: most blocks never hit the arm that needs it.
    let sibling_value = std::cell::OnceCell::new();
    for child in children {
        if let Some(kc) = arena.keyed_clause(child) {
            // lock) rather than holding `with_string` across the recursive
            // lock, which would risk a re-entrant read-lock deadlock under writer
            let child_key = get_string_or_empty(table, kc.key.normal);
            for (rule_type, _) in rules {
                // variable name (F# InfoService fNode).
                if let RuleType::NodeRule {
                    left: NewField::VariableSetField(ns),
                    ..
                } = rule_type
                {
                    if !child_key.is_empty() {
                        out.entry(ns.clone()).or_default().push(DefinedVariable {
                            name: child_key.clone(),
                            namespace: Some(ns.clone()),
                            location: SourceLocation {
                                line: kc.pos.start.line,
                                col: kc.pos.start.col,
                                end: (kc.pos.end.line, kc.pos.end.col),
                            },
                            value: None,
                        });
                    }
                } else if let RuleType::NodeRule {
                    left: NewField::SpecificField(expected_key),
                    rules: inner,
                    ..
                } = rule_type
                {
                    if child_key.eq_ignore_ascii_case(expected_key) {
                        scan_children_for_varset(kc.children, arena, table, inner, out);
                    }
                } else if let RuleType::NodeRule { rules: inner, .. } = rule_type {
                    scan_children_for_varset(kc.children, arena, table, inner, out);
                }
            }
            continue;
        }
        match child {
            Child::Leaf(li) => {
                let leaf = &arena.leaves[*li as usize];
                // resolution releases the table lock, avoiding the re-entrant
                // read-lock hazard of nesting `with_string` borrows). Most leaves
                let key = std::cell::OnceCell::new();
                let val = std::cell::OnceCell::new();
                for (rule_type, _opts) in rules {
                    match rule_type {
                        RuleType::LeafRule {
                            left: NewField::VariableSetField(ns),
                            ..
                        } => {
                            let key =
                                key.get_or_init(|| get_string_or_empty(table, leaf.key.normal));
                            let val = val.get_or_init(|| leaf_value_string(&leaf.value, table));
                            out.entry(ns.clone()).or_default().push(DefinedVariable {
                                name: key.clone(),
                                namespace: Some(ns.clone()),
                                location: SourceLocation {
                                    line: leaf.pos.start.line,
                                    col: leaf.pos.start.col,
                                    end: (leaf.pos.end.line, leaf.pos.end.col),
                                },
                                value: (!val.is_empty()).then(|| val.clone()),
                            });
                        }
                        RuleType::LeafRule {
                            left: NewField::SpecificField(expected_key),
                            right: NewField::VariableSetField(ns),
                        } => {
                            let val = val.get_or_init(|| leaf_value_string(&leaf.value, table));
                            let key =
                                key.get_or_init(|| get_string_or_empty(table, leaf.key.normal));
                            if !val.is_empty() && key.eq_ignore_ascii_case(expected_key) {
                                out.entry(ns.clone()).or_default().push(DefinedVariable {
                                    name: val.clone(),
                                    namespace: Some(ns.clone()),
                                    location: SourceLocation {
                                        line: leaf.pos.start.line,
                                        col: leaf.pos.start.col,
                                        end: (leaf.pos.end.line, leaf.pos.end.col),
                                    },
                                    value: sibling_value
                                        .get_or_init(|| {
                                            sibling_value_in_children(children, arena, table)
                                        })
                                        .clone(),
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            // LeafValueRule(VariableSetField): a bare value inside a block is the
            // defined variable name (F# InfoService fLeafValue).
            Child::LeafValue(lvi) => {
                let lv = &arena.leaf_values[*lvi as usize];
                with_leaf_value_str(&lv.value, table, |val| {
                    if !val.is_empty() {
                        for (rule_type, _opts) in rules {
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
