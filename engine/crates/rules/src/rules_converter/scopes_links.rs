use super::*;

pub(crate) fn extract_modifier_names(
    children: &Vec<Child>,
    ast: &ParsedFile,
    table: &StringTable,
    ruleset: &mut RuleSet,
) {
    for child in children {
        let Child::Leaf(lidx) = child else {
            continue;
        };
        let leaf = &ast.arena.leaves[*lidx as usize];
        let name = table.get_string(leaf.key.normal).unwrap_or_default();
        if !name.is_empty() {
            let category = value_to_string(&leaf.value, table);
            ruleset.modifiers.push((name, category));
        }
    }
}

pub(crate) fn extract_modifier_categories(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    ruleset: &mut RuleSet,
) {
    for child in children {
        let Some((name, body)) = entry_body(child, ast, table) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let scopes = child_clause_values(body, ast, table, "supported_scopes");
        ruleset.modifier_categories.insert(name, scopes);
    }
}

fn entry_body<'a>(
    child: &Child,
    ast: &'a ParsedFile,
    table: &StringTable,
) -> Option<(String, &'a [Child])> {
    let kc = ast.arena.keyed_clause(child)?;
    Some((
        table.get_string(kc.key.normal).unwrap_or_default(),
        kc.children,
    ))
}

fn child_clause_values(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    key: &str,
) -> Vec<String> {
    for child in children {
        if let Child::Leaf(lidx) = child {
            let l = &ast.arena.leaves[*lidx as usize];
            if table.get_string(l.key.normal).unwrap_or_default() == key {
                return collect_leaf_values_from_clause(&l.value, ast, table);
            }
        }
    }
    Vec::new()
}

fn child_scalar(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    key: &str,
) -> Option<String> {
    children.iter().find_map(|child| {
        if let Child::Leaf(lidx) = child {
            let l = &ast.arena.leaves[*lidx as usize];
            if table.get_string(l.key.normal).unwrap_or_default() == key
                && !matches!(l.value, Value::Clause(_))
            {
                return Some(value_to_string(&l.value, table));
            }
        }
        None
    })
}

fn child_scalars(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    key: &str,
) -> Vec<String> {
    children
        .iter()
        .filter_map(|child| {
            if let Child::Leaf(lidx) = child {
                let l = &ast.arena.leaves[*lidx as usize];
                if table.get_string(l.key.normal).unwrap_or_default() == key
                    && !matches!(l.value, Value::Clause(_))
                {
                    return Some(value_to_string(&l.value, table));
                }
            }
            None
        })
        .collect()
}

fn child_scope_list(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    key: &str,
) -> Vec<String> {
    let clause = child_clause_values(children, ast, table, key);
    if !clause.is_empty() {
        return clause;
    }
    child_scalar(children, ast, table, key)
        .into_iter()
        .collect()
}

pub(crate) fn extract_scope_defs(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    ruleset: &mut RuleSet,
) {
    for child in children {
        let Some((name, body)) = entry_body(child, ast, table) else {
            continue;
        };
        let name = name.trim_matches('"').to_string();
        if name.is_empty() {
            continue;
        }
        ruleset.scope_inputs.push(ScopeInput {
            aliases: child_clause_values(body, ast, table, "aliases"),
            is_subscope_of: child_clause_values(body, ast, table, "is_subscope_of"),
            name,
        });
    }
}

pub(crate) fn extract_localisation_commands(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    ruleset: &mut RuleSet,
) {
    for child in children {
        let Child::Leaf(lidx) = child else {
            continue;
        };
        let name = table
            .get_string(ast.arena.leaves[*lidx as usize].key.normal)
            .unwrap_or_default()
            .trim()
            .trim_matches('"')
            .to_string();
        if name.is_empty() || (name.starts_with('<') && name.ends_with('>')) {
            continue;
        }
        ruleset
            .localisation_commands
            .insert(name.to_ascii_lowercase());
    }
}

pub(crate) fn extract_links(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    ruleset: &mut RuleSet,
) {
    for child in children {
        let Some((name, body)) = entry_body(child, ast, table) else {
            if let Child::Leaf(lidx) = child {
                let n = table
                    .get_string(ast.arena.leaves[*lidx as usize].key.normal)
                    .unwrap_or_default();
                if !n.is_empty() {
                    ruleset.scope_links.insert(n.to_ascii_lowercase());
                }
            }
            continue;
        };
        let name = name.trim_matches('"').to_string();
        if name.is_empty() {
            continue;
        }
        let prefix = child_scalar(body, ast, table, "prefix");
        ruleset.scope_links.insert(name.to_ascii_lowercase());
        ruleset.link_inputs.push(LinkInput {
            output_scope: child_scalar(body, ast, table, "output_scope"),
            input_scopes: child_scope_list(body, ast, table, "input_scopes"),
            from_data: child_scalar(body, ast, table, "from_data")
                .is_some_and(|v| v.eq_ignore_ascii_case("yes")),
            data_source: child_scalars(body, ast, table, "data_source"),
            prefix,
            name,
        });
    }
}
