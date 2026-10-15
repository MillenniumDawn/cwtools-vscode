use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use cwtools_index::TypeIndex;
use cwtools_parser::ast::{Arena, Child, Value};
use cwtools_rules::rules_types::{NewField, PatternKind, RootRule, RuleSet, RuleType, TypeType};
use cwtools_string_table::string_table::StringTable;
use rustc_hash::FxHashMap;

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

/// Every site that names one instance, whatever its ASCII case, in merge order.
/// `key` is the lowercased name the bucket is filed under, kept so `merge` can
/// hand the same allocation to `file_keys`.
#[derive(Debug)]
struct NameSites {
    key: Arc<str>,
    sites: Vec<ReferenceSite>,
}

/// Where a [`NameSites`] is filed: the referenced type and the lowercased name.
#[derive(Debug)]
struct BucketKey {
    ref_type: Arc<str>,
    name: Arc<str>,
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

/// A use site with its lookup key, used to snapshot one open document's
/// references alongside the parsed AST that produced them.
#[derive(Debug, Clone)]
pub struct IndexedUseSite {
    pub type_name: String,
    pub name: String,
    /// Alias-key references are matched without regard to ASCII case; ordinary
    /// type references retain the exact-spelling behavior of `references`.
    pub case_insensitive: bool,
    pub site: UseSite,
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
/// by the referenced type name, then by the referenced name lowercased (ASCII),
/// so a lookup or a removal visits the sites of one name rather than every use
/// of the type. The `Arc<str>` type/file keys are shared, so only the
/// referenced identifier is stored per site.
#[derive(Debug, Default)]
pub struct ReferenceIndex {
    map: FxHashMap<Arc<str>, FxHashMap<Arc<str>, NameSites>>,
    /// file_uri → the `(type, lowercased name)` keys of `map` that file has use
    /// sites under, each once. Lets [`remove_file`](Self::remove_file) visit
    /// only the file's own keys instead of scanning the whole workspace,
    /// mirroring `TypeIndex::file_buckets`.
    file_keys: FxHashMap<Arc<str>, Vec<BucketKey>>,
}

impl ReferenceIndex {
    pub(crate) fn merge(&mut self, file_uri: &str, refs: Vec<CollectedRef>) {
        if refs.is_empty() {
            return;
        }
        let uri: Arc<str> = Arc::from(file_uri);
        let file_keys = self.file_keys.entry(Arc::clone(&uri)).or_default();
        for CollectedRef {
            ref_type,
            name,
            key,
            value,
        } in refs
        {
            let into_site = |name: String| ReferenceSite {
                file: Arc::clone(&uri),
                name,
                location: key,
                value,
            };
            let lower = ascii_lowercase(&name);
            let by_name = self.map.entry(Arc::clone(&ref_type)).or_default();
            match by_name.get_mut(&*lower) {
                Some(bucket) => {
                    // A file's sites for a name are merged back to back, so the
                    // last one tells whether `file_keys` already has this key.
                    if !bucket.sites.last().is_some_and(|s| s.file == uri) {
                        file_keys.push(BucketKey {
                            ref_type,
                            name: Arc::clone(&bucket.key),
                        });
                    }
                    bucket.sites.push(into_site(name));
                }
                None => {
                    let bucket_key: Arc<str> = Arc::from(&*lower);
                    file_keys.push(BucketKey {
                        ref_type,
                        name: Arc::clone(&bucket_key),
                    });
                    // Capacity 4, as a plain push gives; `vec![site]` regrows on the second use.
                    let mut sites = Vec::with_capacity(4);
                    sites.push(into_site(name));
                    by_name.insert(
                        Arc::clone(&bucket_key),
                        NameSites {
                            key: bucket_key,
                            sites,
                        },
                    );
                }
            }
        }
    }

