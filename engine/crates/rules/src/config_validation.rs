//! Structural validation of the loaded `.cwt` rule config.
//!
//! Walks each parsed `.cwt` AST while it is still alive, collecting the
//! references it makes to types, enums and single-aliases as lightweight
//! [`RefCandidate`]s (position + classification, no AST retained). After every
//! file is merged the candidates are resolved against the fully-merged
//! `RuleSet`, flagging any that no definition provides — a broken schema
//! otherwise silently degrades every downstream check (see `referenced_name`
//! for why alias categories are out).
//!
//! Splitting collection from resolution lets the loader drop each AST as soon
//! as it is converted instead of pinning every parsed file for a second walk.
//! Collection reuses the converter's `field_from_string` so the reference
//! classification can't drift from how the rules are actually compiled.
//! Definitions self-resolve (a `type[foo]` definition's own name is in
//! `type_by_name`), so this permissive whole-AST walk never false-flags a
//! definition; it only fires on a *referenced* name that no definition provides.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use cwtools_error_codes::CW601_RULES_UNDEFINED_REFERENCE;
use cwtools_parser::ast::{Child, ParsedFile, Value};
use cwtools_string_table::string_table::StringTable;

use crate::rules_converter::field_parser::field_from_string;
use crate::rules_converter::value_to_string;
use crate::rules_types::{CwtDefKind, CwtDefPosition, NewField, RuleSet, ValueType};
use crate::ruleset_loader::RuleParseError;

/// A single reference made by a `.cwt` rule, classified and positioned but not
/// yet resolved. Collected while the source AST is alive so the AST can be
/// dropped before the merged `RuleSet` exists; resolved later by
/// [`resolve_reference_candidates`].
pub struct RefCandidate {
    file: PathBuf,
    line: u32,
    col: u16,
    kind: RefKind,
    name: String,
}

/// Walk one parsed `.cwt` AST and append every type/enum/single-alias reference
/// it makes to `out`, keyed by source position. Does not touch the `RuleSet`:
/// resolution is deferred to [`resolve_reference_candidates`] so this can run
/// per-file before the cross-file merge, letting the caller drop each AST as it
/// is converted.
pub fn collect_reference_candidates(
    path: &Path,
    ast: &ParsedFile,
    table: &StringTable,
    out: &mut Vec<RefCandidate>,
) {
    for child in &ast.root_children {
        collect_child(child, ast, table, path, out);
    }
}

/// Resolve collected references against the fully-merged `RuleSet`, returning
/// one `RuleParseError` per undefined reference (positioned at the referencing
/// leaf), in candidate order. Run after all files are merged so cross-file
/// definitions resolve.
pub fn resolve_reference_candidates(
    candidates: &[RefCandidate],
    ruleset: &RuleSet,
) -> Vec<RuleParseError> {
    // Defined single_alias names, indexed once for O(1) `is_defined` lookups
    // instead of a linear scan per referenced single_alias.
    let single_alias_names: HashSet<&str> = ruleset
        .single_aliases
        .iter()
        .map(|(k, _)| k.as_str())
        .collect();
    let mut errors = Vec::new();
    for c in candidates {
        if !is_defined(ruleset, &single_alias_names, c.kind, &c.name) {
            errors.push(RuleParseError::new(
                &CW601_RULES_UNDEFINED_REFERENCE,
                c.file.clone(),
                c.line,
                c.col,
                format!("rule references undefined {} `{}`", c.kind.label(), c.name),
            ));
        }
    }
    errors
}

/// Validate parsed `.cwt` ASTs against the fully-merged `RuleSet` in one call
/// (collect then resolve). Used by the single-file `.cwt` LSP lint, where the
/// caller already holds the AST and the ruleset together; the bulk loader
/// instead collects per-file and resolves once so it need not pin every AST.
pub fn validate_ruleset_references(
    files: &[(PathBuf, ParsedFile)],
    ruleset: &RuleSet,
    table: &StringTable,
) -> Vec<RuleParseError> {
    let mut candidates = Vec::new();
    for (path, ast) in files {
        collect_reference_candidates(path, ast, table, &mut candidates);
    }
    resolve_reference_candidates(&candidates, ruleset)
}

