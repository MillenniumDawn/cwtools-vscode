use std::sync::Arc;

use cwtools_parser::ast::ParsedFile;
use cwtools_rules::rules_types::{NewField, RootRule, RuleSet, RuleType};
use cwtools_string_table::string_table::StringTable;

use crate::paths::logical_path_from_uri;

use super::unquote;

/// Walk open documents' ASTs for use sites of `instance_name`.
///
/// `docs` is a snapshot of `(uri, ast)` pairs rather than the document store
/// itself, so the caller can release the `documents` mutex before the walk —
/// every `did_change` needs that same mutex (#472).
pub(crate) fn scan_use_sites(
    type_name: &str,
    instance_name: &str,
    docs: &[(String, Arc<ParsedFile>)],
    ruleset: &RuleSet,
    workspace_prefix: &Option<Arc<str>>,
    string_table: &cwtools_string_table::string_table::StringTable,
    definitions: &[(String, cwtools_info::SourceLocation)],
) -> Vec<cwtools_info::UseSite> {
    let mut results = Vec::new();
    let patterns = cwtools_info::build_type_patterns(ruleset);

    for (file_uri, ast) in docs {
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
                patterns: &patterns,
                definitions,
            },
            &mut results,
        );
    }

    results
}

/// rules/table/path needed to classify a candidate. Invariant across the walk of
struct TypeRefSearch<'a> {
    type_name: &'a str,
    instance_name: &'a str,
    file_uri: &'a str,
    ruleset: &'a RuleSet,
    logical_path: &'a str,
    table: &'a StringTable,
    patterns: &'a [cwtools_info::TypePatternAlias],
    definitions: &'a [(String, cwtools_info::SourceLocation)],
}

