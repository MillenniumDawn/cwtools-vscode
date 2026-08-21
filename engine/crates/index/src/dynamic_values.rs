//! Dynamically-defined value collection for editor features.
//!
//! Two kinds of names are defined by game/mod *content* rather than the rules:
//!   * complex-enum members — extracted from script files per the config's
//!     `complex_enum[...]` definitions (e.g. `equipment_stat` from
//!     `common/script_enums.txt`, `country_tags` from `common/country_tags`).
//!   * `value_set[...]` members — flags/tokens written by effects
//!     (`set_country_flag = my_flag` defines a `country_flag`).
//!
//! Complex-enum members feed completion (and hover) only: validation keeps its
//! lenient behavior for absent/large enums, so collecting them changes no
//! diagnostics. Value-set members feed completion the same way, but the rule
//! engine reads them too, for `from_data` scope links whose `data_source` is
//! `value[<set>]` (see `TypeIndex::value_set_values`).

use crate::{dec_ref, unquote};
use std::collections::HashMap;
use std::sync::Arc;

use cwtools_parser::ast::{Child, Leaf, ParsedFile, Value};
use cwtools_rules::rules_types::{ComplexEnumNameTree, ComplexEnumNameTreeEntry, RuleSet};
use cwtools_string_table::string_table::{StringId, StringTable};

use crate::check_path_dir;

/// One (enum-name, value) pair stored in the per-file bookkeeping list.
type NameValuePair = (Arc<str>, Arc<str>);

/// `name -> value -> refcount`, with per-file bookkeeping so single-file
/// re-indexing (the LSP edit path) replaces a file's contribution instead of
/// leaking it. Used for both complex-enum members (name = enum name) and
/// value-set members (name = namespace).
///
/// `Arc<str>` keys in both maps share the same allocation — each (name, value)
/// string is allocated once even though it appears in both `by_name` and the
/// per-file bookkeeping list.
#[derive(Debug, Clone, Default)]
pub struct NamedValueIndex {
    by_name: HashMap<Arc<str>, HashMap<Arc<str>, usize>>,
    per_file: HashMap<String, Vec<NameValuePair>>,
}

impl NamedValueIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace `file_uri`'s contribution with `items`.
    pub fn merge_file(&mut self, file_uri: &str, items: HashMap<String, Vec<String>>) {
        self.remove_file(file_uri);
        let mut flat: Vec<(Arc<str>, Arc<str>)> = Vec::new();
        for (name, values) in items {
            let name_arc: Arc<str> = Arc::from(name.as_str());
            for v in values {
                let val_arc: Arc<str> = Arc::from(v.as_str());
                *self
                    .by_name
                    .entry(Arc::clone(&name_arc))
                    .or_default()
                    .entry(Arc::clone(&val_arc))
                    .or_insert(0) += 1;
                flat.push((Arc::clone(&name_arc), val_arc));
            }
        }
        if !flat.is_empty() {
            self.per_file.insert(file_uri.to_string(), flat);
        }
    }

    /// Drop `file_uri`'s contribution (refcounted).
    pub fn remove_file(&mut self, file_uri: &str) {
        let Some(flat) = self.per_file.remove(file_uri) else {
            return;
        };
        for (name, v) in flat {
            if let Some(vals) = self.by_name.get_mut(name.as_ref()) {
                dec_ref(vals, v.as_ref());
                if vals.is_empty() {
                    self.by_name.remove(name.as_ref());
                }
            }
        }
    }

    /// All known values for `name`.
    pub fn values(&self, name: &str) -> impl Iterator<Item = &str> {
        self.by_name
            .get(name)
            .into_iter()
            .flat_map(|m| m.keys().map(Arc::as_ref))
    }

    /// Whether `name`'s set contains `value` (O(1) hash probe, versus scanning
    /// every member with `values(name)`).
    pub fn contains(&self, name: &str, value: &str) -> bool {
        self.by_name
            .get(name)
            .is_some_and(|m| m.contains_key(value))
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// Export as `(name, values)` pairs (the vanilla-cache shape).
    pub fn export(&self) -> Vec<(String, Vec<String>)> {
        self.by_name
            .iter()
            .map(|(name, vals)| {
                (
                    name.as_ref().to_string(),
                    vals.keys().map(|v| v.as_ref().to_string()).collect(),
                )
            })
            .collect()
    }
}

