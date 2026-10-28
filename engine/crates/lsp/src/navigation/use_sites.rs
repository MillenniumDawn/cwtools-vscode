use std::cell::OnceCell;
use std::sync::Arc;

#[cfg(test)]
thread_local! {
    static AST_LEAF_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_ast_leaf_visits() {
    AST_LEAF_VISITS.set(0);
}

#[cfg(test)]
pub(crate) fn ast_leaf_visits() -> usize {
    AST_LEAF_VISITS.get()
}

use cwtools_parser::{ast::ParsedFile, unquote};
use cwtools_rules::rules_types::RuleSet;
use cwtools_string_table::string_table::StringTable;

use crate::paths::logical_path_from_uri;

#[derive(Debug, Default)]
pub(crate) struct BoundedUseSites {
    pub(crate) sites: Vec<cwtools_info::UseSite>,
    pub(crate) total: usize,
    limit: usize,
}

pub(crate) struct BoundedUseSiteScan<'a> {
    pub(crate) type_name: &'a str,
    pub(crate) instance_name: &'a str,
    pub(crate) docs: &'a [(String, Arc<ParsedFile>)],
    pub(crate) ruleset: &'a RuleSet,
    pub(crate) workspace_prefix: &'a Option<Arc<str>>,
    pub(crate) string_table: &'a StringTable,
    pub(crate) definitions: &'a [(String, cwtools_info::SourceLocation)],
}

trait UseSiteSink {
    fn push(
        &mut self,
        file_uri: &str,
        key: cwtools_info::SourceLocation,
        value: cwtools_info::SourceLocation,
    );
}

impl UseSiteSink for Vec<cwtools_info::UseSite> {
    fn push(
        &mut self,
        file_uri: &str,
        key: cwtools_info::SourceLocation,
        value: cwtools_info::SourceLocation,
    ) {
        Vec::push(
            self,
            cwtools_info::UseSite {
                file: Arc::from(file_uri),
                key,
                value,
            },
        );
    }
}

impl UseSiteSink for BoundedUseSites {
    fn push(
        &mut self,
        file_uri: &str,
        key: cwtools_info::SourceLocation,
        value: cwtools_info::SourceLocation,
    ) {
        self.total += 1;
        if self.sites.len() < self.limit {
            self.sites.push(cwtools_info::UseSite {
                file: Arc::from(file_uri),
                key,
                value,
            });
        }
    }
}

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
            &TypeRefSearch {
                type_name,
                instance_name,
                file_uri,
                ruleset,
                logical_path: &logical_path,
                table: string_table,
                patterns: &patterns,
                definitions,
                ast,
                references: OnceCell::new(),
            },
            &mut results,
        );
    }

    results
}

/// Walk open documents while retaining only `limit` sites. `total` counts all
/// matches, so callers can report omissions without building the full vector.
pub(crate) fn scan_use_sites_bounded(
    scan: BoundedUseSiteScan<'_>,
    limit: usize,
) -> BoundedUseSites {
    let BoundedUseSiteScan {
        type_name,
        instance_name,
        docs,
        ruleset,
        workspace_prefix,
        string_table,
        definitions,
    } = scan;
    let mut results = BoundedUseSites {
        sites: Vec::with_capacity(limit),
        total: 0,
        limit,
    };
    let patterns = cwtools_info::build_type_patterns(ruleset);

    for (file_uri, ast) in docs {
        let logical_path = logical_path_from_uri(file_uri, workspace_prefix);

        scan_ast_for_type_ref(
            &ast.root_children,
            &TypeRefSearch {
                type_name,
                instance_name,
                file_uri,
                ruleset,
                logical_path: &logical_path,
                table: string_table,
                patterns: &patterns,
                definitions,
                ast,
                references: OnceCell::new(),
            },
            &mut results,
        );
    }

    results
}

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
    patterns: &'a [cwtools_info::TypePatternAlias],
    definitions: &'a [(String, cwtools_info::SourceLocation)],
    ast: &'a ParsedFile,
    /// Key locations of the file's references to the target, classified as the
    /// reference index classifies them. Filled by the first leaf whose value
    /// names the target, so a file that never does costs only the walk.
    references: OnceCell<Vec<cwtools_info::SourceLocation>>,
}

