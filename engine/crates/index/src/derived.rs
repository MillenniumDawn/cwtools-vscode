//! Names a game creates from other definitions instead of declaring them.

use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::HashMap;
use std::sync::Arc;

use crate::{SourceLocation, TypeInstance};

/// `name` is a member of `subtype` (`type.subtype`) when `via_name` is an
/// instance of `via_type`. HOI4 equipment is `naval_equip` through
/// `archetype = <equipment.naval_equip>`, which collection cannot check before
/// the index exists.
#[derive(Debug, Clone)]
pub struct InheritedSubtype {
    pub subtype: String,
    pub name: String,
    pub location: SourceLocation,
    pub via_type: String,
    pub via_name: String,
}

/// An entry of HOI4's `duplicate_archetypes` block: `name` copies the archetype
/// `source`, and the game renames every variant of `source` to match, so
/// `medium_tank_chassis_1` also exists as `medium_tank_artillery_chassis_1`.
#[derive(Debug, Clone)]
pub struct DuplicateArchetype {
    pub type_name: String,
    pub name: String,
    pub source: String,
    pub location: SourceLocation,
}

/// What one file contributes to derived names.
#[derive(Debug, Clone, Default)]
pub struct DerivedNames {
    pub inherited: Vec<InheritedSubtype>,
    pub duplicates: Vec<DuplicateArchetype>,
}

impl DerivedNames {
    pub fn is_empty(&self) -> bool {
        self.inherited.is_empty() && self.duplicates.is_empty()
    }
}

#[derive(Debug, Clone)]
struct Inherited {
    uri: Arc<str>,
    name: String,
    location: SourceLocation,
    via_type: String,
    via_lower: String,
}

#[derive(Debug, Clone)]
struct Duplicate {
    uri: Arc<str>,
    name_lower: String,
    source_lower: String,
    location: SourceLocation,
}

/// Every file's [`DerivedNames`], keyed for lookup on a [`crate::TypeIndex`]
/// miss. Resolving at lookup keeps a derived name in step with the definitions
/// it comes from, whichever file changes.
#[derive(Debug, Clone, Default)]
pub(crate) struct DerivedIndex {
    /// Subtype key, then lowercased member name.
    inherited: FxHashMap<String, FxHashMap<String, Vec<Inherited>>>,
    /// Base type name.
    duplicates: FxHashMap<String, Vec<Duplicate>>,
}

impl DerivedIndex {
    pub(crate) fn is_empty(&self) -> bool {
        self.inherited.is_empty() && self.duplicates.is_empty()
    }

    pub(crate) fn merge(&mut self, uri: &str, names: DerivedNames) {
        if names.is_empty() {
            return;
        }
        let uri: Arc<str> = Arc::from(uri);
        for record in names.inherited {
            self.inherited
                .entry(record.subtype)
                .or_default()
                .entry(record.name.to_ascii_lowercase())
                .or_default()
                .push(Inherited {
                    uri: Arc::clone(&uri),
                    name: record.name,
                    location: record.location,
                    via_type: record.via_type,
                    via_lower: record.via_name.to_ascii_lowercase(),
                });
        }
        for record in names.duplicates {
            self.duplicates
                .entry(record.type_name)
                .or_default()
                .push(Duplicate {
                    uri: Arc::clone(&uri),
                    name_lower: record.name.to_ascii_lowercase(),
                    source_lower: record.source.to_ascii_lowercase(),
                    location: record.location,
                });
        }
    }

    pub(crate) fn remove_files(&mut self, removed: impl Fn(&str) -> bool) {
        if self.is_empty() {
            return;
        }
        for by_name in self.inherited.values_mut() {
            for records in by_name.values_mut() {
                records.retain(|r| !removed(&r.uri));
            }
            by_name.retain(|_, records| !records.is_empty());
        }
        self.inherited.retain(|_, by_name| !by_name.is_empty());
        for records in self.duplicates.values_mut() {
            records.retain(|r| !removed(&r.uri));
        }
        self.duplicates.retain(|_, records| !records.is_empty());
    }

    /// Whether lowercased `name` belongs to `type_name` through a derivation,
    /// given `known` to test the name each derivation depends on.
    pub(crate) fn contains(
        &self,
        type_name: &str,
        name: &str,
        known: impl Fn(&str, &str) -> bool,
    ) -> bool {
        let inherited = self
            .inherited
            .get(type_name)
            .and_then(|by_name| by_name.get(name))
            .is_some_and(|records| records.iter().any(|r| known(&r.via_type, &r.via_lower)));
        inherited
            || self
                .duplicate_sources(type_name, name)
                .any(|source| known(type_name, &source))
    }

    /// The names `name` was renamed from, one per duplicate it could come from.
    fn duplicate_sources<'a>(
        &'a self,
        type_name: &str,
        name: &'a str,
    ) -> impl Iterator<Item = String> + 'a {
        let base = type_name
            .split_once('.')
            .map_or(type_name, |(base, _)| base);
        self.duplicates
            .get(base)
            .into_iter()
            .flatten()
            .filter_map(move |d| {
                let rest = name.strip_prefix(d.name_lower.as_str())?;
                (!rest.is_empty()).then(|| format!("{}{rest}", d.source_lower))
            })
    }

    /// Every derived membership as instances to merge per defining file, for an
    /// index stored as plain instances (the base-game cache), which drops this
    /// side table. `declared` lists a type's own lowercased names, and
    /// `is_declared` and `contains` answer for a lowercased name without and
    /// with derivations.
    pub(crate) fn materialize<'a>(
        &self,
        type_names: &[&'a str],
        declared: impl Fn(&str) -> Vec<&'a str>,
        is_declared: impl Fn(&str, &str) -> bool,
        contains: impl Fn(&str, &str) -> bool,
    ) -> FxHashMap<Arc<str>, HashMap<String, Vec<TypeInstance>>> {
        let mut out: FxHashMap<Arc<str>, HashMap<String, Vec<TypeInstance>>> = FxHashMap::default();
        let mut seen: FxHashSet<(String, String)> = FxHashSet::default();
        let mut push = |uri: &Arc<str>, type_name: &str, name: &str, location| {
            let lower = name.to_ascii_lowercase();
            if is_declared(type_name, &lower)
                || !contains(type_name, &lower)
                || !seen.insert((type_name.to_string(), lower))
            {
                return;
            }
            out.entry(Arc::clone(uri))
                .or_default()
                .entry(type_name.to_string())
                .or_default()
                .push(TypeInstance {
                    name: name.to_string(),
                    location,
                    primary_loc_key: None,
                    required_loc_keys: Vec::new(),
                });
        };
        for (subtype, by_name) in &self.inherited {
            for records in by_name.values() {
                for r in records {
                    push(&r.uri, subtype, &r.name, r.location);
                }
            }
        }
        for (base, duplicates) in &self.duplicates {
            let keys: Vec<&str> = type_names
                .iter()
                .copied()
                .filter(|t| t.split_once('.').map_or(*t, |(b, _)| b) == base)
                .collect();
            let sources = declared(base);
            for d in duplicates {
                for source in &sources {
                    let Some(rest) = source.strip_prefix(d.source_lower.as_str()) else {
                        continue;
                    };
                    if rest.is_empty() {
                        continue;
                    }
                    let name = format!("{}{rest}", d.name_lower);
                    for key in &keys {
                        push(&d.uri, key, &name, d.location);
                    }
                }
            }
        }
        out
    }
}
