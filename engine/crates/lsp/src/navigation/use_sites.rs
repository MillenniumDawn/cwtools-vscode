use std::collections::HashMap;

use cwtools_rules::rules_types::{NewField, RootRule, RuleSet, RuleType};
use cwtools_string_table::string_table::StringTable;

use crate::ParsedDoc;
use crate::paths::logical_path_from_uri;

use super::unquote;

/// Scan all documents indexed in `info` (whose text is in `docs`) for leaves
/// whose value equals `instance_name` and whose rule context is a TypeField
/// for `type_name`.
///
/// Returns a list of (file_uri, SourceLocation) use-sites.
///
/// Implementation: walks every leaf in every indexed file's AST.  For each
/// leaf whose value equals the target name, `is_type_ref_leaf` classifies the
/// key against the ruleset; matches are recorded as use-sites.
///
/// This is O(files × leaves) but runs only on demand (find-references / rename)
/// so is acceptable for mod-sized workspaces.
pub(crate) fn scan_use_sites(
    type_name: &str,
    instance_name: &str,
    docs: &HashMap<String, ParsedDoc>,
    ruleset: &RuleSet,
    workspace_prefix: &Option<std::sync::Arc<str>>,
    string_table: &cwtools_string_table::string_table::StringTable,
) -> Vec<(String, cwtools_info::SourceLocation)> {
    let mut results = Vec::new();

    for (file_uri, parsed_doc) in docs {
        let ast = match &parsed_doc.ast {
            Some(a) => a,
            None => continue,
        };
        let logical_path = logical_path_from_uri(file_uri, workspace_prefix);

        scan_ast_for_type_ref(
            &ast.root_children,
            &ast.arena,
            &TypeRefSearch {
                type_name,
                instance_name,
                file_uri,
                ruleset,
                logical_path: &logical_path,
                table: string_table,
            },
            &mut results,
        );
    }

    results
}

/// Recursively walk children and record leaves whose value classifies as a
/// TypeRef for the specified type+name.
/// What [`scan_ast_for_type_ref`] is looking for: the reference target plus the
/// rules/table/path needed to classify a candidate. Invariant across the walk of
/// one file, so it is threaded by reference through the recursion.
struct TypeRefSearch<'a> {
    type_name: &'a str,
    instance_name: &'a str,
    file_uri: &'a str,
    ruleset: &'a RuleSet,
    logical_path: &'a str,
    table: &'a StringTable,
}

fn scan_ast_for_type_ref(
    children: &[cwtools_parser::ast::Child],
    arena: &cwtools_parser::ast::Arena,
    search: &TypeRefSearch,
    out: &mut Vec<(String, cwtools_info::SourceLocation)>,
) {
    use cwtools_parser::ast::{Child, Value};
    let &TypeRefSearch {
        type_name,
        instance_name,
        file_uri,
        ruleset,
        logical_path,
        table,
    } = search;

    // Only keyed leaves are classified; LeafValue type refs would need
    // parent-context classification, which this shallow walk doesn't do.
    for child in children {
        let Child::Leaf(idx) = child else { continue };
        let leaf = &arena.leaves[*idx as usize];
        let key = table.get_string(leaf.key.normal).unwrap_or_default();
        let raw_val = match &leaf.value {
            Value::String(t) | Value::QString(t) => table.get_string(t.normal).unwrap_or_default(),
            _ => String::new(),
        };
        let val = unquote(&raw_val);
        if val == instance_name && is_type_ref_leaf(ruleset, &key, type_name, logical_path) {
            out.push((
                file_uri.to_string(),
                cwtools_info::SourceLocation {
                    line: leaf.pos.start.line,
                    col: leaf.pos.start.col,
                    end: (leaf.pos.end.line, leaf.pos.end.col),
                },
            ));
        }
        // Recurse into clause values
        if let Value::Clause(ch) = &leaf.value {
            scan_ast_for_type_ref(ch, arena, search, out);
        }
    }
}

