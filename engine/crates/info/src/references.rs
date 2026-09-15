use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use cwtools_index::TypeIndex;
use cwtools_parser::ast::{Arena, Child, Value};
use cwtools_rules::rules_types::{NewField, PatternKind, RootRule, RuleSet, RuleType, TypeType};
use cwtools_string_table::string_table::StringTable;

use crate::{SourceLocation, check_path_dir};

/// One recorded reference to a type instance: the file it appears in, the
/// referenced instance name, the leaf's KEY location, and where the referenced
/// name itself starts. Both positions come from the parser, so a caller that
/// wants the name rather than the enclosing key needs no file text (#472).
#[derive(Debug, Clone)]
struct ReferenceSite {
    file: Arc<str>,
    name: String,
    location: SourceLocation,
    value: SourceLocation,
}

/// One use site handed back to a caller: the file, the `key = value` leaf's key
/// position, and the position of the referenced name itself — inside the quotes
/// for a quoted value, matching what a text scan for the name would find.
#[derive(Debug, Clone)]
pub struct UseSite {
    pub file: Arc<str>,
    pub key: SourceLocation,
    pub value: SourceLocation,
}

/// A reference collected from one file's AST, before it is filed under its
/// referenced type. A named struct rather than a tuple so the collector's
/// output stays readable as it grows.
#[derive(Debug, Clone)]
pub(crate) struct CollectedRef {
    pub(crate) ref_type: Arc<str>,
    pub(crate) name: String,
    pub(crate) key: SourceLocation,
    pub(crate) value: SourceLocation,
}

/// Workspace-wide reverse index of type-instance USE sites (as opposed to the
/// definition sites in [`crate::TypeIndex`]). Lets `references`/`rename` find
/// where an instance is used across files that aren't open in the editor. Keyed
/// by the referenced type name; the `Arc<str>` type/file keys are shared, so
/// only the referenced identifier is stored per site.
#[derive(Debug, Default)]
pub struct ReferenceIndex {
    map: HashMap<Arc<str>, Vec<ReferenceSite>>,
    /// file_uri → the set of `map` bucket keys (referenced type names) that file
    /// has use sites under. Lets [`remove_file`](Self::remove_file) touch only
    /// the file's own buckets instead of scanning the whole workspace, mirroring
    /// `TypeIndex::file_buckets`.
    file_types: HashMap<Arc<str>, HashSet<Arc<str>>>,
}

impl ReferenceIndex {
    pub(crate) fn merge(&mut self, file_uri: &str, refs: Vec<CollectedRef>) {
        if refs.is_empty() {
            return;
        }
        let uri: Arc<str> = Arc::from(file_uri);
        for collected in refs {
            self.file_types
                .entry(Arc::clone(&uri))
                .or_default()
                .insert(Arc::clone(&collected.ref_type));
            self.map
                .entry(collected.ref_type)
                .or_default()
                .push(ReferenceSite {
                    file: Arc::clone(&uri),
                    name: collected.name,
                    location: collected.key,
                    value: collected.value,
                });
        }
    }

    /// Remove every site contributed by `file_uri` (called on reindex/close).
    ///
    /// Visits only the referenced-type buckets `file_types` records for this
    /// file, proportional to its own reference count rather than the whole
    /// index — same reverse-map removal as `TypeIndex::remove_file`.
    pub(crate) fn remove_file(&mut self, file_uri: &str) {
        let Some(types) = self.file_types.remove(file_uri) else {
            return;
        };
        for ty in &types {
            let Some(sites) = self.map.get_mut(ty) else {
                continue;
            };
            sites.retain(|s| s.file.as_ref() != file_uri);
            if sites.is_empty() {
                self.map.remove(ty);
            }
        }
    }

    /// Every recorded reference to instance `name` of `type_name`. Exact-match
    /// on the name (Paradox refs are written verbatim). Both the key and the
    /// name's own position are returned, so a caller needs no file text to
    /// point at the name.
    pub fn references(&self, type_name: &str, name: &str) -> Vec<UseSite> {
        self.references_where(type_name, |n| n == name)
    }

    pub fn references_ci(&self, type_name: &str, name: &str) -> Vec<UseSite> {
        self.references_where(type_name, |n| n.eq_ignore_ascii_case(name))
    }

