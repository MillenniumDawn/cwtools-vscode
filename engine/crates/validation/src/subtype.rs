use cwtools_parser::ast::{Child, ParsedFile, Value};
use cwtools_rules::rules_types::*;
use cwtools_string_table::string_table::StringTable;
use rustc_hash::FxHashMap;

use crate::common::child_key_matches;
use crate::rule_core::field_matches_value;

pub fn collect_subtype_instances(
    ruleset: &RuleSet,
    file: &ParsedFile,
    logical_path: &str,
    table: &StringTable,
) -> std::collections::HashMap<String, Vec<cwtools_index::TypeInstance>> {
    let mut out: std::collections::HashMap<String, Vec<cwtools_index::TypeInstance>> =
        Default::default();
    cwtools_index::for_each_instance_node(
        ruleset,
        file,
        logical_path,
        table,
        &mut |td, name, node_key, children, location| {
            let node = cwtools_index::InstanceNode {
                td,
                name,
                node_key,
                children,
                location,
            };
            subtype_membership_for_instance(ruleset, file, &node, table, &mut out);
        },
    );
    out
}

pub fn subtype_membership_for_instance(
    ruleset: &RuleSet,
    file: &ParsedFile,
    node: &cwtools_index::InstanceNode,
    table: &StringTable,
    out: &mut std::collections::HashMap<String, Vec<cwtools_index::TypeInstance>>,
) {
    let mut key = String::new();
    for st in &node.td.subtypes {
        if subtype_matches(
            st,
            node.children,
            file,
            table,
            ruleset,
            Some(node.node_key),
            None,
        ) {
            key.clear();
            key.push_str(&node.td.name);
            key.push('.');
            key.push_str(&st.name);
            let entry = match out.get_mut(key.as_str()) {
                Some(v) => v,
                None => out.entry(key.clone()).or_default(),
            };
            entry.push(cwtools_index::TypeInstance {
                name: node.name.to_string(),
                location: node.location,
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            });
        }
    }
}

pub(crate) fn subtype_rules_match(
    rules: &[(RuleType, Options)],
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    ruleset: &RuleSet,
    type_index: Option<&cwtools_index::TypeIndex>,
) -> bool {
    #[derive(Default)]
    struct KeyGroup<'a> {
        leaf_rights: Vec<(&'a NewField, &'a Options)>,
        node_inners: Vec<(&'a [(RuleType, Options)], &'a Options)>,
    }
    let mut groups: FxHashMap<&str, KeyGroup> =
        FxHashMap::with_capacity_and_hasher(rules.len(), Default::default());
    for (rt, opts) in rules {
        match rt {
            RuleType::LeafRule {
                left: NewField::SpecificField(k),
                right,
            } => {
                groups
                    .entry(k.as_str())
                    .or_default()
                    .leaf_rights
                    .push((right, opts));
            }
            RuleType::NodeRule {
                left: NewField::SpecificField(k),
                rules: inner,
            } => {
                groups
                    .entry(k.as_str())
                    .or_default()
                    .node_inners
                    .push((inner.as_ref(), opts));
            }
            _ => {}
        }
    }
    if groups.is_empty() {
        return true;
    }
    let mut activated = false;

    for (k, group) in &groups {
        // `k` is loop-invariant; unquote it once instead of per child.
        let k_unq = crate::common::unquote_key(k);
        let mut count: i32 = 0;
        let mut any_match = false;
        for c in children {
            let (matches_key, leaf_value, clause): (bool, Option<&Value>, Option<&[Child]>) =
                match c {
                    Child::Leaf(idx) => {
                        let leaf = &ast.arena.leaves[*idx as usize];
                        if table
                            .with_string(leaf.key.normal, |s| {
                                crate::common::unquote_key(s).eq_ignore_ascii_case(k_unq)
                            })
                            .unwrap_or(false)
                        {
                            match &leaf.value {
                                Value::Clause(ch) => (true, None, Some(ch.as_slice())),
                                v => (true, Some(v), None),
                            }
                        } else {
                            (false, None, None)
                        }
                    }
                    _ => (false, None, None),
                };
            if !matches_key {
                continue;
            }
            count += 1;
            if let Some(v) = leaf_value {
                for (right, _) in &group.leaf_rights {
                    if field_matches_value(right, v, table, ruleset) {
                        any_match = true;
                        if field_activates_on_presence(right)
                            || typefield_value_is_instance(right, v, table, type_index)
                        {
                            activated = true;
                        }
                    }
                }
            }
            if let Some(ic) = clause
                && group.node_inners.iter().any(|(inner, _)| {
                    subtype_rules_match(inner, ic, ast, table, ruleset, type_index)
                })
            {
                any_match = true;
                activated = true;
            }
        }
        if count > 0 && !any_match {
            return false;
        }
        let all_opts = group
            .leaf_rights
            .iter()
            .map(|(_, o)| *o)
            .chain(group.node_inners.iter().map(|(_, o)| *o));
        let min_required = all_opts.clone().map(|o| o.min).max().unwrap_or(0);
        let max_allowed = all_opts.map(|o| o.max).min().unwrap_or(i32::MAX);
        if min_required > count || count > max_allowed {
            return false;
        }
        if count == 0
            && group
                .leaf_rights
                .iter()
                .any(|(r, _)| is_default_satisfied_literal(r))
        {
            activated = true;
        }
    }

    activated
}

fn field_activates_on_presence(right: &NewField) -> bool {
    !matches!(
        right,
        NewField::TypeField(_)
            | NewField::AliasField(_)
            | NewField::SingleAliasField(_)
            | NewField::IgnoreField(_)
            | NewField::IgnoreMarkerField
    )
}

fn is_default_satisfied_literal(right: &NewField) -> bool {
    matches!(right, NewField::SpecificField(v) if v == "no" || v == "false" || v == "0")
}

pub(crate) fn subtype_matches(
    subtype: &SubTypeDefinition,
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    ruleset: &RuleSet,
    node_key: Option<&str>,
    type_index: Option<&cwtools_index::TypeIndex>,
) -> bool {
    if !subtype.type_key_filter.is_empty() {
        return node_key.is_some_and(|k| {
            subtype
                .type_key_filter
                .iter()
                .any(|f| f.eq_ignore_ascii_case(k))
        });
    }
    if let Some(fk) = &subtype.type_key_field {
        return children
            .iter()
            .any(|c| child_key_matches(c, ast, table, fk));
    }
    subtype_rules_match(&subtype.rules, children, ast, table, ruleset, type_index)
}

pub(crate) fn typefield_value_is_instance(
    right: &NewField,
    value: &Value,
    table: &StringTable,
    type_index: Option<&cwtools_index::TypeIndex>,
) -> bool {
    let (NewField::TypeField(TypeType::Simple(tname)), Some(idx)) = (right, type_index) else {
        return false;
    };
    match value {
        Value::String(t) | Value::QString(t) => {
            crate::common::with_match_text(table, t, |v| idx.contains(tname, v))
        }
        _ => false,
    }
}