    /// Remove every site contributed by `file_uri` (called on reindex/close).
    ///
    /// Visits only the `(type, name)` buckets `file_keys` records for this
    /// file, so the cost is the file's own sites plus the other uses of the
    /// names it references, never the rest of the type. Same reverse-map
    /// removal as `TypeIndex::remove_file`.
    pub(crate) fn remove_file(&mut self, file_uri: &str) {
        let Some(keys) = self.file_keys.remove(file_uri) else {
            return;
        };
        for BucketKey { ref_type, name } in keys {
            let Some(by_name) = self.map.get_mut(&*ref_type) else {
                continue;
            };
            if let Some(bucket) = by_name.get_mut(&*name) {
                bucket.sites.retain(|s| s.file.as_ref() != file_uri);
                if bucket.sites.is_empty() {
                    by_name.remove(&*name);
                }
            }
            if by_name.is_empty() {
                self.map.remove(&*ref_type);
            }
        }
    }

    /// Remove one case-insensitive `(type, name)` bucket, keeping the
    /// per-file reverse keys in sync. Used when an exported type name leaves
    /// or enters the workspace without rebuilding unrelated references.
    pub(crate) fn remove_name(&mut self, ref_type: &str, name: &str) {
        let lower = ascii_lowercase(name);
        let Some(by_name) = self.map.get_mut(ref_type) else {
            return;
        };
        let Some(bucket) = by_name.remove(lower.as_ref()) else {
            return;
        };
        let mut files = HashSet::new();
        for site in bucket.sites {
            files.insert(site.file);
        }
        for file in files {
            if let Some(keys) = self.file_keys.get_mut(&file) {
                keys.retain(|key| key.ref_type.as_ref() != ref_type || key.name != bucket.key);
                if keys.is_empty() {
                    self.file_keys.remove(&file);
                }
            }
        }
        if by_name.is_empty() {
            self.map.remove(ref_type);
        }
    }

    /// Every recorded reference to instance `name` of `type_name`. Exact-match
    /// on the name (Paradox refs are written verbatim). Both the key and the
    /// name's own position are returned, so a caller needs no file text to
    /// point at the name.
    pub fn references(&self, type_name: &str, name: &str) -> Vec<UseSite> {
        self.references_where(type_name, name, |n| n == name)
    }

    /// Like [`Self::references`], ignoring ASCII case. A name's bucket holds
    /// exactly the spellings that match this way, so no per-site check is left.
    pub fn references_ci(&self, type_name: &str, name: &str) -> Vec<UseSite> {
        self.references_where(type_name, name, |_| true)
    }

    /// Fingerprint the references contributed by one file. This follows the
    /// file's own `(type, lowercase name)` keys, rather than scanning every
    /// workspace reference when deciding whether CodeLens needs a refresh.
    pub(crate) fn file_fingerprint(&self, file_uri: &str) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let Some(keys) = self.file_keys.get(file_uri) else {
            return hasher.finish();
        };
        for BucketKey { ref_type, name } in keys {
            ref_type.hash(&mut hasher);
            name.hash(&mut hasher);
            if let Some(bucket) = self
                .map
                .get(ref_type.as_ref())
                .and_then(|names| names.get(name.as_ref()))
            {
                for site in bucket
                    .sites
                    .iter()
                    .filter(|site| site.file.as_ref() == file_uri)
                {
                    site.name.hash(&mut hasher);
                    hash_location(&mut hasher, site.location);
                    hash_location(&mut hasher, site.value);
                }
            }
        }
        hasher.finish()
    }

    pub(crate) fn file_references(&self, file_uri: &str) -> Vec<IndexedUseSite> {
        let Some(keys) = self.file_keys.get(file_uri) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for BucketKey { ref_type, name } in keys {
            let Some(bucket) = self
                .map
                .get(ref_type.as_ref())
                .and_then(|names| names.get(name.as_ref()))
            else {
                continue;
            };
            out.extend(
                bucket
                    .sites
                    .iter()
                    .filter(|site| site.file.as_ref() == file_uri)
                    .map(|site| IndexedUseSite {
                        type_name: ref_type.to_string(),
                        name: site.name.clone(),
                        case_insensitive: false,
                        site: UseSite {
                            file: Arc::clone(&site.file),
                            key: site.location,
                            value: site.value,
                        },
                    }),
            );
        }
        out
    }

    /// Return at most `limit` matching sites, together with the total number
    /// of matches. The total is counted without allocating the complete result
    /// vector, and `excluded_files` preserves the LSP's open-buffer precedence.
    pub fn references_bounded(
        &self,
        type_name: &str,
        name: &str,
        limit: usize,
        excluded_files: &HashSet<String>,
    ) -> (Vec<UseSite>, usize) {
        self.references_where_bounded(type_name, name, |n| n == name, limit, excluded_files)
    }

