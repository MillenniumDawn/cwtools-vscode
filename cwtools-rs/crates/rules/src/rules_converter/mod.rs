//! `.cwt` AST -> `RuleSet` conversion.
//!
//! The top-level entry point [`ast_to_ruleset`] dispatches each root block to a
//! focused submodule grouped by output kind:
//! - [`field_parser`] — the `.cwt` field-type string parser
//! - [`types`] — `type[x]` definitions
//! - [`enums`] — `enum[x]` / `complex_enum[x]` / `value[x]`
//! - [`subtypes`] — `subtype[x]` bodies and subtype-scoped loc/modifiers
//! - [`scopes_links`] — `scopes` / `links` / top-level `modifiers`
//! - [`comment_directives`] — `#`/`##`/`###` option and documentation parsing
//!
//! Shared helpers (comment precomputation, the recursive rule builder, value
//! stringification, bracket/alias parsing) stay here and are re-exported into the
//! submodules via `use super::*`.

mod comment_directives;
mod enums;
pub(crate) mod field_parser;
mod scopes_links;
mod subtypes;
mod types;

pub(crate) use comment_directives::*;
pub(crate) use enums::*;
pub(crate) use field_parser::*;
pub(crate) use scopes_links::*;
pub(crate) use subtypes::*;
pub(crate) use types::*;

use crate::rules_types::*;
use cwtools_parser::ast::{Child, ParsedFile, Value};
use cwtools_string_table::string_table::StringTable;

// ±1e12 sentinel for unranged float; 1e6 was too narrow (build costs, populations).
const FLOAT_MAX: f64 = 1e12;
const FLOAT_MIN: f64 = -1e12;
const INT_MAX: i32 = 2_147_483_647;
const INT_MIN: i32 = -2_147_483_648;

/// Precompute comment text directly preceding every child in a single O(N) pass.
/// `result[i]` is the list of comments before child `i` (may be empty).
pub(crate) fn precompute_comments(
    children: &[Child],
    ast: &ParsedFile,
    _table: &StringTable,
) -> Vec<Vec<String>> {
    let mut result = vec![Vec::new(); children.len()];
    let mut pending: Vec<String> = Vec::new();
    for (i, child) in children.iter().enumerate() {
        match child {
            Child::Comment(cidx) => {
                let c = &ast.arena.comments[*cidx as usize];
                pending.push(c.text.trim().to_string());
            }
            _ => {
                if !pending.is_empty() {
                    result[i] = std::mem::take(&mut pending);
                }
            }
        }
    }
    result
}

/// Convert a parsed .cwt AST into a RuleSet.
pub fn ast_to_ruleset(ast: &ParsedFile, table: &StringTable) -> RuleSet {
    let mut ruleset = fill_ruleset(ast, table);
    ruleset.reindex();
    ruleset
}

/// Like `ast_to_ruleset` but skips the per-ruleset `reindex()` call.
/// Only safe when the caller is about to merge the result into a larger
/// `RuleSet` and will call `reindex()` on the final combined set.
pub(crate) fn ast_to_ruleset_raw(ast: &ParsedFile, table: &StringTable) -> RuleSet {
    // No reindex() — caller is responsible.
    fill_ruleset(ast, table)
}

