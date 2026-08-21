//! Rule-driven variable / value_set collection: gathering defined variable names
//! (and their values) from parsed files.

use cwtools_parser::ast::{Arena, Child, ParsedFile, SourceRange, Value};
use cwtools_rules::rules_types::{NewField, RuleSet, RuleType};
use cwtools_string_table::string_table::StringTable;
use std::collections::{HashMap, HashSet};

use crate::{
    NormalizedPath, SourceLocation, check_path_dir_norm, get_string_or_empty, leaf_value_string,
    with_leaf_value_str,
};

/// A defined variable entry (either @-style or rule-driven value_set).
#[derive(Debug, Clone)]
pub struct DefinedVariable {
    pub name: String,
    pub namespace: Option<String>, // value_set namespace, if any
    pub location: SourceLocation,
    /// The value assigned at this definition site, when the rule shape provides
    /// one (`set_variable = { var = x value = 5 }` or shorthand
    /// `set_variable = { x = 5 }`). `None` when no value is statically known.
    pub value: Option<String>,
}

/// Collect variables using full rule-tree walking.
/// For each leaf where the rule field is `VariableSetField(ns)`, record the
/// variable name under namespace `ns`.
///
/// When `at_vars` is `Some`, those entries are used as the "@" namespace
/// instead of re-scanning the AST for `@`-prefix leaves (avoids a redundant
/// walk when the caller already collected them via the heuristic pass).
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

    // Index the root TypeRules by name once so the per-type lookup is O(1) instead
    // of an O(types × root_rules) linear scan. A type name can carry more than one
    // TypeRule, so group them (each matching rule is still scanned, preserving the
    // original multiplicity).
    let mut type_rules: HashMap<&str, Vec<&RuleType>> = HashMap::new();
    for root_rule in &ruleset.root_rules {
        if let cwtools_rules::rules_types::RootRule::TypeRule(name, (rule_type, _opts)) = root_rule
        {
            type_rules.entry(name.as_str()).or_default().push(rule_type);
        }
    }

    // Walk type instances (path-filtered) and scan their rules for VariableSetField
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
                // Scan each root instance's children against these rules.
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
            // Gate on the cheap `@` prefix test inside `with_string` so the vast
            // majority of (non-`@`) leaves never allocate a key String.
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

/// The value of a `value`/`amount`/`add` child leaf in `children`, used to
/// recover the assigned value for the explicit `var = X / value = Y` form.
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
    // For the explicit `var = value_set[variable] / value = variable_field` form
    // the assigned value lives in a sibling `value` leaf of the same block.
    // Computed lazily: most blocks never hit the arm that needs it.
    let sibling_value = std::cell::OnceCell::new();
    for child in children {
        // A keyed clause (`key = { ... }`) takes the NodeRule path.
        if let Some(kc) = arena.keyed_clause(child) {
            // Resolve the clause key with `get_string` (which releases the table
            // lock) rather than holding `with_string` across the recursive
            // `scan_children_for_varset` calls below — those re-acquire the table
            // lock, which would risk a re-entrant read-lock deadlock under writer
            // contention during parallel indexing.
            let child_key = get_string_or_empty(table, kc.key.normal);
            for (rule_type, _) in rules {
                // NodeRule(VariableSetField): the clause's key IS the defined
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
                    // Only recurse when the child's key matches the rule's
                    // expected key. Previously ALL NodeRules were applied to
                    // every child node, recording junk variable names.
                    if child_key.eq_ignore_ascii_case(expected_key) {
                        scan_children_for_varset(kc.children, arena, table, inner, out);
                    }
                } else if let RuleType::NodeRule { rules: inner, .. } = rule_type {
                    // Non-SpecificField node rule (e.g. alias or scalar key):
                    // recurse unconditionally as before.
                    scan_children_for_varset(kc.children, arena, table, inner, out);
                }
            }
            continue;
        }
        match child {
            Child::Leaf(li) => {
                let leaf = &arena.leaves[*li as usize];
                // Resolve key and value lazily and only on a matching rule (each
                // resolution releases the table lock, avoiding the re-entrant
                // read-lock hazard of nesting `with_string` borrows). Most leaves
                // match neither variable-set arm, so they allocate nothing.
                let key = std::cell::OnceCell::new();
                let val = std::cell::OnceCell::new();
                for (rule_type, _opts) in rules {
                    match rule_type {
                        // left = VariableSetField: the leaf's key IS the defined
                        // variable name, and its RHS is the assigned value
                        // (shorthand `set_variable = { my_var = 5 }`). Only applies
                        // when the rule's left is a pure variable-set field (no
                        // specific key to match against).
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
                        // right = VariableSetField: the leaf's VALUE is the defined
                        // variable name (explicit `var = my_var`), but only when the
                        // leaf's key matches the rule's expected key (SpecificField).
                        // The assigned value comes from the sibling `value` leaf.
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

/// The set of effect/trigger names that DEFINE a `value_set[variable]` (e.g.
/// `set_variable`, `set_temp_variable`, `add_to_variable`) or a
/// `value_set[array]` (`add_to_array`, `resize_array`, …). An alias qualifies
/// when its rule body contains a matching `VariableSetField`. Config-driven, so
/// it tracks whatever the game's `.cwt` declares rather than a hardcoded list.
///
/// Arrays are folded in with variables because the engine stores them the same
/// way — `my_arr^num` is a variable read — so a name defined by `add_to_array`
/// is "set" for CW246.
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

/// Scan a file's AST for variable definitions and push each raw name into `out`.
/// For every block whose key is a variable-defining effect, the defined name is
/// the value of an explicit `var`/`variable` child, or — in the shorthand form
/// `set_variable = { my_var = 3 }` — the inner assignment's key. The rule-driven
/// [`collect_defined_variables_from_rules`] misses these because they live inside
/// `alias[effect]` expansions the type-rule walk never reaches; this direct scan
/// does not depend on rule matching.
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

/// Like [`collect_set_variable_names`] but keeps each definition's source
/// location and, where the block provides one, its assigned value (the `value`
/// child for the explicit form, or the RHS for the shorthand form). Used by the
/// LSP so hover/goto can point at a variable's definition and show its value.
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

/// Extract variable definitions from the direct children of one variable-defining
/// effect block (`set_variable = { ... }` and friends): the explicit
/// `var`/`variable = NAME` form (value from a sibling `value`/`amount`/`add`), or
/// the shorthand `{ my_var = 5 }` form. Non-recursive — this is one effect
/// invocation's body. The per-node core shared by [`collect_set_variable_defs`]
/// and the fused index walk (`crate::collect`); the walk that finds the effect
/// blocks stays with the caller.
pub(crate) fn extract_set_variable_defs_block(
    children: &[Child],
    arena: &Arena,
    table: &StringTable,
    out: &mut Vec<DefinedVariable>,
) {
    // Explicit form: a `var`/`variable`/`array` child holds the name as its
    // value; the assigned value (if any) is the sibling `value`/`amount`/`add`
    // leaf.
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
    // Shorthand form: the inner assignment key is the variable name and its
    // RHS (if a leaf) is the assigned value.
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
        // Case-insensitive compare without allocating a lowercased copy of the
        // key just to probe the skip-list (paradox keys are ASCII).
        if !SKIP_KEYS.iter().any(|k| key.eq_ignore_ascii_case(k)) {
            out.push(variable_def(key, value, pos));
        }
    }
}