    /// Case-insensitive counterpart to [`Self::references_bounded`].
    pub fn references_ci_bounded(
        &self,
        type_name: &str,
        name: &str,
        limit: usize,
        excluded_files: &HashSet<String>,
    ) -> (Vec<UseSite>, usize) {
        self.references_where_bounded(type_name, name, |_| true, limit, excluded_files)
    }

    /// The sites filed under `name`, whatever its ASCII case, in index order.
    fn sites_for(&self, type_name: &str, name: &str) -> &[ReferenceSite] {
        self.map
            .get(type_name)
            .and_then(|by_name| by_name.get(&*ascii_lowercase(name)))
            .map_or(&[], |bucket| bucket.sites.as_slice())
    }

    fn references_where(
        &self,
        type_name: &str,
        name: &str,
        matches: impl Fn(&str) -> bool,
    ) -> Vec<UseSite> {
        self.sites_for(type_name, name)
            .iter()
            .filter(|s| matches(&s.name))
            .map(|s| UseSite {
                file: Arc::clone(&s.file),
                key: s.location,
                value: s.value,
            })
            .collect()
    }

    fn references_where_bounded(
        &self,
        type_name: &str,
        name: &str,
        matches: impl Fn(&str) -> bool,
        limit: usize,
        excluded_files: &HashSet<String>,
    ) -> (Vec<UseSite>, usize) {
        let mut sites_out = Vec::new();
        let mut total = 0;
        for site in self.sites_for(type_name, name) {
            if !matches(&site.name) || excluded_files.contains(site.file.as_ref()) {
                continue;
            }
            total += 1;
            if sites_out.len() < limit {
                sites_out.push(UseSite {
                    file: Arc::clone(&site.file),
                    key: site.location,
                    value: site.value,
                });
            }
        }
        (sites_out, total)
    }
}

/// The key a name is filed under. Borrows when the name has no uppercase byte,
/// the common case, so indexing and lookups skip an allocation.
fn ascii_lowercase(name: &str) -> Cow<'_, str> {
    if name.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(name.to_ascii_lowercase())
    } else {
        Cow::Borrowed(name)
    }
}

fn hash_location<H: std::hash::Hasher>(hasher: &mut H, location: SourceLocation) {
    use std::hash::Hash;

    location.line.hash(hasher);
    location.col.hash(hasher);
    location.end.hash(hasher);
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
    pub(crate) key: String,
    pub(crate) location: SourceLocation,
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
    /// Reverse index from a possible aliased definition to the exact candidate
    /// sites that could become references if that name is exported.
    by_instance: HashMap<(String, String), Vec<AliasKeyCandidate>>,
    file_keys: HashMap<Arc<str>, Vec<(String, String)>>,
}

#[derive(Debug, Clone)]
pub(crate) struct AliasKeyCandidate {
    pub(crate) file: Arc<str>,
    pub(crate) site_index: usize,
    pub(crate) pattern_index: usize,
}

impl AliasKeyCache {
    pub(crate) fn remove_file(&mut self, file_uri: &str) {
        self.sites.remove(file_uri);
        let Some(keys) = self.file_keys.remove(file_uri) else {
            return;
        };
        for key in keys {
            if let Some(candidates) = self.by_instance.get_mut(&key) {
                candidates.retain(|candidate| candidate.file.as_ref() != file_uri);
                if candidates.is_empty() {
                    self.by_instance.remove(&key);
                }
            }
        }
    }

