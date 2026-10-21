use cwtools_parser::{
    ast::{Child, ParsedFile, Value},
    unquote,
};
use cwtools_rules::rules_types::*;
use cwtools_string_table::string_table::StringTable;

use crate::common::child_key_matches;
use crate::rule_core::field_matches_value;

pub fn collect_subtype_instances(
    ruleset: &RuleSet,
    file: &ParsedFile,
    logical_path: &str,
    table: &StringTable,
) -> std::collections::HashMap<String, Vec<cwtools_index::TypeInstance>> {
    let mut out = cwtools_index::CollectedTypeInstances::default();
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
    out.subtype_instances
}

pub fn subtype_membership_for_instance(
    ruleset: &RuleSet,
    file: &ParsedFile,
    node: &cwtools_index::InstanceNode,
    table: &StringTable,
    out: &mut cwtools_index::CollectedTypeInstances,
) {
    let mut key = String::new();
    let mut unresolved = Vec::new();
    for (subtype_index, st) in node.td.subtypes.iter().enumerate() {
        let match_context = SubtypeMatchContext::for_subtype(ruleset, node.td, st, subtype_index);
        let matched = subtype_matches(
            st,
            node.children,
            file,
            table,
            &match_context,
            Some(node.node_key),
            &mut TypeRefs::Defer(&mut unresolved),
        );
        if !matched && unresolved.is_empty() {
            continue;
        }
        key.clear();
        key.push_str(&node.td.name);
        key.push('.');
        key.push_str(&st.name);
        if !matched {
            out.derived
                .inherited
                .extend(unresolved.drain(..).map(|(via_type, via_name)| {
                    cwtools_index::InheritedSubtype {
                        subtype: key.clone(),
                        name: node.name.to_string(),
                        location: node.location,
                        via_type,
                        via_name,
                    }
                }));
            continue;
        }
        let subtypes = &mut out.subtype_instances;
        let entry = match subtypes.get_mut(key.as_str()) {
            Some(v) => v,
            None => subtypes.entry(key.clone()).or_default(),
        };
        entry.push(cwtools_index::TypeInstance {
            name: node.name.to_string(),
            location: node.location,
            primary_loc_key: None,
            required_loc_keys: Vec::new(),
        });
    }
}

/// What subtype matching does with a `<type>` value that would activate it.
pub(crate) enum TypeRefs<'a> {
    /// Activate when the index holds the value, and never without one.
    Check(Option<&'a cwtools_index::TypeIndex>),
    /// Collect the `(type, name)` pairs for an index that does not exist yet.
    Defer(&'a mut Vec<(String, String)>),
}

impl<'a> TypeRefs<'a> {
    fn discard(&mut self) {
        if let TypeRefs::Defer(pending) = self {
            pending.clear();
        }
    }

    /// Nested clauses activate only on what is known now.
    fn nested(&self) -> TypeRefs<'a> {
        match self {
            TypeRefs::Check(type_index) => TypeRefs::Check(*type_index),
            TypeRefs::Defer(_) => TypeRefs::Check(None),
        }
    }
}