fn fill_ruleset(ast: &ParsedFile, table: &StringTable) -> RuleSet {
    let mut ruleset = RuleSet::new();

    let precomputed = precompute_comments(&ast.root_children, ast, table);
    for (idx, child) in ast.root_children.iter().enumerate() {
        let comments = &precomputed[idx];

        if let Child::Leaf(lidx) = child {
            let leaf = &ast.arena.leaves[*lidx as usize];
            let key = table.get_string(leaf.key.normal).unwrap_or_default();
            match key.as_str() {
                "types" => {
                    if let Value::Clause(children) = &leaf.value {
                        extract_types_from_children(children, ast, table, &mut ruleset);
                    }
                }
                "enums" => {
                    if let Value::Clause(children) = &leaf.value {
                        extract_enums_from_children(children, ast, table, &mut ruleset);
                    }
                }
                "values" => {
                    if let Value::Clause(children) = &leaf.value {
                        extract_values_from_children(children, ast, table, &mut ruleset);
                    }
                }
                "modifiers" => {
                    if let Value::Clause(children) = &leaf.value {
                        extract_modifier_names(children, ast, table, &mut ruleset);
                    }
                }
                "modifier_categories" => {
                    if let Value::Clause(children) = &leaf.value {
                        extract_modifier_categories(children, ast, table, &mut ruleset);
                    }
                }
                "links" => {
                    if let Value::Clause(children) = &leaf.value {
                        extract_links(children, ast, table, &mut ruleset);
                    }
                }
                "scopes" => {
                    if let Value::Clause(children) = &leaf.value {
                        extract_scope_defs(children, ast, table, &mut ruleset);
                    }
                }
                "localisation_commands" => {
                    if let Value::Clause(children) = &leaf.value {
                        extract_localisation_commands(children, ast, table, &mut ruleset);
                    }
                }
                _ => {
                    process_root_leaf(key, leaf, ast, table, comments, &mut ruleset);
                }
            }
        }
    }
    ruleset
}

fn process_root_leaf(
    key: String,
    leaf: &cwtools_parser::ast::Leaf,
    ast: &ParsedFile,
    table: &StringTable,
    comments: &[String],
    ruleset: &mut RuleSet,
) {
    if key.starts_with("alias[") {
        if let Some((category, _alias_name)) = get_alias_settings(&key, "alias") {
            let full_name = format!("{}:{}", category, _alias_name);
            let rule = leaf_to_rule(leaf, ast, table);
            let opts = options_from_comments(comments, leaf_is_eqeq(leaf));
            ruleset.aliases.push((full_name, (rule, opts)));
        }
    } else if key.starts_with("single_alias[") {
        if let Some(alias_name) = get_setting_from_string(&key, "single_alias") {
            let rule = leaf_to_rule(leaf, ast, table);
            let opts = options_from_comments(comments, leaf_is_eqeq(leaf));
            ruleset.single_aliases.push((alias_name, (rule, opts)));
        }
    } else {
        let rule = leaf_to_rule(leaf, ast, table);
        let opts = options_from_comments(comments, leaf_is_eqeq(leaf));
        ruleset
            .root_rules
            .push(RootRule::TypeRule(key, (rule, opts)));
    }
}

fn leaf_is_eqeq(leaf: &cwtools_parser::ast::Leaf) -> bool {
    leaf.op == cwtools_parser::ast::Operator::EqualEqual
}

fn leaf_to_rule(
    leaf: &cwtools_parser::ast::Leaf,
    ast: &ParsedFile,
    table: &StringTable,
) -> RuleType {
    match &leaf.value {
        Value::Clause(children) => {
            let inner = children_to_rules(children, ast, table);
            RuleType::NodeRule {
                left: NewField::SpecificField(
                    table.get_string(leaf.key.normal).unwrap_or_default(),
                ),
                rules: inner.into(),
            }
        }
        _ => {
            let key_str = table.get_string(leaf.key.normal).unwrap_or_default();
            let left = field_from_string(&key_str);
            let right = field_from_string(&value_to_string(&leaf.value, table));
            RuleType::LeafRule { left, right }
        }
    }
}