/// Check if a leaf with key `leaf_key` is a TypeField reference to `type_name`.
/// Uses the ruleset's depth-one leaf-key lookup when available. Hand-built
/// rulesets that have not been reindexed retain the direct root-rule scan.
pub(crate) fn is_type_ref_leaf(
    ruleset: &RuleSet,
    leaf_key: &str,
    type_name: &str,
    logical_path: &str,
) -> bool {
    if !ruleset.type_reference_rules().is_empty() {
        return ruleset
            .type_reference_rules_for_key(leaf_key)
            .is_some_and(|entries| {
                entries.iter().any(|entry| {
                    if entry.ref_type != type_name {
                        return false;
                    }
                    match &entry.root_type {
                        None => true,
                        Some(root_type) => ruleset
                            .type_by_name()
                            .get(root_type)
                            .map(|&idx| {
                                cwtools_info::check_path_dir(
                                    &ruleset.types[idx].path_options,
                                    logical_path,
                                )
                            })
                            // Preserve the legacy scan: a TypeRule without a
                            // matching TypeDefinition has no path gate.
                            .unwrap_or(true),
                    }
                })
            });
    }

    for root_rule in &ruleset.root_rules {
        let (rule_type_name, (rule_type, _)) = match root_rule {
            RootRule::TypeRule(n, r) => (Some(n.as_str()), r),
            RootRule::AliasRule(n, r) => (Some(n.as_str()), r),
            RootRule::SingleAliasRule(n, r) => (Some(n.as_str()), r),
        };

        // For TypeRules, check path filter
        if let RootRule::TypeRule(..) = root_rule
            && let Some(name) = rule_type_name
            && let Some(&idx) = ruleset.type_by_name().get(name)
        {
            let td = &ruleset.types[idx];
            if !cwtools_info::check_path_dir(&td.path_options, logical_path) {
                continue;
            }
        }

        let rules = match rule_type {
            RuleType::NodeRule { rules, .. } => rules.as_ref(),
            _ => continue,
        };

        for (inner, _) in rules {
            if let RuleType::LeafRule {
                left: NewField::SpecificField(k),
                right: NewField::TypeField(cwtools_rules::rules_types::TypeType::Simple(t)),
            } = inner
                && k.eq_ignore_ascii_case(leaf_key)
                && t == type_name
            {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cwtools_parser::parser::parse_string;
    use cwtools_rules::rules_types::{
        EnumDefinition, Options, PathOptions, TypeDefinition, ValueType,
    };

    use super::*;

    fn make_leaf_rule(key: &str, right: NewField) -> cwtools_rules::rules_types::NewRule {
        (
            RuleType::LeafRule {
                left: NewField::SpecificField(key.to_string()),
                right,
            },
            Options::default(),
        )
    }

    fn make_node_rule(
        key: &str,
        children: Vec<cwtools_rules::rules_types::NewRule>,
    ) -> cwtools_rules::rules_types::NewRule {
        (
            RuleType::NodeRule {
                left: NewField::SpecificField(key.to_string()),
                rules: children.into(),
            },
            Options::default(),
        )
    }

    fn bool_enum_ruleset() -> RuleSet {
        let mut rs = RuleSet::new();

        rs.enums.push(EnumDefinition {
            key: "my_enum".to_string(),
            description: String::new(),
            values: vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
        });

        rs.types.push(TypeDefinition {
            name: "my_type".to_string(),
            name_field: Some("id".to_string()),
            path_options: PathOptions {
                paths: vec!["events".to_string()],
                path_strict: false,
                path_file: None,
                path_extension: None,
                paths_lower: Vec::new(),
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
        });

        rs.root_rules.push(RootRule::TypeRule(
            "my_type".to_string(),
            make_node_rule(
                "my_type",
                vec![
                    make_leaf_rule(
                        "kind",
                        NewField::ValueField(ValueType::Enum("my_enum".to_string())),
                    ),
                    make_leaf_rule("active", NewField::ValueField(ValueType::Bool)),
                    make_leaf_rule("name", NewField::ScalarField),
                ],
            ),
        ));

        rs.reindex();
        rs
    }

    #[test]
    fn test_is_type_ref_leaf() {
        let mut rs = bool_enum_ruleset();
        // Add a TypeRule with a leaf that references type "my_type"
        rs.root_rules.push(RootRule::TypeRule(
            "owner_type".to_string(),
            (
                RuleType::NodeRule {
                    left: NewField::SpecificField("owner_type".to_string()),
                    rules: [(
                        RuleType::LeafRule {
                            left: NewField::SpecificField("base".to_string()),
                            right: NewField::TypeField(
                                cwtools_rules::rules_types::TypeType::Simple("my_type".to_string()),
                            ),
                        },
                        Options::default(),
                    )]
                    .into(),
                },
                Options::default(),
            ),
        ));
        rs.reindex();

        // "base" field referencing "my_type" should be recognized
        assert!(is_type_ref_leaf(&rs, "base", "my_type", "events/test.txt"));
        // "base" field referencing a different type should not match
        assert!(!is_type_ref_leaf(
            &rs,
            "base",
            "other_type",
            "events/test.txt"
        ));
        // unrelated field should not match
        assert!(!is_type_ref_leaf(
            &rs,
            "unrelated",
            "my_type",
            "events/test.txt"
        ));
    }

    #[test]
    fn test_scan_use_sites() {
        let table = StringTable::new();
        // Nested: foo node containing a leaf "base = my_instance"
        let source = "foo = { base = my_instance }\n";
        let parsed = parse_string(source, &table);

        let mut rs = bool_enum_ruleset();
        // Use an AliasRule (not path-filtered) that contains base -> TypeField(my_type)
        rs.root_rules.push(RootRule::AliasRule(
            "effect:use_type".to_string(),
            (
                RuleType::NodeRule {
                    left: NewField::SpecificField("use_type".to_string()),
                    rules: [(
                        RuleType::LeafRule {
                            left: NewField::SpecificField("base".to_string()),
                            right: NewField::TypeField(
                                cwtools_rules::rules_types::TypeType::Simple("my_type".to_string()),
                            ),
                        },
                        Options::default(),
                    )]
                    .into(),
                },
                Options::default(),
            ),
        ));
        rs.reindex();

        let mut docs = HashMap::new();
        docs.insert(
            "file:///test.txt".to_string(),
            ParsedDoc {
                version: 0,
                text: Arc::from(source),
                ast: Some(Arc::new(parsed)),
                ast_version: Some(0),
                ast_source_bytes: source.len(),
            },
        );

        let ws_uri: Option<std::sync::Arc<str>> = Some("file:///".into());
        let sites = scan_use_sites("my_type", "my_instance", &docs, &rs, &ws_uri, &table);
        assert!(!sites.is_empty(), "expected use sites, got none");
        assert!(
            sites.iter().any(|(uri, _)| uri == "file:///test.txt"),
            "expected correct uri"
        );
    }
}
