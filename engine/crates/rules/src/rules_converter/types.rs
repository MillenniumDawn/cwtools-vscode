//! Type extraction: `types = { type[x] = { ... } }` blocks into `TypeDefinition`s,
//! including their localisation/modifier sub-blocks and the `##` directives that
//! precede the node.

use super::*;

pub(crate) fn extract_types_from_children(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    ruleset: &mut RuleSet,
) {
    let precomputed = precompute_comments(children, ast, table);
    for (idx, tchild) in children.iter().enumerate() {
        let comments = &precomputed[idx];
        let Child::Leaf(lidx) = tchild else {
            continue;
        };
        let leaf = &ast.arena.leaves[*lidx as usize];
        let key = table.get_string(leaf.key.normal).unwrap_or_default();
        if key.starts_with("type[")
            && let Some(typename) = extract_bracket_content(&key, "type")
        {
            let typedef = process_type_node(typename, leaf, ast, table, ruleset, comments);
            ruleset.types.push(typedef);
        }
    }
}

pub(crate) fn process_type_node(
    name: String,
    leaf: &cwtools_parser::ast::Leaf,
    ast: &ParsedFile,
    table: &StringTable,
    ruleset: &mut RuleSet,
    comments: &[String],
) -> TypeDefinition {
    let mut def = TypeDefinition {
        name,
        name_field: None,
        path_options: PathOptions {
            paths: Vec::new(),
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
    };

    // Parse type_key_filter from comments before this type[] node
    def.type_key_filter = parse_type_key_filter_from_comments(comments);
    def.graph_related_types = parse_graph_related_types_from_comments(comments);
    apply_option_directives(&mut def, comments);

    if let Value::Clause(children) = &leaf.value {
        // First pass: collect subtypes, localisation node, modifiers node
        let mut localisation_children: Option<Vec<Child>> = None;
        let mut modifiers_children: Option<Vec<Child>> = None;

        let precomputed = precompute_comments(children, ast, table);
        for (cidx, child) in children.iter().enumerate() {
            let child_comments = &precomputed[cidx];
            if let Child::Leaf(lidx) = child {
                let l = &ast.arena.leaves[*lidx as usize];
                let k = table.get_string(l.key.normal).unwrap_or_default();
                if k.starts_with("subtype[") {
                    if let Some(st_name) = extract_bracket_content(&k, "subtype") {
                        let st = process_subtype_node_from_leaf(
                            st_name,
                            l,
                            ast,
                            table,
                            ruleset,
                            child_comments,
                        );
                        def.subtypes.push(st);
                    }
                } else if k == "localisation" || k == "modifiers" {
                    if let Value::Clause(clause_ch) = &l.value {
                        if k == "localisation" {
                            localisation_children = Some(clause_ch.clone());
                        } else {
                            modifiers_children = Some(clause_ch.clone());
                        }
                    }
                } else {
                    match k.as_str() {
                        "path" => {
                            let v = clean_path(&leaf_value_string(l, table));
                            def.path_options.paths.push(v);
                        }
                        "path_strict" => {
                            def.path_options.path_strict = leaf_value_string(l, table) == "yes";
                        }
                        "path_file" => {
                            def.path_options.path_file = Some(leaf_value_string(l, table));
                        }
                        "path_extension" => {
                            def.path_options.path_extension = Some(leaf_value_string(l, table));
                        }
                        "name_field" => {
                            def.name_field = Some(leaf_value_string(l, table));
                        }
                        "type_per_file" => {
                            def.type_per_file = leaf_value_string(l, table) == "yes";
                        }
                        "starts_with" => {
                            def.starts_with = Some(leaf_value_string(l, table));
                        }
                        "type_key_prefix" => {
                            def.key_prefix = Some(leaf_value_string(l, table));
                        }
                        "severity" => {
                            def.warning_only = leaf_value_string(l, table) == "warning";
                        }
                        "unique" => {
                            def.unique = leaf_value_string(l, table) == "yes";
                        }
                        // The `should_be_used` directive maps onto the
                        // `should_be_referenced` field (the field is named for
                        // the cross-file "is this type ever referenced?" check
                        // it feeds, but the directive that enables it is spelled
                        // `should_be_used`). Field is shared across crates, so
                        // it is not renamed here (#204).
                        "should_be_used" => {
                            def.should_be_referenced = leaf_value_string(l, table) == "yes";
                        }
                        "skip_root_key" => {
                            if let Value::Clause(block_children) = &l.value {
                                parse_skip_root_key_block(
                                    block_children,
                                    ast,
                                    table,
                                    &mut def.skip_root_key,
                                );
                            } else {
                                parse_skip_root_key_leaf(l, table, &mut def.skip_root_key);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Multiple leaf skip_root_key directives are already promoted inline
        // (above) to a single MultipleKeys entry.  The block form intentionally
        // produces one entry per element (nested levels), so no further
        // collapsing is needed or correct here.

        // Parse localisation block
        if let Some(loc_children) = localisation_children {
            def.localisation = parse_localisation_block(&loc_children, ast, table);
            // Also look for subtype localisation sub-blocks and attach them
            let subtype_locs = parse_subtype_localisation(&loc_children, ast, table);
            for (st_name, locs) in subtype_locs {
                if let Some(st) = def.subtypes.iter_mut().find(|s| s.name == st_name) {
                    st.localisation.extend(locs);
                }
            }
        }

        // Parse modifiers block
        if let Some(mod_children) = modifiers_children {
            def.modifiers = parse_modifiers_block(&mod_children, ast, table);
            let subtype_mods = parse_subtype_modifiers(&mod_children, ast, table);
            for (st_name, mods) in subtype_mods {
                if let Some(st) = def.subtypes.iter_mut().find(|s| s.name == st_name) {
                    st.modifiers.extend(mods);
                }
            }
        }
    }

    def
}

/// Seed the type options that may equally be written as a `## key = value`
/// directive above the `type[x]` node instead of as a leaf in its body (#264).
/// Seeding happens before the body loop, but the body leaf is authoritative
/// for all six options: a body `unique = no` or `severity = error` overrides a
/// directive that set the option on.
///
/// `path`, `path_file`, `path_extension`, `name_field`, `type_key_prefix` and
/// `skip_root_key` stay body-only: nothing writes them as directives, and
/// `skip_root_key` has block and leaf forms that don't map onto a comment line.
fn apply_option_directives(def: &mut TypeDefinition, comments: &[String]) {
    let yes = |key: &str| find_directive(comments, key) == Some("yes");
    def.unique = yes("unique");
    def.type_per_file = yes("type_per_file");
    def.path_options.path_strict = yes("path_strict");
    // The directive is spelled `should_be_used`; the field it feeds is named
    // `should_be_referenced` (see the body-loop arm comment, #204).
    def.should_be_referenced = yes("should_be_used");
    def.warning_only = find_directive(comments, "severity") == Some("warning");
    def.starts_with = find_directive(comments, "starts_with").map(str::to_string);
}

/// Block form: `skip_root_key = { A B }`.
/// Each element is a separate nested level (F# RulesParser.fs:1031-1035 maps
/// each to its own layer). `any` becomes `AnyKey`; anything else becomes
/// `SpecificKey`. Appends to `out`.
fn parse_skip_root_key_block(
    block_children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    out: &mut Vec<SkipRootKey>,
) {
    for block_child in block_children {
        if let Child::LeafValue(lvidx) = block_child {
            let lv = &ast.arena.leaf_values[*lvidx as usize];
            let v = value_to_string(&lv.value, table);
            if v.is_empty() {
                continue;
            }
            if v == "any" {
                out.push(SkipRootKey::AnyKey);
            } else {
                out.push(SkipRootKey::SpecificKey(v));
            }
        }
    }
}

/// Leaf form: `skip_root_key = A`.
/// `any` becomes `AnyKey`. A first named key becomes `SpecificKey`; subsequent
/// named leaves (multiple `skip_root_key = ...` directives) promote the prior
/// entries into a single `MultipleKeys` alternative, using the first entry's
/// operator (F# parity). Appends to / rewrites `out`.
fn parse_skip_root_key_leaf(
    l: &cwtools_parser::ast::Leaf,
    table: &StringTable,
    out: &mut Vec<SkipRootKey>,
) {
    let op = l.op;
    let v = leaf_value_string(l, table);
    if v == "any" {
        out.push(SkipRootKey::AnyKey);
    } else if out.is_empty() {
        out.push(SkipRootKey::SpecificKey(v));
    } else {
        // Multiple leaves: promote to MultipleKeys, using the first entry's
        // operator (F# parity).
        let should_match = op == cwtools_parser::ast::Operator::Equals;
        let first_match_kind = match &out[0] {
            SkipRootKey::MultipleKeys(_, mk) => *mk,
            _ => MatchKind::from_equals(should_match),
        };
        // Flatten the existing entries (SpecificKey / MultipleKeys) plus the
        // new key into one alternative list. AnyKey carries no key text.
        let mut all_keys: Vec<String> = out
            .drain(..)
            .flat_map(|existing| match existing {
                SkipRootKey::SpecificKey(k) => vec![k],
                SkipRootKey::MultipleKeys(ks, _) => ks,
                SkipRootKey::AnyKey => Vec::new(),
            })
            .collect();
        all_keys.push(v);
        out.push(SkipRootKey::MultipleKeys(all_keys, first_match_kind));
    }
}

/// Split a brace-wrapped list `{ a b c }` into its whitespace-separated items,
/// dropping empties. The braces must already be confirmed by the caller via
/// `rhs.starts_with('{') && rhs.ends_with('}')`; only the inner text is split.
fn split_brace_list(rhs: &str) -> Vec<String> {
    let inner = rhs.trim_matches(|c| c == '{' || c == '}');
    inner
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

pub(crate) fn parse_type_key_filter_from_comments(
    comments: &[String],
) -> Option<(Vec<String>, bool)> {
    // Check for negated form first (`type_key_filter <> value`) — only on exactly-## lines.
    for c in comments.iter().rev() {
        let Some(rest) = c.strip_prefix("##") else {
            continue;
        };
        if rest.starts_with('#') {
            continue;
        }
        let rest = rest.trim_start();
        if !rest.starts_with("type_key_filter") {
            continue;
        }
        let after = rest["type_key_filter".len()..].trim_start();
        let (rhs, negative) = if let Some(r) = after.strip_prefix("<>") {
            (r.trim(), true)
        } else if let Some(r) = after.strip_prefix('=') {
            (r.trim(), false)
        } else {
            continue;
        };
        let values = if rhs.starts_with('{') && rhs.ends_with('}') {
            split_brace_list(rhs)
        } else {
            vec![rhs.to_string()]
        };
        return Some((values, negative));
    }
    None
}

fn parse_graph_related_types_from_comments(comments: &[String]) -> Vec<String> {
    if let Some(rhs) = find_directive(comments, "graph_related_types") {
        if rhs.starts_with('{') && rhs.ends_with('}') {
            return split_brace_list(rhs);
        } else if !rhs.is_empty() {
            return vec![rhs.to_string()];
        }
    }
    Vec::new()
}

/// Walk the leaf children of a localisation/modifier sub-block, invoking `f`
/// with `(child_comments, key, value)` for each `Child::Leaf` whose key is not a
/// `subtype[...]` sub-block (those carry Leaf+Clause children handled elsewhere).
///
/// Shared skeleton for `parse_localisation_block` / `parse_modifiers_block`,
/// which differ only in the directives they read and the struct they build.
fn for_each_block_leaf(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    mut f: impl FnMut(&[String], String, String),
) {
    let precomputed = precompute_comments(children, ast, table);
    for (cidx, child) in children.iter().enumerate() {
        let child_comments = &precomputed[cidx];
        if let Child::Leaf(lidx) = child {
            let l = &ast.arena.leaves[*lidx as usize];
            let key = table.get_string(l.key.normal).unwrap_or_default();
            if key.starts_with("subtype[") {
                continue;
            }
            let value = value_to_string(&l.value, table);
            f(child_comments, key, value);
        }
    }
}

pub(crate) fn parse_localisation_block(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
) -> Vec<TypeLocalisation> {
    let mut out = Vec::new();
    for_each_block_leaf(children, ast, table, |child_comments, key, value| {
        let required = has_directive(child_comments, "required");
        let optional = has_directive(child_comments, "optional");
        let primary = has_directive(child_comments, "primary");
        let replace_scopes = parse_replace_scopes_from_comments(child_comments);

        let loc = if let Some(dollar_idx) = value.find('$') {
            let prefix = value[..dollar_idx].to_string();
            let suffix = value[dollar_idx + 1..].to_string();
            TypeLocalisation {
                name: key,
                prefix,
                suffix,
                required,
                optional,
                explicit_field: None,
                replace_scopes,
                primary,
            }
        } else {
            TypeLocalisation {
                name: key,
                prefix: String::new(),
                suffix: String::new(),
                required,
                optional,
                explicit_field: Some(value),
                replace_scopes,
                primary,
            }
        };
        out.push(loc);
    });
    out
}

pub(crate) fn parse_modifiers_block(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
) -> Vec<TypeModifier> {
    let mut out = Vec::new();
    for_each_block_leaf(children, ast, table, |child_comments, key, value| {
        let explicit = has_directive(child_comments, "explicit");
        // Documentation is the first exactly-### line (not ##, which is directives).
        let documentation = child_comments
            .iter()
            .find(|s| s.starts_with("###"))
            .map(|s| s.trim_start_matches('#').trim().to_string());

        let modifier = if let Some(dollar_idx) = value.find('$') {
            let prefix = value[..dollar_idx].to_string();
            let suffix = value[dollar_idx + 1..].to_string();
            TypeModifier {
                prefix,
                suffix,
                category: key,
                documentation,
                explicit,
            }
        } else {
            TypeModifier {
                prefix: String::new(),
                suffix: String::new(),
                category: key,
                documentation,
                explicit,
            }
        };
        out.push(modifier);
    });
    out
}

#[cfg(test)]
mod option_directive_tests {
    use cwtools_parser::parser::parse_string;
    use cwtools_string_table::string_table::StringTable;

    use crate::{rules_converter::ast_to_ruleset, rules_types::TypeDefinition};

    fn parse_typedef(cwt: &str) -> TypeDefinition {
        let table = StringTable::new();
        let ast = parse_string(cwt, &table);
        ast_to_ruleset(&ast, &table)
            .types
            .into_iter()
            .next()
            .expect("no type parsed")
    }

    #[test]
    fn comment_directives_set_every_shared_option() {
        let def = parse_typedef(
            r#"types = {
                ## unique = yes
                ## type_per_file = yes
                ## path_strict = yes
                ## should_be_used = yes
                ## severity = warning
                ## starts_with = my_
                type[foo] = { path = "game/common/foo" }
            }"#,
        );
        assert!(def.unique);
        assert!(def.type_per_file);
        assert!(def.path_options.path_strict);
        assert!(def.should_be_referenced);
        assert!(def.warning_only);
        assert_eq!(def.starts_with, Some("my_".to_string()));
    }

    #[test]
    fn body_leaves_still_set_every_shared_option() {
        let def = parse_typedef(
            r#"types = { type[foo] = {
                path = "game/common/foo"
                unique = yes
                type_per_file = yes
                path_strict = yes
                should_be_used = yes
                severity = warning
                starts_with = "my_"
            } }"#,
        );
        assert!(def.unique);
        assert!(def.type_per_file);
        assert!(def.path_options.path_strict);
        assert!(def.should_be_referenced);
        assert!(def.warning_only);
        assert_eq!(def.starts_with, Some("my_".to_string()));
    }

    // `= yes` options need exactly `yes`, so an explicit `no` stays off.
    #[test]
    fn explicit_no_directive_leaves_the_option_off() {
        let def = parse_typedef(
            r#"types = {
                ## unique = no
                ## path_strict = no
                type[foo] = { path = "game/common/foo" }
            }"#,
        );
        assert!(!def.unique);
        assert!(!def.path_options.path_strict);
    }

    // The body leaf is authoritative for the boolean/severity options, so an
    // off-value spelling turns a directive-set option off. `starts_with` has
    // no off spelling; its body override is covered below.
    #[test]
    fn body_off_value_overrides_directive_on_value() {
        let def = parse_typedef(
            r#"types = {
                ## unique = yes
                ## type_per_file = yes
                ## path_strict = yes
                ## should_be_used = yes
                ## severity = warning
                type[foo] = { path = "game/common/foo" unique = no type_per_file = no path_strict = no should_be_used = no severity = error }
            }"#,
        );
        assert!(!def.unique);
        assert!(!def.type_per_file);
        assert!(!def.path_options.path_strict);
        assert!(!def.should_be_referenced);
        assert!(!def.warning_only);
    }

    // Quoted directive values unquote like the body form, so `## unique = "yes"`
    // and `## severity = "warning"` read the same as their bare spellings.
    #[test]
    fn quoted_yes_and_warning_directives_set_the_option() {
        let def = parse_typedef(
            r#"types = {
                ## unique = "yes"
                ## severity = "warning"
                type[foo] = { path = "game/common/foo" }
            }"#,
        );
        assert!(def.unique);
        assert!(def.warning_only);
    }

    #[test]
    fn body_leaf_wins_over_the_directive() {
        let def = parse_typedef(
            r#"types = {
                ## starts_with = from_comment_
                type[foo] = { path = "game/common/foo" starts_with = "from_body_" }
            }"#,
        );
        assert_eq!(def.starts_with, Some("from_body_".to_string()));
    }

    // The body form goes through `value_to_string`, which unquotes. A directive
    // is raw comment text, so it has to unquote too or the two forms disagree.
    #[test]
    fn quoted_directive_value_is_unquoted() {
        let def = parse_typedef(
            r#"types = {
                ## starts_with = "my_"
                type[foo] = { path = "game/common/foo" }
            }"#,
        );
        assert_eq!(def.starts_with, Some("my_".to_string()));
    }

    // A directive on a neighbouring type must not leak across.
    #[test]
    fn directives_do_not_leak_to_the_next_type() {
        let table = StringTable::new();
        let ast = parse_string(
            r#"types = {
                ## unique = yes
                type[first] = { path = "game/common/first" }
                type[second] = { path = "game/common/second" }
            }"#,
            &table,
        );
        let rs = ast_to_ruleset(&ast, &table);
        let first = rs.types.iter().find(|t| t.name == "first").unwrap();
        let second = rs.types.iter().find(|t| t.name == "second").unwrap();
        assert!(first.unique);
        assert!(!second.unique);
    }

    // `type_key_filter` between the directive and the node is the shape the HOI4
    // config actually uses for its focus types.
    #[test]
    fn directive_survives_another_directive_below_it() {
        let def = parse_typedef(
            r#"types = {
                ## unique = yes
                ## type_key_filter = focus
                type[focus] = { path = "game/common/national_focus" name_field = "id" }
            }"#,
        );
        assert!(def.unique);
        assert_eq!(
            def.type_key_filter,
            Some((vec!["focus".to_string()], false))
        );
    }
}