/// Collect the source position of every `type[x]` / `enum[x]` /
/// `complex_enum[x]` / `single_alias[x]` definition in one parsed `.cwt`, for
/// goto/hover inside rule files. Type and enum definitions live under the
/// `types` / `enums` root blocks; single_aliases sit at the root.
pub fn collect_definition_positions(
    path: &Path,
    ast: &ParsedFile,
    table: &StringTable,
    out: &mut Vec<CwtDefPosition>,
) {
    let def = |kind, name: &str, leaf: &cwtools_parser::ast::Leaf| CwtDefPosition {
        kind,
        name: name.to_string(),
        file: path.to_path_buf(),
        line: leaf.pos.start.line,
        col: leaf.pos.start.col,
    };
    for child in &ast.root_children {
        let Child::Leaf(idx) = child else { continue };
        let leaf = &ast.arena.leaves[*idx as usize];
        let raw_key = table.get_string(leaf.key.normal).unwrap_or_default();
        let key = raw_key.trim_matches('"');
        if let Some(name) = bracket_name(key, "single_alias") {
            out.push(def(CwtDefKind::SingleAlias, name, leaf));
            continue;
        }
        let member_kinds: &[(&str, CwtDefKind)] = if key.eq_ignore_ascii_case("types") {
            &[("type", CwtDefKind::Type)]
        } else if key.eq_ignore_ascii_case("enums") {
            &[
                ("complex_enum", CwtDefKind::Enum),
                ("enum", CwtDefKind::Enum),
            ]
        } else {
            continue;
        };
        let Value::Clause(inner) = &leaf.value else {
            continue;
        };
        for c in inner {
            let Child::Leaf(idx) = c else { continue };
            let member = &ast.arena.leaves[*idx as usize];
            let member_key = table.get_string(member.key.normal).unwrap_or_default();
            let member_key = member_key.trim_matches('"');
            for (prefix, kind) in member_kinds {
                if let Some(name) = bracket_name(member_key, prefix) {
                    out.push(def(*kind, name, member));
                    break;
                }
            }
        }
    }
}

/// `prefix[NAME]` → `NAME`; `None` for any other shape.
fn bracket_name<'a>(key: &'a str, prefix: &str) -> Option<&'a str> {
    key.strip_prefix(prefix)?
        .strip_prefix('[')?
        .strip_suffix(']')
}

fn collect_child(
    child: &Child,
    ast: &ParsedFile,
    table: &StringTable,
    path: &Path,
    out: &mut Vec<RefCandidate>,
) {
    match child {
        Child::Leaf(idx) => {
            let leaf = &ast.arena.leaves[*idx as usize];
            let pos = &leaf.pos.start;
            // The key may itself be a reference (`<character> = { … }`).
            let key = table.get_string(leaf.key.normal).unwrap_or_default();
            collect_field(&key, pos.line, pos.col, path, out);
            match &leaf.value {
                Value::Clause(children) => {
                    for ch in children {
                        collect_child(ch, ast, table, path, out);
                    }
                }
                other => {
                    collect_field(&value_to_string(other, table), pos.line, pos.col, path, out)
                }
            }
        }
        Child::LeafValue(idx) => {
            let lv = &ast.arena.leaf_values[*idx as usize];
            let pos = &lv.pos.start;
            match &lv.value {
                Value::Clause(children) => {
                    for ch in children {
                        collect_child(ch, ast, table, path, out);
                    }
                }
                other => {
                    collect_field(&value_to_string(other, table), pos.line, pos.col, path, out)
                }
            }
        }
        Child::Comment(_) => {}
    }
}

fn collect_field(s: &str, line: u32, col: u16, path: &Path, out: &mut Vec<RefCandidate>) {
    if let Some((kind, name)) = referenced_name(&field_from_string(s)) {
        out.push(RefCandidate {
            file: path.to_path_buf(),
            line,
            col,
            kind,
            name,
        });
    }
}

#[derive(Clone, Copy)]
enum RefKind {
    Type,
    Enum,
    SingleAlias,
}

impl RefKind {
    fn label(self) -> &'static str {
        match self {
            RefKind::Type => "type",
            RefKind::Enum => "enum",
            RefKind::SingleAlias => "single_alias",
        }
    }
}

/// The referenced name a field carries, if it points at a definition the config
/// must provide.
///
/// Path / scope / value-set fields are intentionally omitted: they resolve
/// leniently (engine-provided or defined by use). Alias categories are omitted
/// too — an `alias[cat:name]` *definition* key parses to the same `AliasField`
/// as an `alias_name[cat]` *reference*, so a whole-AST walk can't tell them apart
/// and would false-flag the definitions. Types, enums and single-aliases use
/// distinct definition syntax (`type[x]`, `enum[x]` under `enums`,
/// `single_alias[x]`) that does NOT parse to a reference field, so their
/// definitions self-resolve and only genuine dangling references fire.
fn referenced_name(field: &NewField) -> Option<(RefKind, String)> {
    match field {
        // `<type>` / `<type.subtype>`: the subtype qualifier constrains the
        // match but the definition is keyed by the base type, so check that.
        NewField::TypeField(t) => Some((RefKind::Type, t.base_name().to_string())),
        NewField::ValueField(ValueType::Enum(n)) => Some((RefKind::Enum, n.clone())),
        NewField::SingleAliasField(n) => Some((RefKind::SingleAlias, n.clone())),
        _ => None,
    }
}