pub(crate) fn subtype_rules_match(
    rules: &[(RuleType, Options)],
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    ruleset: &RuleSet,
    precomputed_groups: Option<&[SubtypeRuleKeyGroup]>,
    type_refs: &mut TypeRefs<'_>,
) -> bool {
    let fallback_groups;
    let groups = if let Some(groups) = precomputed_groups {
        groups
    } else {
        fallback_groups = build_subtype_rule_key_groups(rules);
        &fallback_groups
    };
    if groups.is_empty() {
        return true;
    }
    let mut activated = false;

    for group in groups {
        // `key` is loop-invariant; unquote it once instead of per child.
        let k_unq = unquote(group.key.as_str());
        let mut count: i32 = 0;
        let mut any_match = false;
        for c in children {
            let (matches_key, leaf_value, clause): (bool, Option<&Value>, Option<&[Child]>) =
                match c {
                    Child::Leaf(idx) => {
                        let leaf = &ast.arena.leaves[*idx as usize];
                        if table
                            .with_string(leaf.key.normal, |s| {
                                unquote(s).eq_ignore_ascii_case(k_unq)
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
            if let Some(value) = leaf_value {
                for &rule_index in &group.leaf_rule_indices {
                    if let RuleType::LeafRule { right, .. } = &rules[rule_index].0
                        && field_matches_value(right, value, table, ruleset)
                    {
                        any_match = true;
                        if field_activates_on_presence(right) {
                            activated = true;
                            continue;
                        }
                        match type_refs {
                            TypeRefs::Check(type_index) => {
                                if typefield_value_is_instance(right, value, table, *type_index) {
                                    activated = true;
                                }
                            }
                            TypeRefs::Defer(pending) => {
                                pending.extend(simple_type_ref(right, value, table));
                            }
                        }
                    }
                }
            }
            if let Some(inner_children) = clause
                && group
                    .node_rule_indices
                    .iter()
                    .enumerate()
                    .any(|(nested_index, &rule_index)| {
                        if let RuleType::NodeRule { rules: inner, .. } = &rules[rule_index].0 {
                            subtype_rules_match(
                                inner,
                                inner_children,
                                ast,
                                table,
                                ruleset,
                                group.node_rule_groups.get(nested_index).map(Vec::as_slice),
                                &mut type_refs.nested(),
                            )
                        } else {
                            false
                        }
                    })
            {
                any_match = true;
                activated = true;
            }
        }
        if count > 0 && !any_match {
            type_refs.discard();
            return false;
        }
        let min_required = group
            .leaf_rule_indices
            .iter()
            .chain(&group.node_rule_indices)
            .map(|&rule_index| rules[rule_index].1.min)
            .max()
            .unwrap_or(0);
        let max_allowed = group
            .leaf_rule_indices
            .iter()
            .chain(&group.node_rule_indices)
            .map(|&rule_index| rules[rule_index].1.max)
            .min()
            .unwrap_or(i32::MAX);
        if min_required > count || count > max_allowed {
            type_refs.discard();
            return false;
        }
        if count == 0
            && group.leaf_rule_indices.iter().any(|&rule_index| {
                matches!(
                    &rules[rule_index].0,
                    RuleType::LeafRule { right, .. } if is_default_satisfied_literal(right)
                )
            })
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

pub(crate) struct SubtypeMatchContext<'a> {
    ruleset: &'a RuleSet,
    precomputed_groups: Option<&'a [SubtypeRuleKeyGroup]>,
}

impl<'a> SubtypeMatchContext<'a> {
    pub(crate) fn for_subtype(
        ruleset: &'a RuleSet,
        type_def: &TypeDefinition,
        subtype: &SubTypeDefinition,
        subtype_index: usize,
    ) -> Self {
        let precomputed_groups =
            if subtype.type_key_filter.is_empty() && subtype.type_key_field.is_none() {
                ruleset.subtype_rule_key_groups_for(type_def, subtype_index)
            } else {
                None
            };
        Self {
            ruleset,
            precomputed_groups,
        }
    }
}

pub(crate) fn subtype_matches(
    subtype: &SubTypeDefinition,
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    match_context: &SubtypeMatchContext<'_>,
    node_key: Option<&str>,
    type_refs: &mut TypeRefs<'_>,
) -> bool {
    type_refs.discard();
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
    subtype_rules_match(
        &subtype.rules,
        children,
        ast,
        table,
        match_context.ruleset,
        match_context.precomputed_groups,
        type_refs,
    )
}

/// The `(type, name)` a `<type>` value names, which only an index can confirm.
fn simple_type_ref(
    right: &NewField,
    value: &Value,
    table: &StringTable,
) -> Option<(String, String)> {
    let NewField::TypeField(TypeType::Simple(type_name)) = right else {
        return None;
    };
    match value {
        Value::String(t) | Value::QString(t) => crate::common::with_match_text(table, t, |name| {
            Some((type_name.clone(), name.to_string()))
        }),
        _ => None,
    }
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