    pub(crate) fn merge_file(
        &mut self,
        file_uri: &str,
        sites: Vec<AliasKeySite>,
        patterns: &[TypePatternAlias],
    ) -> Vec<(String, String)> {
        self.remove_file(file_uri);
        if sites.is_empty() {
            return Vec::new();
        }
        let file: Arc<str> = Arc::from(file_uri);
        let mut grouped: HashMap<&str, Vec<usize>> = HashMap::new();
        if !patterns.is_empty() {
            for (site_index, site) in sites.iter().enumerate() {
                grouped.entry(&site.key).or_default().push(site_index);
            }
        }
        let mut per_instance: HashMap<_, Vec<AliasKeyCandidate>> = HashMap::new();
        for (key, site_indices) in grouped {
            for (pattern_index, pattern) in patterns.iter().enumerate() {
                let Some(middle) = strip_affix(key, &pattern.prefix, &pattern.suffix) else {
                    continue;
                };
                let key = (pattern.type_name.clone(), middle.to_ascii_lowercase());
                per_instance
                    .entry(key)
                    .or_default()
                    .extend(site_indices.iter().map(|&site_index| AliasKeyCandidate {
                        file: Arc::clone(&file),
                        site_index,
                        pattern_index,
                    }));
            }
        }
        let mut keys = Vec::with_capacity(per_instance.len());
        for (key, mut candidates) in per_instance {
            // Restore source/pattern order across case-folded and overlapping groups.
            candidates
                .sort_unstable_by_key(|candidate| (candidate.site_index, candidate.pattern_index));
            self.by_instance
                .entry(key.clone())
                .and_modify(|existing| existing.append(&mut candidates))
                .or_insert(candidates);
            keys.push(key);
        }
        self.file_keys.insert(Arc::clone(&file), keys.clone());
        self.sites.insert(file, sites);
        keys
    }

    pub(crate) fn file_uris(&self) -> impl Iterator<Item = &Arc<str>> {
        self.sites.keys()
    }

    pub(crate) fn sites(&self, file_uri: &str) -> &[AliasKeySite] {
        self.sites.get(file_uri).map(Vec::as_slice).unwrap_or(&[])
    }

    pub(crate) fn candidates_for(&self, type_name: &str, name: &str) -> &[AliasKeyCandidate] {
        let lower = name.to_ascii_lowercase();
        self.by_instance
            .get(&(type_name.to_string(), lower))
            .map_or(&[], Vec::as_slice)
    }