#[cfg(test)]
mod skip_root_key_tests {
    use cwtools_parser::parser::parse_string;
    use cwtools_string_table::string_table::StringTable;

    use crate::{
        rules_converter::ast_to_ruleset,
        rules_types::{MatchKind, SkipRootKey},
    };

    fn parse_type(cwt: &str) -> Vec<SkipRootKey> {
        let table = StringTable::new();
        let ast = parse_string(cwt, &table);
        let rs = ast_to_ruleset(&ast, &table);
        rs.types
            .into_iter()
            .next()
            .map(|t| t.skip_root_key)
            .unwrap_or_default()
    }

    // Single leaf: skip_root_key = ideas
    #[test]
    fn single_leaf_produces_specific_key() {
        let srk = parse_type(
            r#"types = { type[idea] = { path = "game/common/ideas" skip_root_key = ideas } }"#,
        );
        assert_eq!(srk, vec![SkipRootKey::SpecificKey("ideas".into())]);
    }

    // Single leaf: skip_root_key = any
    #[test]
    fn single_any_leaf_produces_any_key() {
        let srk = parse_type(
            r#"types = { type[idea] = { path = "game/common/ideas" skip_root_key = any } }"#,
        );
        assert_eq!(srk, vec![SkipRootKey::AnyKey]);
    }