fn scan_ast_for_type_ref(
    children: &[cwtools_parser::ast::Child],
    arena: &cwtools_parser::ast::Arena,
    search: &TypeRefSearch,
    out: &mut Vec<cwtools_info::UseSite>,
) {
    use cwtools_parser::ast::{Child, Value};
    let &TypeRefSearch {
        type_name,
        instance_name,
        file_uri,
        ruleset,
        logical_path,
        table,
        patterns,
        definitions,
    } = search;

    for child in children {
        let Child::Leaf(idx) = child else { continue };
        let leaf = &arena.leaves[*idx as usize];
        // Value first, key only on a match: `with_string` borrows from the
        // table instead of allocating a `String` per leaf, and the key is
        // needed only once the cheap name comparison has passed (#472). An
        // unresolvable id is no match — no caller passes an empty name.
        let quoted = match &leaf.value {
            Value::String(t) | Value::QString(t) => table
                .with_string(t.normal, |raw| {
                    let val = unquote(raw);
                    (val == instance_name).then_some(val.len() != raw.len())
                })
                .flatten(),
            _ => None,
        };
        if let Some(quoted) = quoted
            && table
                .with_string(leaf.key.normal, |key| {
                    is_type_ref_leaf(ruleset, key, type_name, logical_path)
                })
                .unwrap_or(false)
        {
            out.push(cwtools_info::UseSite {
                file: Arc::from(file_uri),
                key: cwtools_info::SourceLocation {
                    line: leaf.pos.start.line,
                    col: leaf.pos.start.col,
                    end: (leaf.pos.end.line, leaf.pos.end.col),
                },
                value: cwtools_info::value_location(&leaf.value_pos, quoted),
            });
        }
        let key_loc = cwtools_info::SourceLocation {
            line: leaf.pos.start.line,
            col: leaf.pos.start.col,
            end: (leaf.pos.end.line, leaf.pos.end.col),
        };
        let is_definition = definitions.iter().any(|(uri, loc)| {
            uri == file_uri && loc.line == key_loc.line && loc.col == key_loc.col
        });
        if !is_definition
            && table
                .with_string(leaf.key.normal, |key| {
                    cwtools_info::key_matches_type_pattern(patterns, type_name, instance_name, key)
                })
                .unwrap_or(false)
        {
            out.push(cwtools_info::UseSite {
                file: Arc::from(file_uri),
                key: key_loc,
                value: key_loc,
            });
        }
        if let Value::Clause(ch) = &leaf.value {
            scan_ast_for_type_ref(ch, arena, search, out);
        }
    }
}

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
    use cwtools_rules::rules_types::Options;

    use super::*;
    use crate::navigation::test_rules::{bool_enum_ruleset, type_ref_ruleset};

    #[test]
    fn test_is_type_ref_leaf() {
        let mut rs = bool_enum_ruleset();
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

        assert!(is_type_ref_leaf(&rs, "base", "my_type", "events/test.txt"));
        assert!(!is_type_ref_leaf(
            &rs,
            "base",
            "other_type",
            "events/test.txt"
        ));
        assert!(!is_type_ref_leaf(
            &rs,
            "unrelated",
            "my_type",
            "events/test.txt"
        ));
    }

    fn scan(source: &str) -> Vec<cwtools_info::UseSite> {
        let table = StringTable::new();
        let parsed = parse_string(source, &table);
        let docs = vec![("file:///test.txt".to_string(), Arc::new(parsed))];
        let ws_uri: Option<Arc<str>> = Some("file:///".into());
        scan_use_sites(
            "my_type",
            "my_instance",
            &docs,
            &type_ref_ruleset(),
            &ws_uri,
            &table,
            &[],
        )
    }

    #[test]
    fn test_scan_use_sites() {
        let sites = scan("foo = { base = my_instance }\n");
        assert_eq!(sites.len(), 1, "expected one use site");
        assert_eq!(sites[0].file.as_ref(), "file:///test.txt");
        // The leaf key `base` starts at column 8, the value at column 15.
        assert_eq!((sites[0].key.line, sites[0].key.col), (1, 8));
        assert_eq!((sites[0].value.line, sites[0].value.col), (1, 15));
    }

    #[test]
    fn scan_use_sites_points_inside_the_quotes_of_a_quoted_reference() {
        let sites = scan("foo = { base = \"my_instance\" }\n");
        assert_eq!(sites.len(), 1, "a quoted reference is still a use site");
        // The opening quote is at column 15; the name itself starts at 16, so a
        // caller can highlight the name without re-reading the file (#472).
        assert_eq!((sites[0].value.line, sites[0].value.col), (1, 16));
    }

    fn scripted_effect_ruleset() -> RuleSet {
        let table = StringTable::new();
        cwtools_rules::rules_converter::ast_to_ruleset(
            &parse_string(
                r#"
types = { type[scripted_effect] = { path = "game/common/scripted_effects" } }
scripted_effect = { alias_name[effect] = alias_match_left[effect] }
alias[effect:<scripted_effect>] = yes
"#,
                &table,
            ),
            &table,
        )
    }

    #[test]
    fn scan_use_sites_finds_scripted_effect_call_keys() {
        let table = StringTable::new();
        let parsed = parse_string("my_caller = { my_se = yes }\n", &table);
        let docs = vec![("file:///caller.txt".to_string(), Arc::new(parsed))];
        let ws_uri: Option<Arc<str>> = Some("file:///".into());
        let sites = scan_use_sites(
            "scripted_effect",
            "my_se",
            &docs,
            &scripted_effect_ruleset(),
            &ws_uri,
            &table,
            &[],
        );
        assert_eq!(sites.len(), 1, "expected the my_se = yes call");
        assert_eq!((sites[0].key.line, sites[0].key.col), (1, 14));
        assert_eq!(
            (sites[0].value.line, sites[0].value.col),
            (sites[0].key.line, sites[0].key.col)
        );
    }

    #[test]
    fn scan_use_sites_skips_the_scripted_effect_definition() {
        let table = StringTable::new();
        let parsed = parse_string("my_se = { log = hi }\n", &table);
        let docs = vec![("file:///e.txt".to_string(), Arc::new(parsed))];
        let ws_uri: Option<Arc<str>> = Some("file:///".into());
        let definition = cwtools_info::SourceLocation {
            line: 1,
            col: 0,
            end: (1, 0),
        };
        let sites = scan_use_sites(
            "scripted_effect",
            "my_se",
            &docs,
            &scripted_effect_ruleset(),
            &ws_uri,
            &table,
            &[("file:///e.txt".to_string(), definition)],
        );
        assert!(sites.is_empty(), "the definition is not a use: {sites:?}");
    }
}