    pub(crate) fn site(&self, file_uri: &str, site_index: usize) -> Option<&AliasKeySite> {
        self.sites.get(file_uri)?.get(site_index)
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
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use cwtools_parser::parser::parse_string;
    use cwtools_rules::rules_converter::ast_to_ruleset;
    use cwtools_string_table::string_table::StringTable;

    use super::{CollectedRef, ReferenceIndex};
    use crate::{InfoService, SourceLocation, TypeInstance};

    fn collected(ref_type: &str, name: &str, line: u32) -> CollectedRef {
        let at = SourceLocation {
            line,
            col: 0,
            end: (line, 0),
        };
        CollectedRef {
            ref_type: Arc::from(ref_type),
            name: name.to_string(),
            key: at,
            value: at,
        }
    }

    fn lines(sites: &[super::UseSite]) -> Vec<(&str, u32)> {
        sites
            .iter()
            .map(|site| (site.file.as_ref(), site.key.line))
            .collect()
    }

    #[test]
    fn exact_lookup_filters_spellings_and_ci_lookup_returns_them_all() {
        let mut index = ReferenceIndex::default();
        index.merge(
            "a.txt",
            vec![collected("t", "Alpha", 1), collected("t", "alpha", 2)],
        );
        index.merge(
            "b.txt",
            vec![collected("t", "ALPHA", 3), collected("t", "beta", 4)],
        );

        assert_eq!(lines(&index.references("t", "Alpha")), [("a.txt", 1)]);
        assert_eq!(lines(&index.references("t", "alpha")), [("a.txt", 2)]);
        assert_eq!(lines(&index.references("t", "beta")), [("b.txt", 4)]);
        assert!(index.references("t", "aLPHA").is_empty());
        let all = [("a.txt", 1), ("a.txt", 2), ("b.txt", 3)];
        assert_eq!(lines(&index.references_ci("t", "aLpHa")), all);
        assert_eq!(lines(&index.references_ci("t", "alpha")), all);
        assert!(index.references_ci("other", "alpha").is_empty());

        let excluded = HashSet::from(["a.txt".to_string()]);
        let (sites, total) = index.references_ci_bounded("t", "ALPHA", 5, &excluded);
        assert_eq!((lines(&sites), total), (vec![("b.txt", 3)], 1));
        let (sites, total) = index.references_bounded("t", "Alpha", 1, &HashSet::new());
        assert_eq!((lines(&sites), total), (vec![("a.txt", 1)], 1));
    }

    #[test]
    fn file_fingerprint_tracks_only_that_files_reference_sites() {
        let mut index = ReferenceIndex::default();
        index.merge("a.txt", vec![collected("t", "Alpha", 1)]);
        let baseline = index.file_fingerprint("a.txt");
        index.merge("b.txt", vec![collected("t", "other", 2)]);
        assert_eq!(index.file_fingerprint("a.txt"), baseline);

        index.remove_file("a.txt");
        index.merge("a.txt", vec![collected("t", "Alpha", 3)]);
        assert_ne!(index.file_fingerprint("a.txt"), baseline);
    }

    #[test]
    fn removing_a_file_keeps_other_sites_for_the_same_name_in_order() {
        let mut index = ReferenceIndex::default();
        for (file, line) in [("a.txt", 1), ("b.txt", 2), ("c.txt", 3)] {
            index.merge(file, vec![collected("t", "shared", line)]);
        }
        index.merge("b.txt", vec![collected("t", "shared", 4)]);

        index.remove_file("b.txt");
        assert_eq!(
            lines(&index.references("t", "shared")),
            [("a.txt", 1), ("c.txt", 3)]
        );
        index.remove_file("a.txt");
        assert_eq!(lines(&index.references("t", "shared")), [("c.txt", 3)]);
    }

    #[test]
    fn a_file_records_each_name_once_however_often_it_uses_it() {
        let mut index = ReferenceIndex::default();
        index.merge(
            "a.txt",
            vec![
                collected("t", "same", 1),
                collected("t", "SAME", 2),
                collected("t", "same", 3),
            ],
        );
        assert_eq!(index.file_keys["a.txt"].len(), 1);
        assert_eq!(index.references_ci("t", "same").len(), 3);
    }

    #[test]
    fn removing_the_last_site_drops_the_name_and_type_buckets() {
        let mut index = ReferenceIndex::default();
        index.merge("a.txt", vec![collected("t", "only_a", 1)]);
        index.merge(
            "b.txt",
            vec![collected("t", "Both", 1), collected("u", "both", 2)],
        );
        index.merge("a.txt", vec![collected("u", "both", 3)]);

        index.remove_file("a.txt");
        assert!(!index.map["t"].contains_key("only_a"));
        assert_eq!(index.map["u"].len(), 1);
        assert_eq!(lines(&index.references_ci("u", "BOTH")), [("b.txt", 2)]);

        index.remove_file("never.txt");
        index.remove_file("b.txt");
        assert!(index.map.is_empty(), "{:?}", index.map);
        assert!(index.file_keys.is_empty(), "{:?}", index.file_keys);
    }

    #[test]
    fn alias_candidate_grouping_preserves_sites_patterns_and_file_order() {
        let patterns: Vec<_> = [
            ("", "", "effect"),
            ("pre_", "_post", "effect"),
            ("pre_", "_post", "other"),
            ("", "", "effect"),
        ]
        .into_iter()
        .map(|(prefix, suffix, type_name)| super::TypePatternAlias {
            prefix: prefix.to_string(),
            suffix: suffix.to_string(),
            type_name: type_name.to_string(),
        })
        .collect();
        let keys = [
            "pre_FOO_post",
            "plain",
            "pre_foo_post",
            "pre_FOO_post",
            "Pre_FOO_post",
            "pre__post",
            "ÄBC",
            "äbc",
            "",
        ];
        let sites: Vec<_> = keys
            .iter()
            .enumerate()
            .map(|(index, key)| super::AliasKeySite {
                key: (*key).to_string(),
                location: SourceLocation {
                    line: index as u32 + 1,
                    col: 0,
                    end: (index as u32 + 1, key.len() as u16),
                },
            })
            .collect();
        let mut cache = super::AliasKeyCache::default();
        let mut expected: HashMap<_, Vec<_>> = HashMap::new();
        for uri in ["other.txt", "caller.txt"] {
            let merged = cache.merge_file(uri, sites.clone(), &patterns);
            let mut file_keys = HashSet::new();
            for (site_index, key) in keys.iter().enumerate() {
                for (pattern_index, pattern) in patterns.iter().enumerate() {
                    if let Some(middle) = super::strip_affix(key, &pattern.prefix, &pattern.suffix)
                    {
                        let key = (pattern.type_name.clone(), middle.to_ascii_lowercase());
                        expected.entry(key.clone()).or_default().push((
                            uri.to_string(),
                            site_index,
                            pattern_index,
                        ));
                        file_keys.insert(key);
                    }
                }
            }
            assert_eq!(merged.into_iter().collect::<HashSet<_>>(), file_keys);
        }
        let actual: HashMap<_, Vec<_>> = cache
            .by_instance
            .iter()
            .map(|(key, candidates)| {
                (
                    key.clone(),
                    candidates
                        .iter()
                        .map(|candidate| {
                            (
                                candidate.file.to_string(),
                                candidate.site_index,
                                candidate.pattern_index,
                            )
                        })
                        .collect(),
                )
            })
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(cache.candidates_for("effect", "FOO").len(), 6);
        assert_eq!(cache.candidates_for("effect", "ÄBC").len(), 4);
        assert_eq!(cache.candidates_for("effect", "äbc").len(), 4);

        cache.merge_file("caller.txt", sites[..1].to_vec(), &patterns);
        let remaining = cache.candidates_for("effect", "foo");
        assert_eq!(remaining.len(), 4);
        assert_eq!(remaining.last().unwrap().file.as_ref(), "caller.txt");
        assert_eq!(remaining.last().unwrap().site_index, 0);
        assert!(
            cache
                .candidates_for("effect", "plain")
                .iter()
                .all(|candidate| { candidate.file.as_ref() == "other.txt" })
        );
        assert!(
            cache
                .merge_file("caller.txt", Vec::new(), &patterns)
                .is_empty()
        );
        assert_eq!(cache.candidates_for("effect", "foo").len(), 3);
        cache.remove_file("other.txt");
        assert!(cache.by_instance.is_empty());
        assert!(cache.file_keys.is_empty());
        assert!(cache.sites.is_empty());
        assert!(cache.merge_file("no-patterns.txt", sites, &[]).is_empty());
        assert_eq!(cache.sites("no-patterns.txt").len(), keys.len());
        cache.remove_file("no-patterns.txt");
        assert!(cache.file_keys.is_empty());
        assert!(cache.sites.is_empty());
    }

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
    fn alias_refresh_cost_tracks_changed_names_not_all_pattern_instances() {
        let table = StringTable::new();
        let rules = ast_to_ruleset(&parse_string(RULES, &table), &table);
        let mut service = InfoService::new();

        // A large patterned type index reproduces the old per-edit fingerprint
        // scan. The measured count below is exact candidate-site work, not time.
        let instances: Vec<_> = (0..100_000)
            .map(|idx| TypeInstance {
                name: format!("effect_{idx}"),
                location: SourceLocation {
                    line: idx,
                    col: 0,
                    end: (idx, 0),
                },
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            })
            .collect();
        Arc::make_mut(&mut service.type_index).merge(
            "definitions.txt",
            HashMap::from([("scripted_effect".to_string(), instances.clone())]),
        );
        service.type_index.take_instance_name_changes();

        let caller = "my_se = yes\n";
        service.index_file_with_path(
            "caller.txt",
            &parse_string(caller, &table),
            &table,
            &rules,
            "common/decisions/caller.txt",
        );
        service.rebuild_alias_key_index(&rules);

        // The no-change path must not use Arc::make_mut on the large index.
        let held_snapshot = Arc::clone(&service.type_index);
        assert_eq!(service.refresh_alias_key_names(&rules), None);
        assert!(Arc::ptr_eq(&held_snapshot, &service.type_index));

        // Replacing the 100k-name file with the same exports coalesces its
        // removal/addition within one atomic file operation. No alias sites
        // need reclassification.
        let type_index = Arc::make_mut(&mut service.type_index);
        type_index.begin_file_replacement("definitions.txt");
        type_index.remove_file("definitions.txt");
        type_index.merge(
            "definitions.txt",
            HashMap::from([("scripted_effect".to_string(), instances)]),
        );
        type_index.finish_file_replacement("definitions.txt");
        assert!(type_index.take_instance_name_changes().is_empty());
        assert_eq!(service.refresh_alias_key_names(&rules), None);
        assert!(
            service
                .alias_key_index
                .references("scripted_effect", "my_se")
                .is_empty()
        );

        // Adding and removing one matching export visits only the one site
        // whose candidate key carries that name.
        Arc::make_mut(&mut service.type_index).merge(
            "new_definition.txt",
            HashMap::from([(
                "scripted_effect".to_string(),
                vec![TypeInstance {
                    name: "my_se".to_string(),
                    location: SourceLocation {
                        line: 0,
                        col: 0,
                        end: (0, 0),
                    },
                    primary_loc_key: None,
                    required_loc_keys: Vec::new(),
                }],
            )]),
        );
        assert_eq!(service.refresh_alias_key_names(&rules), Some(1));
        assert_eq!(
            lines(
                &service
                    .alias_key_index
                    .references("scripted_effect", "my_se")
            ),
            [("caller.txt", 1)]
        );

        Arc::make_mut(&mut service.type_index).remove_file("new_definition.txt");
        assert_eq!(service.refresh_alias_key_names(&rules), Some(1));
        assert!(
            service
                .alias_key_index
                .references("scripted_effect", "my_se")
                .is_empty()
        );
    }

    #[test]
    fn alias_refresh_keeps_reclassification_dirty_across_absent_reindex() {
        let table = StringTable::new();
        let rules = ast_to_ruleset(&parse_string(RULES, &table), &table);
        let mut service = indexed(&[(
            "effects.txt",
            "common/scripted_effects/effects.txt",
            "my_se = { log = hi }\n",
        )]);

        // The export disappears, then a caller is indexed while it is absent.
        // It cannot yet be classified as a reference.
        service.clear_file("effects.txt");
        service.index_file_with_path(
            "caller.txt",
            &parse_string("my_se = yes\n", &table),
            &table,
            &rules,
            "common/decisions/caller.txt",
        );
        assert!(
            service
                .alias_key_index
                .references("scripted_effect", "my_se")
                .is_empty()
        );

        // Restoring the export cancels the net remove/add transition, but the
        // candidate classified during the absent interval remains dirty.
        service.index_file_with_path(
            "effects.txt",
            &parse_string("my_se = { log = hi }\n", &table),
            &table,
            &rules,
            "common/scripted_effects/effects.txt",
        );
        assert_eq!(service.refresh_alias_key_names(&rules), Some(2));
        assert_eq!(
            lines(
                &service
                    .alias_key_index
                    .references("scripted_effect", "my_se")
            ),
            [("caller.txt", 1)]
        );

        let incrementally_indexed_sites = service
            .alias_key_index
            .references("scripted_effect", "my_se");
        let incrementally_classified = lines(&incrementally_indexed_sites);
        service.rebuild_alias_key_index(&rules);
        let rebuilt_sites = service
            .alias_key_index
            .references("scripted_effect", "my_se");
        assert_eq!(incrementally_classified, lines(&rebuilt_sites));
    }

    #[test]
    fn bounded_references_cap_storage_and_exclude_open_files() {
        let mut files = vec![(
            "e.txt",
            "common/scripted_effects/e.txt",
            "my_se = { log = hi }\n",
        )];
        const CALLER_URIS: [&str; 12] = [
            "d0.txt", "d1.txt", "d2.txt", "d3.txt", "d4.txt", "d5.txt", "d6.txt", "d7.txt",
            "d8.txt", "d9.txt", "d10.txt", "d11.txt",
        ];
        let callers: Vec<String> = (0..CALLER_URIS.len())
            .map(|idx| format!("my_dec_{idx} = {{ complete_effect = {{ my_se = yes }} }}\n"))
            .collect();
        for (uri, source) in CALLER_URIS.iter().zip(&callers) {
            files.push((uri, "common/decisions/d.txt", source));
        }
        let svc = indexed(&files);
        let excluded = HashSet::from(["d0.txt".to_string()]);
        let (sites, total) =
            svc.alias_key_index
                .references_ci_bounded("scripted_effect", "MY_SE", 3, &excluded);

        assert_eq!(sites.len(), 3);
        assert_eq!(total, 11);
        assert!(sites.iter().all(|site| site.file.as_ref() != "d0.txt"));
        assert_eq!(
            svc.alias_key_index
                .references_ci("scripted_effect", "MY_SE")
                .len(),
            12
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
