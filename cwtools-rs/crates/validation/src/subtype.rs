//! Subtype matching: decide which subtypes of a type are active for an entity.

use cwtools_parser::ast::{Child, ParsedFile, Value};
use cwtools_rules::rules_types::*;
use cwtools_string_table::string_table::StringTable;
use rustc_hash::FxHashMap;

use crate::common::child_key_matches;
use crate::rule_core::field_matches_value;

/// Derive a file's subtype-qualified membership: for every type instance, which
/// of its subtypes are active, keyed `"type.subtype" -> [instances]`. Merged into
/// the [`cwtools_index::TypeIndex`] so `<type.subtype>` references (e.g.
/// `archetype = <equipment.naval_equip>`) resolve.
///
/// Membership is computed from each instance's *own* discriminators with no
/// type index (`type_index = None`), so a subtype that only activates via a
/// `<type.subtype>` back-reference is intentionally NOT recorded here: an
/// archetype self-determines from a direct discriminator (`type = enum[...]`,
/// `is_archetype = yes`), and that is exactly what a referencing variant needs to
/// resolve its `archetype = <type.subtype>` at validation time. Plugged into
/// [`cwtools_index::index_discovered_files`] as a [`cwtools_index::SubtypeCollector`].
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

/// Append one instance node's active subtypes to `out` under `"type.subtype"`
/// keys. The per-node core of [`collect_subtype_instances`], and the
/// [`cwtools_index::SubtypeCollector`] hook that `index_discovered_files` runs
/// inside the type-instance walk so type and subtype collection share a single
/// navigation.
pub fn subtype_membership_for_instance(
    ruleset: &RuleSet,
    file: &ParsedFile,
    node: &cwtools_index::InstanceNode,
    table: &StringTable,
    out: &mut std::collections::HashMap<String, Vec<cwtools_index::TypeInstance>>,
) {
    // Only reached for types that declare subtypes. The `type.subtype` key is
    // assembled into one reused buffer and only handed to the map on the first
    // instance of each subtype; after that the entry already exists.
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
                // Hover resolves the primary loc key via the base-type
                // instances; subtype-qualified entries don't need it.
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            });
        }
    }
}

/// Test whether a subtype's rules are satisfied by an entity's children.
///
/// A subtype is active unless one of its rules is violated:
///   - a required rule (min >= 1) whose key is absent (or under-count),
///   - a key present more than its max,
///   - a PRESENT field whose value doesn't match the rule.
///
/// Fields the rules don't mention are ignored, so a subtype whose rules are
/// all optional (`## cardinality = 0..1`) and absent matches vacuously.
/// The real discriminators are the un-annotated rules (default `1..1`, required)
/// and any present field whose value contradicts a rule.
pub(crate) fn subtype_rules_match(
    rules: &[(RuleType, Options)],
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    ruleset: &RuleSet,
    type_index: Option<&cwtools_index::TypeIndex>,
) -> bool {
    // A subtype with discriminators must be *positively activated* by the entity:
    // A subtype matches when its rules apply cleanly, but one whose
    // discriminators are all optional (`0..1`) and absent would otherwise match
    // every entity and wrongly impose its required body fields. So we additionally
    // require some discriminator to be actively met. A present field that fails a
    // discriminator still *blocks* the match (contradiction), and a missing
    // required (`min>=1`) discriminator still fails it.
    //
    // Discriminators are grouped by key. Several rules can share a key as a
    // disjunction — both same-kind (`trait_type = assignable_trait` / `trait_type =
    // assignable_terrain_trait`) and cross-kind (`type = enum[air_units]` as a leaf
    // OR `type = { enum[air_units] }` as a block). Cardinality is counted by key
    // across leaves AND nodes, and a present field is a contradiction only when it
    // matches NONE of the key's rules. So we collect both leaf and node rules under
    // one key and evaluate them together.
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
        // No discriminators at all → pure-marker subtype, matches vacuously.
        return true;
    }
    let mut activated = false;

    for (k, group) in &groups {
        // `k` is loop-invariant; unquote it once instead of per child.
        let k_unq = crate::common::unquote_key(k);
        let mut count: i32 = 0;
        let mut any_match = false;
        for c in children {
            // Resolve this child's key and decide which discriminator kind applies:
            // a scalar leaf checks the leaf rules; a block (node or clause-leaf)
            // checks the node rules.
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
                        // A present field activates the subtype. A bare `<type>` ref
                        // doesn't activate on shape alone (the key is common), but it
                        // DOES when the value is a verified instance of that type —
                        // e.g. `category = <peace_action_categories>` with a real
                        // category. A `<type.subtype>` ref has no plain type entry so
                        // this naturally declines (keeps `air_equip` from activating
                        // on every `archetype = ...`).
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
        // Present but matching none of the disjuncts (of the applicable kind) → contradiction.
        if count > 0 && !any_match {
            return false;
        }
        // Cardinality is counted by key across both kinds: required if any disjunct
        // demands it, capped by the tightest max.
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
        // Absent but a disjunct is the field's default value (`= no`/`false`/`0`).
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

/// Whether a present field matching this discriminator activates the subtype.
/// Most discriminators are presence signals (`days_remove = scalar`, `is_archetype
/// = yes`, `type = enum[...]`). The exception is a bare `<type>` reference and the
/// alias/ignore placeholders: those keys are common and their value check is
/// permissive, so presence alone is not a reliable subtype signal.
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

/// A `field = literal` discriminator whose literal is the field's default value,
/// so an absent field satisfies it. Paradox booleans default to `no`/`false` and
/// numeric flags to `0`.
fn is_default_satisfied_literal(right: &NewField) -> bool {
    matches!(right, NewField::SpecificField(v) if v == "no" || v == "false" || v == "0")
}

/// Decide whether a subtype is active for an entity.
///
/// - `## type_key_filter`: active iff the instance's own node key is in the list.
/// - Explicit `type_key_field`: active iff the entity has a child with that key.
/// - Otherwise: apply the subtype's rules (cardinality-aware) — see
///   [`subtype_rules_match`]. An empty subtype matches vacuously.
pub(crate) fn subtype_matches(
    subtype: &SubTypeDefinition,
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    ruleset: &RuleSet,
    node_key: Option<&str>,
    type_index: Option<&cwtools_index::TypeIndex>,
) -> bool {
    // `## type_key_filter` discriminates on the instance's own node key (e.g.
    // `shared_focus` selects subtype[shared], `joint_focus` selects subtype[joint_focus]).
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

/// True when `right` is a plain `<type>` reference and `value` is a known instance
/// of that type. Used so a present typed discriminator activates its subtype only
/// when the value is real (not on the shape of the key alone). A `<type.subtype>`
/// reference has no plain type entry in the index, so it declines here.
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