fn scan_ast_for_type_ref<S: UseSiteSink>(
    children: &[cwtools_parser::ast::Child],
    search: &TypeRefSearch,
    out: &mut S,
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
        ast,
        ref references,
    } = search;

    for child in children {
        let Child::Leaf(idx) = child else { continue };
        #[cfg(test)]
        AST_LEAF_VISITS.set(AST_LEAF_VISITS.get() + 1);
        let leaf = &ast.arena.leaves[*idx as usize];
        let key_loc = cwtools_info::SourceLocation {
            line: leaf.pos.start.line,
            col: leaf.pos.start.col,
            end: (leaf.pos.end.line, leaf.pos.end.col),
        };
        // Value first: `with_string` borrows from the table instead of
        // allocating a `String` per leaf, and the reference check below runs
        // only once the cheap name comparison has passed (#472). An
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
            && references
                .get_or_init(|| {
                    cwtools_info::type_ref_key_locations(
                        ast,
                        table,
                        ruleset,
                        logical_path,
                        type_name,
                        instance_name,
                    )
                })
                .iter()
                .any(|site| site.line == key_loc.line && site.col == key_loc.col)
        {
            out.push(
                file_uri,
                key_loc,
                cwtools_info::value_location(&leaf.value_pos, quoted),
            );
        }
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
            out.push(file_uri, key_loc, key_loc);
        }
        if let Value::Clause(ch) = &leaf.value {
            scan_ast_for_type_ref(ch, search, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cwtools_parser::parser::parse_string;

    use super::*;
    use crate::navigation::test_rules::type_ref_ruleset;

    /// `my_type` lives under `events/`, which is where `base` is a reference.
    const IN_PATH: &str = "file:///events/test.txt";

    fn scan_at(uri: &str, source: &str) -> Vec<cwtools_info::UseSite> {
        let table = StringTable::new();
        let parsed = parse_string(source, &table);
        let docs = vec![(uri.to_string(), Arc::new(parsed))];
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

    fn scan(source: &str) -> Vec<cwtools_info::UseSite> {
        scan_at(IN_PATH, source)
    }

    #[test]
    fn test_scan_use_sites() {
        let sites = scan("foo = { base = my_instance }\n");
        assert_eq!(sites.len(), 1, "expected one use site");
        assert_eq!(sites[0].file.as_ref(), IN_PATH);
        // The leaf key `base` starts at column 8, the value at column 15.
        assert_eq!((sites[0].key.line, sites[0].key.col), (1, 8));
        assert_eq!((sites[0].value.line, sites[0].value.col), (1, 15));
    }

    #[test]
    fn scan_use_sites_ignores_a_key_outside_the_rules_that_make_it_a_reference() {
        // The alias rule also names `base`, but alias roots do not apply on
        // their own: an unrelated file has no enclosing rule for the leaf.
        let sites = scan_at("file:///other/test.txt", "foo = { base = my_instance }\n");
        assert!(sites.is_empty(), "not a reference here: {sites:?}");
    }

    #[test]
    fn scan_use_sites_follows_parsed_alias_context() {
        let table = StringTable::new();
        let ruleset = cwtools_rules::rules_converter::ast_to_ruleset(
            &parse_string(
                r#"
types = { type[event] = { path = "game/events" name_field = id } }
event = {
    id = scalar
    metadata = { id = scalar }
    immediate = { alias_name[effect] = alias_match_left[effect] }
}
alias[effect:country_event] = { id = <event> }
"#,
                &table,
            ),
            &table,
        );
        let source = "country_event = {\n\
            id = alpha\n\
            metadata = { id = alpha }\n\
            immediate = { country_event = { id = alpha } }\n\
        }\n";
        let docs = vec![(
            "file:///events/e.txt".to_string(),
            Arc::new(parse_string(source, &table)),
        )];
        let ws_uri: Option<Arc<str>> = Some("file:///".into());
        let sites = scan_use_sites("event", "alpha", &docs, &ruleset, &ws_uri, &table, &[]);
        let lines: Vec<u32> = sites.iter().map(|site| site.key.line).collect();
        assert_eq!(lines, [4], "only the alias use counts: {sites:?}");
    }

    #[test]
    fn bounded_scan_counts_high_fanout_without_materializing_matches() {
        let source: String = (0..64)
            .map(|idx| format!("foo_{idx} = {{ base = my_instance }}\n"))
            .collect();
        let table = StringTable::new();
        let parsed = parse_string(&source, &table);
        let docs = vec![(IN_PATH.to_string(), Arc::new(parsed))];
        let ws_uri: Option<Arc<str>> = Some("file:///".into());
        let ruleset = type_ref_ruleset();
        let bounded = scan_use_sites_bounded(
            BoundedUseSiteScan {
                type_name: "my_type",
                instance_name: "my_instance",
                docs: &docs,
                ruleset: &ruleset,
                workspace_prefix: &ws_uri,
                string_table: &table,
                definitions: &[],
            },
            3,
        );

        assert_eq!(bounded.sites.len(), 3);
        assert_eq!(bounded.total, 64);
        assert_eq!(bounded.sites[0].key.line, 1);
        assert_eq!(bounded.sites[2].key.line, 3);
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