pub(crate) fn children_to_rules(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
) -> Vec<NewRule> {
    let mut rules = Vec::new();
    let precomputed = precompute_comments(children, ast, table);
    for (idx, child) in children.iter().enumerate() {
        let comments = &precomputed[idx];
        match child {
            Child::Leaf(lidx) => {
                let leaf = &ast.arena.leaves[*lidx as usize];
                let key = table.get_string(leaf.key.normal).unwrap_or_default();

                if key.starts_with("subtype[") {
                    if let Some(st_name) = extract_bracket_content(&key, "subtype") {
                        let positive = !st_name.starts_with('!');
                        let name = if positive {
                            st_name
                        } else {
                            st_name[1..].to_string()
                        };
                        let inner = match &leaf.value {
                            Value::Clause(ch) => children_to_rules(ch, ast, table),
                            _ => Vec::new(),
                        };
                        rules.push((
                            RuleType::SubtypeRule {
                                name,
                                positive,
                                rules: inner.into(),
                            },
                            options_from_comments(comments, false),
                        ));
                    }
                    continue;
                }

                let is_eqeq = leaf.op == cwtools_parser::ast::Operator::EqualEqual;
                let opts = options_from_comments(comments, is_eqeq);
                let rule = match &leaf.value {
                    Value::Clause(ch) => {
                        let inner = children_to_rules(ch, ast, table);
                        RuleType::NodeRule {
                            left: field_from_string(&key),
                            rules: inner.into(),
                        }
                    }
                    _ => {
                        let right_str = value_to_string(&leaf.value, table);
                        // colour[rgb]/colour[hsv] special: expand inline to NodeRule
                        if right_str.starts_with("colour[") && right_str.ends_with(']') {
                            let colour_rules = build_colour_rules(&right_str);
                            RuleType::NodeRule {
                                left: field_from_string(&key),
                                rules: colour_rules.into(),
                            }
                        } else {
                            let left = field_from_string(&key);
                            let right = field_from_string(&right_str);
                            RuleType::LeafRule { left, right }
                        }
                    }
                };
                rules.push((rule, opts));
            }
            Child::LeafValue(lvidx) => {
                let lv = &ast.arena.leaf_values[*lvidx as usize];
                if let Value::Clause(clause_ch) = &lv.value {
                    // Anonymous {…} block in a rule definition — same as F# ValueClauseC.
                    let opts = options_from_comments(comments, false);
                    let inner = children_to_rules(clause_ch, ast, table);
                    rules.push((
                        RuleType::ValueClauseRule {
                            rules: inner.into(),
                        },
                        opts,
                    ));
                } else {
                    let val_str = value_to_string(&lv.value, table);
                    let field = field_from_string(&val_str);
                    let mut opts = options_from_comments(comments, false);
                    opts.leafvalue = true;
                    rules.push((RuleType::LeafValueRule { right: field }, opts));
                }
            }
            _ => {}
        }
    }
    rules
}

/// Build colour sub-rules for the inline `colour[rgb]` / `colour[hsv]` RHS
/// syntax (int 0-255 / float 0-2, 3-4 values). Distinct from the
/// `colour_field` MARKER expanded in `post_process::expand_colour_rule`
/// (float -256..256, exactly 3) — two different .cwt constructs with
/// deliberately different ranges, not a duplicate.
fn build_colour_rules(colour_spec: &str) -> Vec<NewRule> {
    let inner = if colour_spec.starts_with("colour[") && colour_spec.ends_with(']') {
        &colour_spec[7..colour_spec.len() - 1]
    } else {
        ""
    };
    match inner {
        "rgb" => vec![(
            RuleType::LeafValueRule {
                right: NewField::ValueField(ValueType::Int { min: 0, max: 255 }),
            },
            Options {
                min: 3,
                max: 4,
                strict_min: true,
                leafvalue: true,
                ..Options::default()
            },
        )],
        "hsv" => vec![(
            RuleType::LeafValueRule {
                right: NewField::ValueField(ValueType::Float { min: 0.0, max: 2.0 }),
            },
            Options {
                min: 3,
                max: 4,
                strict_min: true,
                leafvalue: true,
                ..Options::default()
            },
        )],
        _ => {
            // Unknown colour format — emit both
            vec![
                (
                    RuleType::LeafValueRule {
                        right: NewField::ValueField(ValueType::Int { min: 0, max: 255 }),
                    },
                    Options {
                        min: 3,
                        max: 4,
                        strict_min: true,
                        leafvalue: true,
                        ..Options::default()
                    },
                ),
                (
                    RuleType::LeafValueRule {
                        right: NewField::ValueField(ValueType::Float { min: 0.0, max: 2.0 }),
                    },
                    Options {
                        min: 3,
                        max: 4,
                        strict_min: true,
                        leafvalue: true,
                        ..Options::default()
                    },
                ),
            ]
        }
    }
}