fn is_defined(
    ruleset: &RuleSet,
    single_alias_names: &HashSet<&str>,
    kind: RefKind,
    name: &str,
) -> bool {
    match kind {
        RefKind::Type => ruleset.type_by_name().contains_key(name),
        RefKind::Enum => {
            ruleset.enum_by_name().contains_key(name)
                || ruleset.complex_enums.iter().any(|c| c.name == name)
        }
        RefKind::SingleAlias => single_alias_names.contains(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules_converter::ast_to_ruleset;
    use cwtools_parser::parser::parse_string;

    fn check(src: &str) -> Vec<RuleParseError> {
        let table = StringTable::new();
        let parsed = parse_string(src, &table);
        let ruleset = ast_to_ruleset(&parsed, &table);
        let files = vec![(PathBuf::from("test.cwt"), parsed)];
        validate_ruleset_references(&files, &ruleset, &table)
    }

    #[test]
    fn collects_definition_positions_by_kind() {
        let src = "types = {\n    type[focus] = { path = \"common/national_focus\" }\n}\n\
                   enums = {\n    enum[stat] = { army navy }\n    complex_enum[cats] = { path = \"common/c\" name = { x } }\n}\n\
                   single_alias[block] = { a = bool }\n";
        let table = StringTable::new();
        let parsed = parse_string(src, &table);
        let mut out = Vec::new();
        collect_definition_positions(&PathBuf::from("defs.cwt"), &parsed, &table, &mut out);
        let find = |kind: CwtDefKind, name: &str| {
            out.iter()
                .find(|d| d.kind == kind && d.name == name)
                .unwrap_or_else(|| panic!("missing {:?} {}, got {:?}", kind, name, out))
        };
        assert_eq!(find(CwtDefKind::Type, "focus").line, 2);
        assert_eq!(find(CwtDefKind::Enum, "stat").line, 5);
        assert_eq!(find(CwtDefKind::Enum, "cats").line, 6);
        assert_eq!(find(CwtDefKind::SingleAlias, "block").line, 8);
        assert!(out.iter().all(|d| d.file == Path::new("defs.cwt")));
    }

    #[test]
    fn flags_undefined_type_reference_but_not_defined_one() {
        let src = "types = {\n    type[foo] = { path = \"common/foo\" }\n}\n\
                   some_rule = {\n    a = <foo>\n    b = <undefined_type>\n}\n";
        let errors = check(src);
        assert!(
            errors.iter().any(|e| e.message.contains("undefined_type")),
            "should flag undefined type, got: {:?}",
            errors
        );
        assert!(
            !errors.iter().any(|e| e.message.contains("`foo`")),
            "must NOT flag the defined type `foo`, got: {:?}",
            errors
        );
    }

    #[test]
    fn defined_type_reference_is_clean() {
        let src = "types = {\n    type[foo] = { path = \"common/foo\" }\n}\n\
                   r = { a = <foo> }\n";
        assert!(check(src).is_empty(), "got: {:?}", check(src));
    }

    #[test]
    fn type_subtype_reference_resolves_to_base_type() {
        // `<decision.timed>` constrains to a subtype but is defined by the base
        // type `decision`, so it must not flag.
        let src = "types = {\n    type[decision] = { path = \"common/decisions\" }\n}\n\
                   r = { a = <decision.timed> }\n";
        assert!(check(src).is_empty(), "got: {:?}", check(src));
    }

    #[test]
    fn split_collect_resolve_matches_combined_across_files() {
        // A type defined in one file, referenced in another: cross-file
        // resolution must work, and the loader's split path (collect per file
        // while the AST is alive, resolve once after merge) must produce
        // diagnostics byte-identical and in the same order as the combined
        // entry point.
        use crate::ruleset_loader::merge_ruleset;
        let table = StringTable::new();
        let a_src = "types = {\n    type[foo] = { path = \"common/foo\" }\n}\n";
        let b_src = "r = {\n    a = <foo>\n    b = <bar>\n}\n";
        let a = parse_string(a_src, &table);
        let b = parse_string(b_src, &table);

        let mut merged = ast_to_ruleset(&a, &table);
        merge_ruleset(&mut merged, ast_to_ruleset(&b, &table));
        merged.reindex();

        let files = vec![(PathBuf::from("a.cwt"), a), (PathBuf::from("b.cwt"), b)];

        // Path 1: combined entry point over both files at once.
        let combined = validate_ruleset_references(&files, &merged, &table);

        // Path 2: loader-style — collect per file, then resolve once.
        let mut candidates = Vec::new();
        for (path, ast) in &files {
            collect_reference_candidates(path, ast, &table, &mut candidates);
        }
        let split = resolve_reference_candidates(&candidates, &merged);

        let key = |e: &RuleParseError| (e.file.clone(), e.line, e.col, e.message.clone());
        assert_eq!(
            combined.iter().map(key).collect::<Vec<_>>(),
            split.iter().map(key).collect::<Vec<_>>(),
            "split path must match combined path exactly (order included)",
        );
        // Cross-file `<foo>` resolves; only the truly-undefined `<bar>` fires.
        assert_eq!(split.len(), 1, "only <bar> should fire, got: {:?}", split);
        assert!(split[0].message.contains("`bar`"));
        assert!(!combined.iter().any(|e| e.message.contains("`foo`")));
    }
}
