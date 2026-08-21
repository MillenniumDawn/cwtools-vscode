use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use cwtools_parser::ast::{Arena, Child, Value};
use cwtools_rules::rules_types::{NewField, RootRule, RuleSet, RuleType, TypeType};
use cwtools_string_table::string_table::StringTable;

use crate::{SourceLocation, check_path_dir};

/// One recorded reference to a type instance: the file it appears in, the
/// referenced instance name, and the leaf's KEY location (the parser records
/// only the leaf start reliably; the value column is resolved from text on
/// demand by the LSP references/rename handlers).
#[derive(Debug, Clone)]
struct ReferenceSite {
    file: Arc<str>,
    name: String,
    location: SourceLocation,
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
    pub(crate) fn merge(&mut self, file_uri: &str, refs: Vec<(Arc<str>, String, SourceLocation)>) {
        if refs.is_empty() {
            return;
        }
        let uri: Arc<str> = Arc::from(file_uri);
        for (ty, name, location) in refs {
            self.file_types
                .entry(Arc::clone(&uri))
                .or_default()
                .insert(Arc::clone(&ty));
            self.map.entry(ty).or_default().push(ReferenceSite {
                file: Arc::clone(&uri),
                name,
                location,
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

    /// `(file_uri, key_location)` for every recorded reference to instance
    /// `name` of `type_name`. Exact-match on the name (Paradox refs are written
    /// verbatim); the KEY location is returned — the caller resolves the value
    /// column from text.
    pub fn references(&self, type_name: &str, name: &str) -> Vec<(Arc<str>, SourceLocation)> {
        self.map
            .get(type_name)
            .map(|sites| {
                sites
                    .iter()
                    .filter(|s| s.name == name)
                    .map(|s| (Arc::clone(&s.file), s.location))
                    .collect()
            })
            .unwrap_or_default()
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
/// Records the KEY location; the value column is resolved from text on demand.
pub(crate) fn collect_type_ref_uses(
    children: &[Child],
    arena: &Arena,
    table: &StringTable,
    map: &HashMap<String, Vec<TypeRefRule>>,
    ruleset: &RuleSet,
    logical_path: &str,
    out: &mut Vec<(Arc<str>, String, SourceLocation)>,
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
                out.push((
                    ref_type,
                    name.to_string(),
                    SourceLocation {
                        line: leaf.pos.start.line,
                        col: leaf.pos.start.col,
                        end: (leaf.pos.end.line, leaf.pos.end.col),
                    },
                ));
            }
        }
        if let Value::Clause(ch) = &leaf.value {
            collect_type_ref_uses(ch, arena, table, map, ruleset, logical_path, out);
        }
    }
}