/// Collect complex-enum members defined in one parsed file, keyed by enum name.
pub fn collect_complex_enum_values(
    ruleset: &RuleSet,
    parsed: &ParsedFile,
    logical_path: &str,
    table: &StringTable,
) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for def in &ruleset.complex_enums {
        if !check_path_dir(&def.path_options, logical_path) {
            continue;
        }
        let mut values = Vec::new();
        if def.start_from_root {
            walk_name_tree(
                &def.name_tree,
                &parsed.root_children,
                parsed,
                table,
                &mut values,
            );
        } else {
            // The tree applies inside each top-level entity.
            for child in &parsed.root_children {
                if let Child::Leaf(idx) = child
                    && let Value::Clause(ch) = &parsed.arena.leaves[*idx as usize].value
                {
                    walk_name_tree(&def.name_tree, ch, parsed, table, &mut values);
                }
            }
        }
        if !values.is_empty() {
            out.entry(def.name.clone()).or_default().extend(values);
        }
    }
    out
}

/// Push a value-set member, stripping an `@datestamp` suffix
/// (`my_flag@1936.1.1` sets `my_flag`). Empty bases are skipped.
fn push_member(out: &mut HashMap<String, Vec<String>>, ns: String, raw: &str) {
    let v = unquote(raw);
    let base = v.split('@').next().unwrap_or(v);
    if !base.is_empty() {
        out.entry(ns).or_default().push(base.to_string());
    }
}

fn push_unquoted_key(out: &mut Vec<String>, table: &StringTable, id: StringId) {
    if let Some(k) = table.get_string(id) {
        out.push(unquote(&k).to_string());
    }
}

/// `scalar` matches any key; otherwise compare case-insensitively.
fn key_matches(table: &StringTable, id: StringId, key: &str) -> bool {
    key == "scalar"
        || table
            .with_string(id, |s| s.eq_ignore_ascii_case(key))
            .unwrap_or(false)
}

/// Walk one level of a complex-enum name tree against `children`, capturing
/// member names per the marker forms:
///   * `enum_name = { ... }` (Node, key `enum_name`)  -> each clause child's KEY
///   * `enum_name = scalar`  (Leaf, key `enum_name`)  -> each scalar leaf's KEY
///   * `key = enum_name`     (Leaf, is_name)          -> matching leaves' VALUE
///   * bare `enum_name`      (BareName)               -> each bare value
///
/// `scalar` as a Node key is a wildcard (descend every clause child).
fn walk_name_tree(
    tree: &ComplexEnumNameTree,
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    out: &mut Vec<String>,
) {
    let ComplexEnumNameTree::Entries(entries) = tree else {
        return;
    };
    for entry in entries {
        match entry {
            ComplexEnumNameTreeEntry::Node {
                key,
                children: inner,
            } => {
                for child in children {
                    let Child::Leaf(idx) = child else { continue };
                    let leaf = &ast.arena.leaves[*idx as usize];
                    let Value::Clause(sub) = &leaf.value else {
                        continue;
                    };
                    if key == "enum_name" {
                        push_unquoted_key(out, table, leaf.key.normal);
                    } else if key_matches(table, leaf.key.normal, key) {
                        walk_name_tree(inner, sub, ast, table, out);
                    }
                }
            }
            ComplexEnumNameTreeEntry::Leaf { key, is_name } => {
                for child in children {
                    let Child::Leaf(idx) = child else { continue };
                    let leaf = &ast.arena.leaves[*idx as usize];
                    if matches!(leaf.value, Value::Clause(_)) {
                        continue;
                    }
                    if *is_name {
                        // `key = enum_name`: the VALUE of matching leaves.
                        if key_matches(table, leaf.key.normal, key)
                            && let Value::String(t) | Value::QString(t) = &leaf.value
                            && let Some(v) = table.get_string(t.normal)
                        {
                            out.push(unquote(&v).to_string());
                        }
                    } else if key == "enum_name" {
                        // `enum_name = scalar`: each scalar leaf's KEY.
                        push_unquoted_key(out, table, leaf.key.normal);
                    }
                }
            }
            ComplexEnumNameTreeEntry::BareName => {
                for child in children {
                    let Child::LeafValue(idx) = child else {
                        continue;
                    };
                    let lv = &ast.arena.leaf_values[*idx as usize];
                    if let Value::String(t) | Value::QString(t) = &lv.value
                        && let Some(v) = table.get_string(t.normal)
                    {
                        out.push(unquote(&v).to_string());
                    }
                }
            }
        }
    }
}