    // Block form: skip_root_key = { ideas any }
    // Must produce TWO nested levels, not one MultipleKeys.
    #[test]
    fn block_form_produces_nested_levels() {
        let srk = parse_type(
            r#"types = { type[idea] = { path = "game/common/ideas" skip_root_key = { ideas any } } }"#,
        );
        assert_eq!(
            srk,
            vec![
                SkipRootKey::SpecificKey("ideas".into()),
                SkipRootKey::AnyKey,
            ],
            "block form must produce one entry per element (nested levels)"
        );
    }

    // Block form with two named keys: skip_root_key = { A B }
    #[test]
    fn block_form_two_named_keys_are_two_levels() {
        let srk = parse_type(
            r#"types = { type[foo] = { path = "game/x" skip_root_key = { wrapper inner } } }"#,
        );
        assert_eq!(
            srk,
            vec![
                SkipRootKey::SpecificKey("wrapper".into()),
                SkipRootKey::SpecificKey("inner".into()),
            ]
        );
    }

    // Multiple leaves: skip_root_key = A  +  skip_root_key = B  (alternatives, F# parity)
    // Must keep MultipleKeys (alternative form, single level with two candidates).
    #[test]
    fn multiple_leaves_produce_multiple_keys() {
        let srk = parse_type(
            r#"types = { type[foo] = { path = "game/x" skip_root_key = a skip_root_key = b } }"#,
        );
        assert_eq!(srk.len(), 1, "multiple leaves must collapse to ONE entry");
        match &srk[0] {
            SkipRootKey::MultipleKeys(keys, MatchKind::Equals) => {
                assert!(keys.contains(&"a".to_string()));
                assert!(keys.contains(&"b".to_string()));
            }
            other => panic!("expected MultipleKeys, got {other:?}"),
        }
    }
}
