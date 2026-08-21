//! Enum extraction: `enums = { enum[x] = { ... } complex_enum[x] = { ... } }`
//! and the related `values = { value[x] = { ... } }` block.

use super::*;

pub(crate) fn extract_enums_from_children(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    ruleset: &mut RuleSet,
) {
    let precomputed = precompute_comments(children, ast, table);
    for (idx, echild) in children.iter().enumerate() {
        let comments = &precomputed[idx];
        let Child::Leaf(lidx) = echild else {
            continue;
        };
        let leaf = &ast.arena.leaves[*lidx as usize];
        let key = table.get_string(leaf.key.normal).unwrap_or_default();
        if key.starts_with("enum[") {
            if let Some(enum_name) = extract_bracket_content(&key, "enum") {
                let def = process_enum_node(enum_name, leaf, ast, table, comments);
                ruleset.enums.push(def);
            }
        } else if key.starts_with("complex_enum[")
            && let Some(enum_name) = extract_bracket_content(&key, "complex_enum")
            && let Value::Clause(ch) = &leaf.value
        {
            let def = process_complex_enum_from_children(enum_name, ch, ast, table, comments);
            ruleset.complex_enums.push(def);
        }
    }
}

pub(crate) fn extract_values_from_children(
    children: &Vec<Child>,
    ast: &ParsedFile,
    table: &StringTable,
    ruleset: &mut RuleSet,
) {
    for vchild in children {
        let Child::Leaf(lidx) = vchild else {
            continue;
        };
        let leaf = &ast.arena.leaves[*lidx as usize];
        let key = table.get_string(leaf.key.normal).unwrap_or_default();
        if key.starts_with("value[")
            && let Some(value_name) = extract_bracket_content(&key, "value")
        {
            let vals = collect_leaf_values_from_clause(&leaf.value, ast, table);
            ruleset.values.entry(value_name).or_default().extend(vals);
        }
    }
}

pub(crate) fn process_enum_node(
    name: String,
    leaf: &cwtools_parser::ast::Leaf,
    ast: &ParsedFile,
    table: &StringTable,
    comments: &[String],
) -> EnumDefinition {
    let mut values = Vec::new();

    if let Value::Clause(children) = &leaf.value {
        for child in children {
            match child {
                Child::LeafValue(lvidx) => {
                    let lv = &ast.arena.leaf_values[*lvidx as usize];
                    let v = value_to_string(&lv.value, table);
                    if !v.is_empty() {
                        values.push(v);
                    }
                }
                Child::Leaf(lidx) => {
                    let l = &ast.arena.leaves[*lidx as usize];
                    let v = table.get_string(l.key.normal).unwrap_or_default();
                    if !v.is_empty() {
                        values.push(v);
                    }
                }
                _ => {}
            }
        }
    }

    // Description from ### or ## comments
    let description = extract_description_from_comments(comments).unwrap_or_else(|| name.clone());

    EnumDefinition {
        key: name,
        description,
        values,
    }
}

pub(crate) fn process_complex_enum_from_children(
    name: String,
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    comments: &[String],
) -> ComplexEnumDef {
    let mut paths: Vec<String> = Vec::new();
    let mut path_strict = false;
    let mut path_file = None;
    let mut path_extension = None;
    let mut start_from_root = false;
    let mut name_tree: Option<ComplexEnumNameTree> = None;

    for child in children {
        if let Child::Leaf(lidx) = child {
            let l = &ast.arena.leaves[*lidx as usize];
            let k = table.get_string(l.key.normal).unwrap_or_default();
            // Handle `name = { ... }` as a Leaf with Clause value
            if k == "name"
                && let Value::Clause(name_ch) = &l.value
            {
                name_tree = Some(build_name_tree(name_ch, ast, table));
                continue;
            }
            let v = leaf_value_string(l, table);
            match k.as_str() {
                "path" => paths.push(clean_path(&v)),
                "path_strict" if v == "yes" => {
                    path_strict = true;
                }
                "path_file" => {
                    path_file = Some(v);
                }
                "path_extension" => {
                    path_extension = Some(v);
                }
                "start_from_root" if v == "yes" => {
                    start_from_root = true;
                }
                _ => {}
            }
        }
    }

    let description = extract_description_from_comments(comments).unwrap_or_else(|| name.clone());

    ComplexEnumDef {
        name,
        description,
        path_options: PathOptions {
            paths,
            path_strict,
            path_file,
            path_extension,
            paths_lower: Vec::new(),
            ..Default::default()
        },
        name_tree: name_tree.unwrap_or(ComplexEnumNameTree::Empty),
        start_from_root,
    }
}

/// Build a ComplexEnumNameTree from the `name = { ... }` block children.
fn build_name_tree(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
) -> ComplexEnumNameTree {
    let mut entries = Vec::new();
    for child in children {
        match child {
            Child::Leaf(lidx) => {
                let l = &ast.arena.leaves[*lidx as usize];
                let k = table.get_string(l.key.normal).unwrap_or_default();
                // Leaf with Clause value = nested node in CWT
                if let Value::Clause(sub_ch) = &l.value {
                    let sub = build_name_tree(sub_ch, ast, table);
                    entries.push(ComplexEnumNameTreeEntry::Node {
                        key: k,
                        children: sub,
                    });
                } else {
                    let v = leaf_value_string(l, table);
                    entries.push(ComplexEnumNameTreeEntry::Leaf {
                        key: k,
                        is_name: v == "enum_name" || v == "this",
                    });
                }
            }
            // A bare `enum_name` value (`stats = { enum_name }`): every bare
            // value at this level of the target file is an enum member.
            Child::LeafValue(lvidx) => {
                let lv = &ast.arena.leaf_values[*lvidx as usize];
                if let Value::String(t) | Value::QString(t) = &lv.value
                    && table
                        .with_string(t.normal, |s| s == "enum_name" || s == "this")
                        .unwrap_or(false)
                {
                    entries.push(ComplexEnumNameTreeEntry::BareName);
                }
            }
            _ => {}
        }
    }
    ComplexEnumNameTree::Entries(entries)
}