/// Collect `value_set[...]` members written by one parsed file, keyed by
/// namespace. Uses `ruleset.value_set_effects` (built by `reindex()`): for a
/// leaf like `set_country_flag = my_flag` the scalar value is the member; for
/// the block form, a `flag`/`name`/`token`/`var`/`variable` child carries it.
pub fn collect_value_set_members(
    ruleset: &RuleSet,
    parsed: &ParsedFile,
    table: &StringTable,
) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    if ruleset.value_set_effects().is_empty() {
        return out;
    }
    collect_value_sets_in(&parsed.root_children, parsed, ruleset, table, &mut out);
    out
}

fn collect_value_sets_in(
    children: &[Child],
    ast: &ParsedFile,
    ruleset: &RuleSet,
    table: &StringTable,
    out: &mut HashMap<String, Vec<String>>,
) {
    for child in children {
        let Child::Leaf(idx) = child else { continue };
        let leaf = &ast.arena.leaves[*idx as usize];
        let ns = value_set_leaf(leaf, ast, ruleset, table, out);
        // Descend into every clause EXCEPT a `variable`-namespace block, whose
        // members are collected elsewhere (CW246 + `all_variables`).
        if let Value::Clause(sub) = &leaf.value
            && ns.as_deref() != Some("variable")
        {
            collect_value_sets_in(sub, ast, ruleset, table, out);
        }
    }
}