    fn references_where(&self, type_name: &str, matches: impl Fn(&str) -> bool) -> Vec<UseSite> {
        self.map
            .get(type_name)
            .map(|sites| {
                sites
                    .iter()
                    .filter(|s| matches(&s.name))
                    .map(|s| UseSite {
                        file: Arc::clone(&s.file),
                        key: s.location,
                        value: s.value,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Where a reference's name starts, from the parser's value range.
///
/// `parse_value` takes the range start *before* consuming the opening quote, so
/// for a quoted value the range begins on the `"` and the name one column
/// further in. Offsetting here keeps the recorded position on the name itself,
/// which is what a text scan for the name used to find. The end is left equal
/// to the start: callers size the range from the name they are looking for, the
/// way `source_range_*` already does.
pub fn value_location(
    value_pos: &cwtools_parser::ast::SourceRange,
    quoted: bool,
) -> SourceLocation {
    let line = value_pos.start.line;
    let col = value_pos.start.col.saturating_add(u16::from(quoted));
    SourceLocation {
        line,
        col,
        end: (line, col),
    }
}

/// A `leaf_key = <type>` mapping extracted from the ruleset's root rules, used
/// to classify type-instance references while indexing. Mirrors the depth-1
/// `SpecificField(key) = TypeField(Simple(type))` rules the LSP's
/// `is_type_ref_leaf` checks. `root_type` is the owning `TypeRule`'s own name
/// (for the path filter); `None` for alias roots, which apply regardless of
/// path.
#[derive(Debug, Clone)]
pub(crate) struct TypeRefRule {
    ref_type: Arc<str>,
    root_type: Option<Arc<str>>,
}

/// Precompute the leaf-key → referenced-type map from a ruleset. Built once per
/// ruleset load and reused across every file's index pass. Keyed by lowercased
/// leaf key (rule keys match case-insensitively).
pub(crate) fn build_type_ref_keys(ruleset: &RuleSet) -> HashMap<String, Vec<TypeRefRule>> {
    let mut map: HashMap<String, Vec<TypeRefRule>> = HashMap::new();
    if !ruleset.type_reference_rules().is_empty() {
        for (key, entries) in ruleset.type_reference_rules() {
            map.insert(
                key.clone(),
                entries
                    .iter()
                    .map(|entry| TypeRefRule {
                        ref_type: Arc::from(entry.ref_type.as_str()),
                        root_type: entry.root_type.as_deref().map(Arc::from),
                    })
                    .collect(),
            );
        }
        return map;
    }

    // Tests and embedders can construct a RuleSet without calling reindex().
    // Preserve the old scan as a compatibility fallback for that shape.
    for root_rule in &ruleset.root_rules {
        let (root_name, is_type_rule, rule) = match root_rule {
            RootRule::TypeRule(n, r) => (n.as_str(), true, r),
            RootRule::AliasRule(n, r) => (n.as_str(), false, r),
            RootRule::SingleAliasRule(n, r) => (n.as_str(), false, r),
        };
        let (rule_type, _) = rule;
        let rules = match rule_type {
            RuleType::NodeRule { rules, .. } => rules.as_ref(),
            _ => continue,
        };
        let root_type: Option<Arc<str>> = if is_type_rule {
            Some(Arc::from(root_name))
        } else {
            None
        };
        for (inner, _) in rules {
            if let RuleType::LeafRule {
                left: NewField::SpecificField(k),
                right: NewField::TypeField(TypeType::Simple(t)),
            } = inner
            {
                map.entry(k.to_ascii_lowercase())
                    .or_default()
                    .push(TypeRefRule {
                        ref_type: Arc::from(t.as_str()),
                        root_type: root_type.clone(),
                    });
            }
        }
    }
    map
}

/// The type a leaf key references, honouring the owning `TypeRule`'s path
/// filter, or `None` when the key isn't a type-ref key here. Mirrors
/// `is_type_ref_leaf`.
pub(crate) fn classify_type_ref_key(
    map: &HashMap<String, Vec<TypeRefRule>>,
    ruleset: &RuleSet,
    key: &str,
    logical_path: &str,
) -> Option<Arc<str>> {
    let entries = map.get(key).or_else(|| {
        key.bytes()
            .any(|b| b.is_ascii_uppercase())
            .then(|| map.get(&key.to_ascii_lowercase()))
            .flatten()
    })?;
    for e in entries {
        let ok = match &e.root_type {
            None => true,
            Some(rt) => ruleset
                .type_by_name()
                .get(rt.as_ref())
                .map(|&idx| check_path_dir(&ruleset.types[idx].path_options, logical_path))
                .unwrap_or(false),
        };
        if ok {
            return Some(Arc::clone(&e.ref_type));
        }
    }
    None
}

/// Walk a file's AST recording every type-instance reference (a `key = value`
/// leaf whose key classifies as a `<type>` ref and whose value is a string).
/// Records the KEY location and the position of the referenced name itself, so
/// callers never have to re-read the file to recover the value column (#472).
pub(crate) fn collect_type_ref_uses(
    children: &[Child],
    arena: &Arena,
    table: &StringTable,
    map: &HashMap<String, Vec<TypeRefRule>>,
    ruleset: &RuleSet,
    logical_path: &str,
    out: &mut Vec<CollectedRef>,
) {
    for child in children {
        let Child::Leaf(idx) = child else { continue };
        let leaf = &arena.leaves[*idx as usize];
        let key = table.get_string(leaf.key.normal).unwrap_or_default();
        if !key.is_empty()
            && let Some(ref_type) = classify_type_ref_key(map, ruleset, &key, logical_path)
            && let Value::String(t) | Value::QString(t) = &leaf.value
            && let Some(raw) = table.get_string(t.normal)
        {
            let name = raw
                .strip_prefix('"')
                .and_then(|x| x.strip_suffix('"'))
                .unwrap_or(&raw);
            if !name.is_empty() {
                out.push(CollectedRef {
                    ref_type,
                    key: SourceLocation {
                        line: leaf.pos.start.line,
                        col: leaf.pos.start.col,
                        end: (leaf.pos.end.line, leaf.pos.end.col),
                    },
                    value: value_location(&leaf.value_pos, name.len() != raw.len()),
                    name: name.to_string(),
                });
            }
        }
        if let Value::Clause(ch) = &leaf.value {
            collect_type_ref_uses(ch, arena, table, map, ruleset, logical_path, out);
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AliasKeySite {
    key: String,
    location: SourceLocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypePatternAlias {
    pub prefix: String,
    pub suffix: String,
    pub type_name: String,
}

#[derive(Debug, Default)]
pub(crate) struct AliasKeyCache {
    sites: HashMap<Arc<str>, Vec<AliasKeySite>>,
}

impl AliasKeyCache {
    pub(crate) fn remove_file(&mut self, file_uri: &str) {
        self.sites.remove(file_uri);
    }

    pub(crate) fn merge_file(&mut self, file_uri: &str, sites: Vec<AliasKeySite>) {
        if sites.is_empty() {
            self.sites.remove(file_uri);
        } else {
            self.sites.insert(Arc::from(file_uri), sites);
        }
    }

    pub(crate) fn file_uris(&self) -> impl Iterator<Item = &Arc<str>> {
        self.sites.keys()
    }

    pub(crate) fn sites(&self, file_uri: &str) -> &[AliasKeySite] {
        self.sites.get(file_uri).map(Vec::as_slice).unwrap_or(&[])
    }
}

pub fn strip_affix<'a>(key: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    let rest = key.strip_prefix(prefix)?;
    let middle = rest.strip_suffix(suffix)?;
    if middle.is_empty() {
        None
    } else {
        Some(middle)
    }
}

pub fn build_type_patterns(ruleset: &RuleSet) -> Vec<TypePatternAlias> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for cat in ruleset.alias_categories().values() {
        for pat in &cat.parsed_patterns {
            if pat.kind != PatternKind::Type {
                continue;
            }
            let type_name = pat
                .placeholder_name
                .split('.')
                .next()
                .unwrap_or(pat.placeholder_name.as_str());
            if !ruleset.type_by_name().contains_key(type_name) {
                continue;
            }
            let pattern = TypePatternAlias {
                prefix: pat.prefix.clone(),
                suffix: pat.suffix.clone(),
                type_name: type_name.to_string(),
            };
            if seen.insert(pattern.clone()) {
                out.push(pattern);
            }
        }
    }
    out
}

pub fn key_matches_type_pattern(
    patterns: &[TypePatternAlias],
    type_name: &str,
    instance_name: &str,
    key: &str,
) -> bool {
    patterns.iter().any(|pat| {
        if pat.type_name != type_name {
            return false;
        }
        strip_affix(key, &pat.prefix, &pat.suffix)
            .is_some_and(|middle| middle.eq_ignore_ascii_case(instance_name))
    })
}

fn add_schema_field(field: &NewField, keys: &mut HashSet<String>) {
    if let NewField::SpecificField(key) = field {
        keys.insert(key.to_ascii_lowercase());
    }
}

fn collect_schema_fields(rule: &RuleType, keys: &mut HashSet<String>) {
    match rule {
        RuleType::LeafRule { left, right } => {
            add_schema_field(left, keys);
            add_schema_field(right, keys);
        }
        RuleType::NodeRule { left, rules } => {
            add_schema_field(left, keys);
            for (inner, _) in rules.iter() {
                collect_schema_fields(inner, keys);
            }
        }
        RuleType::LeafValueRule { right } => add_schema_field(right, keys),
        RuleType::ValueClauseRule { rules } | RuleType::SubtypeRule { rules, .. } => {
            for (inner, _) in rules.iter() {
                collect_schema_fields(inner, keys);
            }
        }
    }
}

pub(crate) fn build_schema_keys(ruleset: &RuleSet) -> HashSet<String> {
    let mut keys = HashSet::new();
    for cat in ruleset.alias_exact().values() {
        for key in cat.keys() {
            keys.insert(key.to_ascii_lowercase());
        }
    }
    for root in &ruleset.root_rules {
        let (_, (rule, _)) = match root {
            RootRule::TypeRule(name, rule) => (name.as_str(), rule),
            RootRule::AliasRule(name, rule) | RootRule::SingleAliasRule(name, rule) => {
                (name.as_str(), rule)
            }
        };
        collect_schema_fields(rule, &mut keys);
    }
    for (_, (rule, _)) in &ruleset.aliases {
        collect_schema_fields(rule, &mut keys);
    }
    for (_, (rule, _)) in &ruleset.single_aliases {
        collect_schema_fields(rule, &mut keys);
    }
    keys
}

fn is_schema_key(schema: &HashSet<String>, key: &str) -> bool {
    if key.bytes().any(|b| b.is_ascii_uppercase()) {
        schema.contains(&key.to_ascii_lowercase())
    } else {
        schema.contains(key)
    }
}

pub(crate) fn collect_alias_key_candidates(
    children: &[Child],
    arena: &Arena,
    table: &StringTable,
    schema: &HashSet<String>,
    out: &mut Vec<AliasKeySite>,
) {
    for child in children {
        let Child::Leaf(idx) = child else { continue };
        let leaf = &arena.leaves[*idx as usize];
        if let Some(key) = table.get_string(leaf.key.normal)
            && !key.is_empty()
            && !is_schema_key(schema, &key)
        {
            out.push(AliasKeySite {
                key,
                location: SourceLocation {
                    line: leaf.pos.start.line,
                    col: leaf.pos.start.col,
                    end: (leaf.pos.end.line, leaf.pos.end.col),
                },
            });
        }
        if let Value::Clause(ch) = &leaf.value {
            collect_alias_key_candidates(ch, arena, table, schema, out);
        }
    }
}

pub(crate) fn classify_alias_key_sites(
    sites: &[AliasKeySite],
    file_uri: &str,
    patterns: &[TypePatternAlias],
    type_index: &TypeIndex,
) -> Vec<CollectedRef> {
    let mut out = Vec::new();
    for site in sites {
        for pat in patterns {
            let Some(middle) = strip_affix(&site.key, &pat.prefix, &pat.suffix) else {
                continue;
            };
            if !type_index.contains(&pat.type_name, middle) {
                continue;
            }
            if type_index.is_instance_at(
                &pat.type_name,
                file_uri,
                middle,
                site.location.line,
                site.location.col,
            ) {
                continue;
            }
            out.push(CollectedRef {
                ref_type: Arc::from(pat.type_name.as_str()),
                name: middle.to_string(),
                key: site.location,
                value: site.location,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use cwtools_parser::parser::parse_string;
    use cwtools_rules::rules_converter::ast_to_ruleset;
    use cwtools_string_table::string_table::StringTable;

    use crate::InfoService;

    const RULES: &str = r#"
types = {
    type[scripted_effect] = { path = "game/common/scripted_effects" }
    type[decision] = { path = "game/common/decisions" }
}
decision = {
    complete_effect = { alias_name[effect] = alias_match_left[effect] }
}
scripted_effect = { alias_name[effect] = alias_match_left[effect] }
alias[effect:<scripted_effect>] = yes
alias[effect:log] = scalar
"#;

    fn indexed(files: &[(&str, &str, &str)]) -> InfoService {
        let table = StringTable::new();
        let rules = ast_to_ruleset(&parse_string(RULES, &table), &table);
        let mut svc = InfoService::new();
        for &(uri, logical_path, source) in files {
            svc.index_file_with_path(
                uri,
                &parse_string(source, &table),
                &table,
                &rules,
                logical_path,
            );
        }
        svc.rebuild_alias_key_index(&rules);
        svc
    }

    #[test]
    fn scripted_effect_call_is_a_use_and_definition_is_not() {
        let svc = indexed(&[
            (
                "e.txt",
                "common/scripted_effects/e.txt",
                "my_se = { log = hi }\n",
            ),
            (
                "d.txt",
                "common/decisions/d.txt",
                "my_dec = {\n    complete_effect = {\n        my_se = yes\n    }\n}\n",
            ),
        ]);
        let sites = svc.alias_key_index.references("scripted_effect", "my_se");
        assert_eq!(sites.len(), 1, "{sites:?}");
        assert_eq!(sites[0].file.as_ref(), "d.txt");
        assert_eq!((sites[0].key.line, sites[0].key.col), (3, 8));
        assert_eq!(
            (sites[0].value.line, sites[0].value.col),
            (sites[0].key.line, sites[0].key.col)
        );
    }

    #[test]
    fn nested_scripted_effect_call_counts() {
        let svc = indexed(&[(
            "e.txt",
            "common/scripted_effects/e.txt",
            "my_se = { log = hi }\nmy_caller = { my_se = yes }\n",
        )]);
        let sites = svc.alias_key_index.references("scripted_effect", "my_se");
        assert_eq!(sites.len(), 1, "{sites:?}");
        assert_eq!(sites[0].file.as_ref(), "e.txt");
        assert_eq!((sites[0].key.line, sites[0].key.col), (2, 14));
    }

    #[test]
    fn rebuild_picks_up_a_caller_indexed_before_the_definition() {
        let table = StringTable::new();
        let rules = ast_to_ruleset(&parse_string(RULES, &table), &table);
        let mut svc = InfoService::new();
        svc.index_file_with_path(
            "d.txt",
            &parse_string(
                "my_dec = {\n    complete_effect = {\n        my_se = yes\n    }\n}\n",
                &table,
            ),
            &table,
            &rules,
            "common/decisions/d.txt",
        );
        assert!(
            svc.alias_key_index
                .references("scripted_effect", "my_se")
                .is_empty(),
            "caller indexed first cannot classify until the definition exists"
        );
        svc.index_file_with_path(
            "e.txt",
            &parse_string("my_se = { log = hi }\n", &table),
            &table,
            &rules,
            "common/scripted_effects/e.txt",
        );
        svc.rebuild_alias_key_index(&rules);
        assert_eq!(
            svc.alias_key_index
                .references("scripted_effect", "my_se")
                .len(),
            1
        );
    }

    #[test]
    fn schema_effect_keys_are_not_alias_key_uses() {
        let svc = indexed(&[(
            "e.txt",
            "common/scripted_effects/e.txt",
            "my_se = { log = hi }\n",
        )]);
        assert!(
            svc.alias_key_index
                .references("scripted_effect", "log")
                .is_empty()
        );
        assert!(
            svc.alias_key_index
                .references("scripted_effect", "my_se")
                .is_empty()
        );
    }
}