pub(crate) fn collect_leaf_values_from_clause(
    value: &Value,
    ast: &ParsedFile,
    table: &StringTable,
) -> Vec<String> {
    if let Value::Clause(ch) = value {
        collect_leaf_values_from_children(ch, ast, table)
    } else {
        Vec::new()
    }
}

pub(crate) fn collect_leaf_values_from_children(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
) -> Vec<String> {
    let mut out = Vec::new();
    for child in children {
        match child {
            Child::LeafValue(lvidx) => {
                let lv = &ast.arena.leaf_values[*lvidx as usize];
                let v = value_to_string(&lv.value, table);
                if !v.is_empty() {
                    out.push(v);
                }
            }
            Child::Leaf(lidx) => {
                let l = &ast.arena.leaves[*lidx as usize];
                let v = table.get_string(l.key.normal).unwrap_or_default();
                if !v.is_empty() {
                    out.push(v);
                }
            }
            _ => {}
        }
    }
    out
}

pub(crate) fn extract_bracket_content(full: &str, prefix: &str) -> Option<String> {
    if let Some(body) = full.strip_prefix(prefix)
        && let Some(inner) = body.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
    {
        return Some(inner.to_string());
    }
    None
}

/// Strip one balanced pair of surrounding double quotes, nothing more.
/// `value_to_string` and the comment-directive readers share this so a quoted
/// directive value reads the same as the equivalent body value.
pub(crate) fn strip_quotes(s: &str) -> &str {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

pub(crate) fn value_to_string(value: &Value, table: &StringTable) -> String {
    match value {
        Value::String(t) | Value::QString(t) => {
            let s = table.get_string(t.normal).unwrap_or_default();
            strip_quotes(&s).to_string()
        }
        Value::Float(f) => f.to_string(),
        Value::Int(i) => i.to_string(),
        // CW script uses yes/no for booleans, not true/false
        Value::Bool(true) => "yes".to_string(),
        Value::Bool(false) => "no".to_string(),
        Value::Clause(_) => String::new(),
    }
}

fn get_alias_settings(full: &str, prefix: &str) -> Option<(String, String)> {
    let setting = get_setting_from_string(full, prefix)?;
    let parts: Vec<&str> = setting.splitn(2, ':').collect();
    if parts.len() < 2 {
        None
    } else {
        Some((parts[0].to_string(), parts[1].to_string()))
    }
}

fn get_setting_from_string(full: &str, key: &str) -> Option<String> {
    let expected = format!("{}[", key);
    if full.starts_with(&expected) && full.ends_with(']') {
        Some(full[expected.len()..full.len() - 1].to_string())
    } else {
        None
    }
}

pub(crate) fn clean_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    normalized
        .strip_prefix("game/")
        .unwrap_or(&normalized)
        .to_string()
}

pub(crate) fn leaf_value_string(leaf: &cwtools_parser::ast::Leaf, table: &StringTable) -> String {
    value_to_string(&leaf.value, table)
}

#[cfg(test)]
mod description_tests {
    use super::extract_description_from_comments;

    #[test]
    fn only_triple_hash_is_documentation() {
        // `## cardinality`/`## scope` are options and must not appear in the
        // hover tooltip; only `###` lines are documentation.
        let comments = vec![
            "### Numeric index of an ai_area (see common/ai_areas), not a name.".to_string(),
            "## cardinality = 0..1".to_string(),
            "## scope = country".to_string(),
        ];
        let desc = extract_description_from_comments(&comments).unwrap();
        assert_eq!(
            desc,
            "Numeric index of an ai_area (see common/ai_areas), not a name."
        );
        assert!(!desc.contains("cardinality"));
        assert!(!desc.contains("scope"));
    }

    #[test]
    fn multiple_doc_lines_join() {
        let comments = vec![
            "### First line.".to_string(),
            "## cardinality = 0..1".to_string(),
            "### Second line.".to_string(),
        ];
        assert_eq!(
            extract_description_from_comments(&comments).unwrap(),
            "First line.\nSecond line."
        );
    }

    #[test]
    fn no_doc_lines_yields_none() {
        let comments = vec![
            "## cardinality = 0..1".to_string(),
            "# plain note".to_string(),
        ];
        assert!(extract_description_from_comments(&comments).is_none());
    }
}