/// Non-recursive value-set work for one leaf child. Looks the leaf's key up in
/// `value_set_effects`; if it names a set, captures the member from a scalar RHS
/// or (block form) from the block's direct children — a binding-field match, or
/// the fixed `flag`/`name`/`token`/… heuristic. Returns the namespace the key
/// maps to so the caller can decide whether to descend (a `variable`-namespace
/// block is skipped — variables have their own collection path). `None` when the
/// key names no set.
///
/// The per-node core shared by [`collect_value_set_members`] and the fused index
/// walk (`crate::collect`); the recursion into clause children stays with the
/// caller so both walk shapes reuse it.
pub(crate) fn value_set_leaf(
    leaf: &Leaf,
    ast: &ParsedFile,
    ruleset: &RuleSet,
    table: &StringTable,
    out: &mut HashMap<String, Vec<String>>,
) -> Option<String> {
    thread_local! {
        static LOWER_BUF: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    }
    let ns = table
        .with_string(leaf.key.normal, |s| {
            // Lowercase into a reused thread-local buffer instead of allocating
            // a String per leaf just to probe the (lowercased-key) effect map.
            LOWER_BUF.with(|buf| {
                let mut key = buf.borrow_mut();
                key.clear();
                key.extend(s.chars().map(|c| c.to_ascii_lowercase()));
                ruleset.value_set_effects().get(key.as_str()).cloned()
            })
        })
        .flatten();
    match (&leaf.value, ns.as_deref()) {
        // Variables have their own collection path (CW246 + completion via
        // all_variables); skip them here to avoid double-storing 100k names.
        (_, Some("variable")) => {}
        (Value::String(t) | Value::QString(t), Some(n)) => {
            table.with_string(t.normal, |v| push_member(out, n.to_string(), v));
        }
        (Value::Clause(sub), Some(n)) => {
            // Block form: the member is the value of the child bound to
            // `value_set[ns]` in the rules (e.g. `flag`, `token_base`, `id`).
            let bindings = table
                .with_string(leaf.key.normal, |s| {
                    LOWER_BUF.with(|buf| {
                        let mut key = buf.borrow_mut();
                        key.clear();
                        key.extend(s.chars().map(|c| c.to_ascii_lowercase()));
                        ruleset.value_set_effect_fields().get(key.as_str()).cloned()
                    })
                })
                .flatten();
            if let Some(bindings) = bindings.filter(|b| !b.is_empty()) {
                // Capture every binding field's value into its declared set.
                // A single block can bind several sets (different keys → sets).
                for c in sub {
                    let Child::Leaf(cidx) = c else { continue };
                    let cl = &ast.arena.leaves[*cidx as usize];
                    let (Value::String(t) | Value::QString(t)) = &cl.value else {
                        continue;
                    };
                    let field_ns = table
                        .with_string(cl.key.normal, |s| {
                            bindings
                                .iter()
                                .find(|(fk, _)| s.eq_ignore_ascii_case(fk))
                                .map(|(_, n)| n.clone())
                        })
                        .flatten();
                    if let Some(field_ns) = field_ns
                        && field_ns != "variable"
                    {
                        table.with_string(t.normal, |v| push_member(out, field_ns, v));
                    }
                }
            } else {
                // No binding-field info — fall back to the fixed-key heuristic
                // for the common flag/name/token block shapes.
                const NAME_KEYS: &[&str] = &["flag", "name", "token", "var", "variable"];
                for c in sub {
                    let Child::Leaf(cidx) = c else { continue };
                    let cl = &ast.arena.leaves[*cidx as usize];
                    let is_name_key = table
                        .with_string(cl.key.normal, |s| {
                            NAME_KEYS.iter().any(|k| s.eq_ignore_ascii_case(k))
                        })
                        .unwrap_or(false);
                    if is_name_key
                        && let Value::String(t) | Value::QString(t) = &cl.value
                        && table
                            .with_string(t.normal, |v| push_member(out, n.to_string(), v))
                            .is_some()
                    {
                        break;
                    }
                }
            }
        }
        _ => {}
    }
    ns
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_parser::parser::parse_string;
    use cwtools_rules::rules_converter::ast_to_ruleset;

    fn ruleset_from(cwt: &str, table: &StringTable) -> RuleSet {
        let parsed = parse_string(cwt, table);
        ast_to_ruleset(&parsed, table)
    }

    #[test]
    fn complex_enum_bare_name_captures_values() {
        // The equipment_stat shape: descend a specific key, capture bare values.
        let table = StringTable::new();
        let rs = ruleset_from(
            r#"
enums = {
    complex_enum[equipment_stat] = {
        path = "game/common"
        path_file = "script_enums.txt"
        start_from_root = yes
        name = {
            script_enum_equipment_stat = {
                enum_name
            }
        }
    }
}
"#,
            &table,
        );
        let file = parse_string(
            "script_enum_equipment_stat = {\n\tbuild_cost_ic\n\treliability\n}\n",
            &table,
        );
        let got = collect_complex_enum_values(&rs, &file, "common/script_enums.txt", &table);
        let vals = got.get("equipment_stat").expect("collected");
        assert!(
            vals.contains(&"build_cost_ic".to_string()),
            "got {:?}",
            vals
        );
        assert!(vals.contains(&"reliability".to_string()));
    }

    #[test]
    fn complex_enum_name_in_key_scalar_captures_keys() {
        // The country_tags shape: `enum_name = scalar` captures leaf keys.
        let table = StringTable::new();
        let rs = ruleset_from(
            r#"
enums = {
    complex_enum[country_tags] = {
        path = "game/common/country_tags"
        start_from_root = yes
        name = {
            enum_name = scalar
        }
    }
}
"#,
            &table,
        );
        let file = parse_string(
            "BRA = \"countries/Brazil.txt\"\nGER = \"countries/Germany.txt\"\n",
            &table,
        );
        let got =
            collect_complex_enum_values(&rs, &file, "common/country_tags/00_tags.txt", &table);
        let vals = got.get("country_tags").expect("collected");
        assert!(vals.contains(&"BRA".to_string()), "got {:?}", vals);
        assert!(vals.contains(&"GER".to_string()));
    }

    #[test]
    fn complex_enum_scalar_wildcard_and_block_name() {
        // The idea_name shape: wildcard descend, `enum_name = {}` captures
        // clause keys; scalar siblings (designer = yes) are NOT captured.
        let table = StringTable::new();
        let rs = ruleset_from(
            r#"
enums = {
    complex_enum[idea_name] = {
        path = "game/common/ideas"
        name = {
            scalar = {
                enum_name = {
                }
            }
        }
    }
}
"#,
            &table,
        );
        let file = parse_string(
            "ideas = {\n\tcountry = {\n\t\tdesigner = yes\n\t\tmy_idea = { cost = 1 }\n\t}\n}\n",
            &table,
        );
        let got = collect_complex_enum_values(&rs, &file, "common/ideas/test.txt", &table);
        let vals = got.get("idea_name").expect("collected");
        assert!(vals.contains(&"my_idea".to_string()), "got {:?}", vals);
        assert!(!vals.contains(&"designer".to_string()), "got {:?}", vals);
    }

    #[test]
    fn value_set_members_scalar_block_and_datestamp() {
        let table = StringTable::new();
        let mut rs = ruleset_from(
            r#"
alias[effect:set_country_flag] = value_set[country_flag]
alias[effect:set_country_flag] = {
    flag = value_set[country_flag]
    value = int
}
"#,
            &table,
        );
        rs.reindex();
        let file = parse_string(
            "my_effect = {\n\tset_country_flag = simple_flag\n\tset_country_flag = stamped_flag@1936.1.1\n\tset_country_flag = { flag = block_flag value = 2 }\n}\n",
            &table,
        );
        let got = collect_value_set_members(&rs, &file, &table);
        let vals = got.get("country_flag").expect("collected");
        assert!(vals.contains(&"simple_flag".to_string()), "got {:?}", vals);
        assert!(vals.contains(&"stamped_flag".to_string()));
        assert!(vals.contains(&"block_flag".to_string()));
    }

    #[test]
    fn value_set_member_under_nonobvious_block_key() {
        // The member lives under `token_base`, which is NOT one of the fixed
        // NAME_KEYS guesses. The collector must read the field actually bound
        // to `value_set[character_token]` in the rules, and must NOT capture the
        // sibling `name = localisation` value as a token.
        let table = StringTable::new();
        let mut rs = ruleset_from(
            r#"
alias[effect:generate_character] = {
    token_base = value_set[character_token]
    name = localisation
}
"#,
            &table,
        );
        rs.reindex();
        let file = parse_string(
            "my_effect = {\n\tgenerate_character = {\n\t\ttoken_base = empowered_legislative\n\t\tname = NAME_x\n\t}\n}\n",
            &table,
        );
        let got = collect_value_set_members(&rs, &file, &table);
        let tokens = got
            .get("character_token")
            .expect("character_token collected");
        assert!(
            tokens.contains(&"empowered_legislative".to_string()),
            "token_base value must be collected, got {tokens:?}",
        );
        assert!(
            !tokens.contains(&"NAME_x".to_string()),
            "the name= sibling must not be collected as a token, got {tokens:?}",
        );
    }

    #[test]
    fn named_value_index_refcounts_per_file() {
        let mut idx = NamedValueIndex::new();
        let items = |v: &str| HashMap::from([("country_flag".to_string(), vec![v.to_string()])]);
        idx.merge_file("a.txt", items("shared"));
        idx.merge_file("b.txt", items("shared"));
        idx.remove_file("a.txt");
        assert!(idx.values("country_flag").any(|v| v == "shared"));
        idx.remove_file("b.txt");
        assert!(idx.values("country_flag").next().is_none());
    }

    #[test]
    fn clone_is_independent() {
        let mut idx = NamedValueIndex::new();
        idx.merge_file(
            "a.txt",
            HashMap::from([("country_flag".to_string(), vec!["a_flag".to_string()])]),
        );
        let snap = idx.clone();
        let mut mutated = snap.clone();
        mutated.merge_file(
            "b.txt",
            HashMap::from([("country_flag".to_string(), vec!["b_flag".to_string()])]),
        );
        assert!(!idx.contains("country_flag", "b_flag"));
        assert!(mutated.contains("country_flag", "b_flag"));
        assert!(snap.contains("country_flag", "a_flag"));
        assert!(!snap.contains("country_flag", "b_flag"));
        // Removing from original must not affect clone.
        idx.remove_file("a.txt");
        assert!(!idx.contains("country_flag", "a_flag"));
        assert!(snap.contains("country_flag", "a_flag"));
    }

    #[test]
    fn clone_multi_namespace_is_independent() {
        let mut idx = NamedValueIndex::new();
        idx.merge_file(
            "a.txt",
            HashMap::from([
                ("country_flag".to_string(), vec!["a_flag".to_string()]),
                ("global_flag".to_string(), vec!["g_flag".to_string()]),
            ]),
        );
        let snap = idx.clone();
        let mut mutated = snap.clone();
        mutated.merge_file(
            "b.txt",
            HashMap::from([("country_flag".to_string(), vec!["b_flag".to_string()])]),
        );
        assert!(!idx.contains("country_flag", "b_flag"));
        assert!(mutated.contains("country_flag", "b_flag"));
        assert!(snap.contains("global_flag", "g_flag"));
        // export() must not alias
        let snap_export = snap.export();
        let idx_export = idx.export();
        assert_eq!(snap_export.len(), idx_export.len());
    }
}
