use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::derived::DerivedIndex;
use crate::dynamic_values;
use crate::{DerivedNames, SourceLocation, dec_ref, is_subtype_key};

/// How many derivations one lookup follows: a renamed copy of a variant that
/// inherits its subtype is two.
const DERIVATION_DEPTH: u8 = 4;

#[inline]
fn ascii_lowercase_if_needed(value: &str) -> Cow<'_, str> {
    if value.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Owned(value.to_ascii_lowercase())
    } else {
        Cow::Borrowed(value)
    }
}

#[derive(Debug, Clone)]
pub struct TypeInstance {
    pub name: String,
    pub location: SourceLocation,
    /// The loc key for the type's `## primary` localisation when taken from an explicit
    /// field (e.g. an event's `title = <key>`), captured so hover can show the localized
    /// title for a reference in another file.
    pub primary_loc_key: Option<String>,
    /// Loc keys this instance must provide that a `## required` localisation
    /// entry takes from a child field's value (`## required title = title`).
    /// Resolved here because the node body is gone by the time CW100 runs off the index.
    pub required_loc_keys: Vec<String>,
}

/// An index of every file path under the game roots, used to check that `filepath` references
/// resolve (CW113). Case-insensitive by default (Windows-authored mods); exact on-disk case
/// after [`FileIndex::set_case_sensitive`], so a case-only mismatch is caught.
#[derive(Debug, Clone, Default)]
pub struct FileIndex {
    files: FxHashSet<String>,
    files_exact: FxHashMap<String, String>,
    case_sensitive: bool,
    /// Keys the primary workspace supplies; a watched delete drops these first (#1067).
    owned: FxHashSet<String>,
    /// Owned keys a parent mod or the base game also provides, which survive a delete.
    overridden: FxHashSet<String>,
}

impl FileIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_root(&mut self, root: &std::path::Path) {
        Self::walk(
            root,
            root,
            &mut self.files,
            self.case_sensitive.then_some(&mut self.files_exact),
        );
    }

    fn walk(
        root: &std::path::Path,
        dir: &std::path::Path,
        out: &mut FxHashSet<String>,
        mut out_exact: Option<&mut FxHashMap<String, String>>,
    ) {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!("FileIndex::walk: cannot read {}: {e}", dir.display());
                return;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                Self::walk(root, &path, out, out_exact.as_deref_mut());
            } else if let Ok(rel) = path.strip_prefix(root)
                && let Some(s) = rel.to_str()
            {
                let norm = s.replace('\\', "/");
                out.insert(norm.to_ascii_lowercase());
                if let Some(exact) = out_exact.as_deref_mut() {
                    exact.insert(norm.to_ascii_lowercase(), norm);
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether a game-relative path exists. Case-insensitive by default;
    /// case-sensitive for live-walked files after [`FileIndex::set_case_sensitive`].
    pub fn contains(&self, path: &str) -> bool {
        if self.case_sensitive {
            self.contains_exact(path)
        } else {
            self.contains_ci(path)
        }
    }

    /// Case-insensitive membership (the default; tolerant of Windows-authored mods).
    fn contains_ci(&self, path: &str) -> bool {
        thread_local! {
            static NORM_BUF: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
        }
        NORM_BUF.with(|buf| {
            let mut norm = buf.borrow_mut();
            norm.clear();
            let mut first = true;
            for seg in path.trim().split(['/', '\\']).filter(|s| !s.is_empty()) {
                if !first {
                    norm.push('/');
                }
                first = false;
                norm.extend(seg.chars().map(|c| c.to_ascii_lowercase()));
            }
            self.files.contains(norm.as_str())
        })
    }

    /// Exact-case membership: a reference matching a known path only case-insensitively
    /// counts as absent (it would fail to load on a case-sensitive filesystem). Falls back
    /// to case-insensitive membership for a path whose on-disk case was never recorded.
    fn contains_exact(&self, path: &str) -> bool {
        thread_local! {
            static NORM_BUF: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
        }
        NORM_BUF.with(|buf| {
            let mut norm = buf.borrow_mut();
            norm.clear();
            let mut first = true;
            for seg in path.trim().split(['/', '\\']).filter(|s| !s.is_empty()) {
                if !first {
                    norm.push('/');
                }
                first = false;
                norm.push_str(seg);
            }
            let norm_ci = norm.to_ascii_lowercase();
            match self.files_exact.get(&norm_ci) {
                Some(orig) => orig.as_str() == norm.as_str(),
                None => self.files.contains(norm_ci.as_str()),
            }
        })
    }

    pub fn on_disk_case(&self, path: &str) -> Option<&str> {
        let norm: String = path
            .trim()
            .split(['/', '\\'])
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("/");
        let orig = self.files_exact.get(&norm.to_ascii_lowercase())?;
        (orig.as_str() != norm).then_some(orig.as_str())
    }

    pub fn insert(&mut self, path: &str) {
        let norm = path.replace('\\', "/");
        let ci = norm.to_ascii_lowercase();
        self.files.insert(ci.clone());
        if self.case_sensitive {
            self.files_exact.insert(ci, norm);
        }
    }

    /// Mark `path` as supplied by the primary workspace. Idempotent for an already-claimed
    /// key; `true` when the index changed: a new key, or (case-sensitive mode) an exact
    /// spelling that replaces the previous one.
    pub fn claim_workspace(&mut self, path: &str) -> bool {
        let spelling = path.replace('\\', "/");
        let norm = spelling.to_ascii_lowercase();
        let fresh = self.files.insert(norm.clone());
        // A key already present without a workspace claim came from a lower layer.
        if !fresh && !self.owned.contains(&norm) {
            self.overridden.insert(norm.clone());
        }
        self.owned.insert(norm.clone());
        if self.case_sensitive && self.files_exact.get(&norm) != Some(&spelling) {
            self.files_exact.insert(norm, spelling);
            true
        } else {
            fresh
        }
    }

    /// Whether the primary workspace supplies `path`, so `release_workspace` would change
    /// something. A read-only check lets callers skip the index clone otherwise (#1067).
    pub fn is_workspace_owned(&self, path: &str) -> bool {
        self.owned
            .contains(&path.replace('\\', "/").to_ascii_lowercase())
    }

    /// The keys the primary workspace supplies inside any of the folders `dirs`, for a
    /// folder delete (#1092). Matches whole path segments, so `foo` never reaches
    /// `foobar/x.dds`; a folder is not a key of its own.
    pub fn workspace_owned_under(&self, dirs: &[String]) -> Vec<String> {
        let dirs: FxHashSet<String> = dirs
            .iter()
            .map(|dir| {
                dir.replace('\\', "/")
                    .trim_matches('/')
                    .to_ascii_lowercase()
            })
            .filter(|dir| !dir.is_empty())
            .collect();
        self.owned
            .iter()
            .filter(|key| {
                let mut parent = key.as_str();
                while let Some((head, _)) = parent.rsplit_once('/') {
                    if dirs.contains(head) {
                        return true;
                    }
                    parent = head;
                }
                false
            })
            .cloned()
            .collect()
    }

    /// Whether `path` is already workspace-owned with the spelling a claim keeps, so
    /// `claim_workspace` would change nothing (#1067).
    pub fn is_workspace_claimed(&self, path: &str) -> bool {
        let spelling = path.replace('\\', "/");
        let norm = spelling.to_ascii_lowercase();
        if !self.owned.contains(&norm) {
            return false;
        }
        !self.case_sensitive || self.files_exact.get(&norm) == Some(&spelling)
    }

    /// Drop the workspace's own contribution of `path`: a watched delete (#1067). A key
    /// a lower layer still provides stays in the index; returns whether the key left it.
    pub fn release_workspace(&mut self, path: &str) -> bool {
        let norm = path.replace('\\', "/").to_ascii_lowercase();
        if !self.owned.remove(&norm) {
            return false;
        }
        // The lower layer's exact case is not kept, so a case-sensitive index stops
        // checking case for the key instead of keeping the deleted file's spelling.
        self.files_exact.remove(&norm);
        if self.overridden.remove(&norm) {
            return false;
        }
        self.files.remove(&norm);
        true
    }

    /// Add paths a parent mod or the base game provides, after the workspace's claims.
    /// A claimed key keeps the workspace spelling and survives its release (#1067).
    /// A borrowed spelling is copied only when a new exact spelling is inserted (#1154).
    pub fn add_paths<'a, I: IntoIterator<Item: Into<Cow<'a, str>>>>(&mut self, paths: I) {
        for path in paths {
            let path = path.into();
            let ci = path.as_ref().to_ascii_lowercase();
            if self.owned.contains(&ci) {
                self.overridden.insert(ci.clone());
            }
            if self.case_sensitive {
                self.files_exact
                    .entry(ci.clone())
                    .or_insert_with(move || path.into_owned());
            }
            self.files.insert(ci);
        }
    }

    /// Toggle exact-case matching. Off by default (Windows-authored mods); enable for mods
    /// that also target case-sensitive filesystems (Linux/Mac), so a reference differing from
    /// the file only by case is flagged. Call before building the index.
    pub fn set_case_sensitive(&mut self, case_sensitive: bool) {
        self.case_sensitive = case_sensitive;
    }

    pub fn paths(&self) -> impl Iterator<Item = &String> {
        self.files.iter()
    }

    /// The on-disk-case relative paths, for persisting to the vanilla cache so a later
    /// case-sensitive run can restore exact case. Empty unless
    /// [`FileIndex::set_case_sensitive`] was set before the paths were added.
    pub fn paths_exact(&self) -> impl Iterator<Item = &String> {
        self.files_exact.values()
    }

    pub fn resolve_relative(&self, referencing_file: &str, value: &str) -> bool {
        let segs: Vec<String> = referencing_file
            .split(['/', '\\'])
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase())
            .collect();
        if segs.len() < 2 {
            return false;
        }
        for start in 0..segs.len() - 1 {
            let self_path = segs[start..].join("/");
            if self.files.contains(&self_path) {
                let dir = &segs[start..segs.len() - 1];
                let sibling = if dir.is_empty() {
                    value.to_string()
                } else {
                    format!("{}/{}", dir.join("/"), value)
                };
                // Stays case-insensitive even in case-sensitive mode: the directory
                // segments come from the lowercased referencing path, so an exact
                // compare cannot be trusted.
                return self.contains_ci(&sibling);
            }
        }
        false
    }
}

#[derive(Debug, Clone, Default)]
pub struct VarIndex {
    names: HashMap<String, usize>,
    /// Base-game variables staged from the vanilla cache or a live walk. Kept apart
    /// from `names` so a `clear_file` on a mod file sharing a name does not strip the
    /// base-game definition, and a re-merge replaces the previous contribution (#306).
    vanilla_names: FxHashSet<String>,
}

impl VarIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty() && self.vanilla_names.is_empty()
    }

    pub fn len(&self) -> usize {
        let mut len = self.names.len();
        for v in &self.vanilla_names {
            if !self.names.contains_key(v.as_str()) {
                len += 1;
            }
        }
        len
    }

    /// Normalized lookup key for a raw variable token: lowercased, unquoted, the base
    /// before any `@`-concatenation, the last `.`-segment of that base, and before any
    /// `?`/`^` selector. Mirrors F# `getVariableFromString` plus the read-side dot-split.
    pub fn normalize(raw: &str) -> String {
        let mut buf = String::new();
        Self::normalize_into(raw, &mut buf);
        buf
    }

    pub fn normalize_into(raw: &str, buf: &mut String) {
        let s = raw.trim().trim_matches('"');
        let before_amp = s.split('@').next().unwrap_or(s);
        let last_seg = before_amp.rsplit('.').next().unwrap_or(before_amp);
        let core = last_seg.split(['?', '^']).next().unwrap_or(last_seg);
        buf.clear();
        buf.extend(core.trim().chars().map(|c| c.to_ascii_lowercase()));
    }

    pub fn add_name(&mut self, raw: &str) {
        let n = Self::normalize(raw);
        if !n.is_empty() {
            *self.names.entry(n).or_insert(0) += 1;
        }
    }

    pub fn remove_name(&mut self, raw: &str) {
        let n = Self::normalize(raw);
        dec_ref(&mut self.names, n.as_str());
    }

    pub fn contains(&self, raw: &str) -> bool {
        thread_local! {
            static NORM_BUF: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
        }
        NORM_BUF.with(|buf| {
            let mut buf = buf.borrow_mut();
            Self::normalize_into(raw, &mut buf);
            self.names.contains_key(buf.as_str()) || self.vanilla_names.contains(buf.as_str())
        })
    }

    /// Replace the base-game contribution with `names` (normalized). A re-merge drops
    /// the previous set, and a name the mod also defines survives a `clear_file` (#306).
    pub fn set_vanilla_names(&mut self, names: &[String]) {
        self.vanilla_names.clear();
        for raw in names {
            let n = Self::normalize(raw);
            if !n.is_empty() {
                self.vanilla_names.insert(n);
            }
        }
    }

    pub fn clear_vanilla_names(&mut self) {
        self.vanilla_names.clear();
    }

    pub fn merge(&mut self, other: &VarIndex) {
        for (name, count) in &other.names {
            *self.names.entry(name.clone()).or_insert(0) += count;
        }
    }

    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.names.keys()
    }

    #[cfg(test)]
    pub(crate) fn vanilla_names_iter(&self) -> impl Iterator<Item = &String> {
        self.vanilla_names.iter()
    }

    pub(crate) fn all_names(&self) -> impl Iterator<Item = &String> {
        self.names.keys().chain(
            self.vanilla_names
                .iter()
                .filter(|k| !self.names.contains_key(k.as_str())),
        )
    }
}

/// Scripted-loc names (`defined_text = { name = X }`) read off the files in a scripted-loc
/// folder, since the HOI4 config types `scripted_loc` under Stellaris's folder and every use
/// read as an unknown command (#348). Lowercased and refcounted per file, like [`VarIndex`].
#[derive(Debug, Clone, Default)]
pub struct ScriptedLocIndex {
    names: FxHashMap<Arc<str>, usize>,
    per_file: FxHashMap<Arc<str>, Vec<Arc<str>>>,
    vanilla_names: FxHashSet<Arc<str>>,
}

impl ScriptedLocIndex {
    pub fn merge_file(&mut self, file_uri: &str, names: Vec<String>) {
        self.remove_file(file_uri);
        if names.is_empty() {
            return;
        }
        let mut flat: Vec<Arc<str>> = Vec::with_capacity(names.len());
        for name in names {
            let key: Arc<str> = Arc::from(name.to_ascii_lowercase().as_str());
            *self.names.entry(Arc::clone(&key)).or_insert(0) += 1;
            flat.push(key);
        }
        self.per_file.insert(Arc::from(file_uri), flat);
    }

    pub fn remove_file(&mut self, file_uri: &str) {
        let Some(flat) = self.per_file.remove(file_uri) else {
            return;
        };
        for name in flat {
            dec_ref(&mut self.names, name.as_ref());
        }
    }

    pub fn set_vanilla_names(&mut self, names: &[String]) {
        self.vanilla_names = names
            .iter()
            .filter(|n| !n.is_empty())
            .map(|n| Arc::from(n.to_ascii_lowercase().as_str()))
            .collect();
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty() && self.vanilla_names.is_empty()
    }

    pub fn contains(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        self.names.contains_key(lower.as_str()) || self.vanilla_names.contains(lower.as_str())
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.names.keys().map(Arc::as_ref)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScriptedGuiIndex {
    names: FxHashMap<Arc<str>, usize>,
    per_file: FxHashMap<Arc<str>, Vec<Arc<str>>>,
    vanilla_names: FxHashSet<Arc<str>>,
}

impl ScriptedGuiIndex {
    pub fn merge_file(&mut self, file_uri: &str, names: Vec<String>) {
        self.remove_file(file_uri);
        if names.is_empty() {
            return;
        }
        let mut flat = Vec::with_capacity(names.len());
        for name in names {
            let key: Arc<str> = Arc::from(name.to_ascii_lowercase().as_str());
            *self.names.entry(Arc::clone(&key)).or_insert(0) += 1;
            flat.push(key);
        }
        self.per_file.insert(Arc::from(file_uri), flat);
    }

    pub fn remove_file(&mut self, file_uri: &str) {
        let Some(flat) = self.per_file.remove(file_uri) else {
            return;
        };
        for name in flat {
            dec_ref(&mut self.names, name.as_ref());
        }
    }

    pub fn set_vanilla_names(&mut self, names: &[String]) {
        self.vanilla_names = names
            .iter()
            .filter(|name| !name.is_empty())
            .map(|name| Arc::from(name.to_ascii_lowercase().as_str()))
            .collect();
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty() && self.vanilla_names.is_empty()
    }

    pub fn contains(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        self.names.contains_key(lower.as_str()) || self.vanilla_names.contains(lower.as_str())
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.names.keys().map(Arc::as_ref)
    }
}

#[derive(Debug, Default, Clone)]
struct NameRefs {
    total: usize,
    workspace: usize,
    /// Where this name's instances sit in the type's `map` entry, unordered.
    /// Most names occur once, so two inline slots avoid a heap allocation.
    positions: SmallVec<[usize; 2]>,
}

fn release_name(set: &mut FxHashMap<Arc<str>, NameRefs>, name: &str, base_game: bool) -> bool {
    if let Some(refs) = set.get_mut(name)
        && refs.release(base_game)
    {
        set.remove(name);
        return true;
    }
    false
}

/// [`release_name`] for one instance whose `map` position is known.
fn release_name_at(
    set: &mut FxHashMap<Arc<str>, NameRefs>,
    name: &str,
    base_game: bool,
    position: usize,
) -> bool {
    let Some(refs) = set.get_mut(name) else {
        return false;
    };
    if let Some(slot) = refs.positions.iter().position(|&p| p == position) {
        refs.positions.swap_remove(slot);
    }
    let gone = refs.release(base_game);
    if gone {
        set.remove(name);
    }
    gone
}

impl NameRefs {
    fn add(&mut self, base_game: bool, position: usize) {
        self.total += 1;
        if !base_game {
            self.workspace += 1;
        }
        self.positions.push(position);
    }

    fn release(&mut self, base_game: bool) -> bool {
        self.total = self.total.saturating_sub(1);
        if !base_game {
            self.workspace = self.workspace.saturating_sub(1);
        }
        self.total == 0
    }
}

#[derive(Debug, Clone, Default)]
pub struct TypeIndex {
    /// Private because `instance_sets`, `name_counts` and `file_positions` are
    /// derived from it; read it through [`TypeIndex::map`].
    map: FxHashMap<String, Vec<(Arc<str>, TypeInstance)>>,
    name_counts: FxHashMap<Arc<str>, usize>,
    instance_sets: FxHashMap<String, FxHashMap<Arc<str>, NameRefs>>,
    /// Membership changes since the alias-key consumer last drained them.
    /// Shared and lockable so draining through `&self` never copy-on-writes
    /// an `Arc<TypeIndex>` that a snapshot still holds.
    instance_name_changes: Arc<std::sync::Mutex<InstanceNameChanges>>,
    /// Names touched by the current atomic file replacement, paired with their
    /// membership before its first transition.
    file_replacement: Option<FileReplacementChanges>,
    base_game_uris: FxHashSet<Arc<str>>,
    file_positions: FxHashMap<Arc<str>, FxHashMap<String, Vec<usize>>>,
    derived: DerivedIndex,
    pub file_index: FileIndex,
    pub var_index: VarIndex,
    pub scripted_loc_index: ScriptedLocIndex,
    pub scripted_gui_index: ScriptedGuiIndex,
    pub complete: bool,
    pub complex_enum_values: dynamic_values::NamedValueIndex,
    pub value_set_values: dynamic_values::NamedValueIndex,
}

#[derive(Debug, Clone, Default)]
struct FileReplacementChanges {
    file_uri: String,
    touched: FxHashMap<(String, Arc<str>), bool>,
}

#[derive(Debug, Default)]
struct InstanceNameChanges {
    /// Net membership changes. Opposing transitions coalesce within a file
    /// replacement, so an unchanged export set causes no refresh work.
    changed: FxHashMap<String, FxHashSet<Arc<str>>>,
    /// Names whose alias classification became dirty while absent. Unlike
    /// `changed`, these survive a later add/remove parity cancellation.
    dirty: FxHashMap<String, FxHashSet<Arc<str>>>,
}

impl TypeIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every indexed instance, keyed by type name.
    pub fn map(&self) -> &FxHashMap<String, Vec<(Arc<str>, TypeInstance)>> {
        &self.map
    }

    pub fn into_map(self) -> FxHashMap<String, Vec<(Arc<str>, TypeInstance)>> {
        self.map
    }

    pub fn contains(&self, type_name: &str, instance: &str) -> bool {
        self.contains_lower(
            type_name,
            &ascii_lowercase_if_needed(instance),
            DERIVATION_DEPTH,
        )
    }

    fn contains_declared(&self, type_name: &str, lower: &str) -> bool {
        self.instance_sets
            .get(type_name)
            .is_some_and(|names| names.contains_key(lower))
    }

    fn contains_lower(&self, type_name: &str, lower: &str, depth: u8) -> bool {
        self.contains_declared(type_name, lower)
            || (depth > 0
                && !self.derived.is_empty()
                && self.derived.contains(type_name, lower, |t, n| {
                    self.contains_lower(t, n, depth - 1)
                }))
    }

    /// Store every derived membership as a plain instance, for an index that
    /// is about to lose its derivation records (the base-game cache keeps only
    /// instances).
    pub fn materialize_derived(&mut self) {
        let type_names: Vec<&str> = self.instance_sets.keys().map(String::as_str).collect();
        let per_file = self.derived.materialize(
            &type_names,
            |type_name| {
                self.instance_sets
                    .get(type_name)
                    .map(|names| names.keys().map(AsRef::as_ref).collect())
                    .unwrap_or_default()
            },
            |type_name, name| self.contains_declared(type_name, name),
            |type_name, name| self.contains(type_name, name),
        );
        for (uri, per_type) in per_file {
            self.merge(&uri, per_type);
        }
    }

    pub fn workspace_definition_count(&self, type_name: &str, instance: &str) -> usize {
        let Some(names) = self.instance_sets.get(type_name) else {
            return 0;
        };
        let refs = if instance.bytes().any(|b| b.is_ascii_uppercase()) {
            names.get(instance.to_ascii_lowercase().as_str())
        } else {
            names.get(instance)
        };
        refs.map(|r| r.workspace).unwrap_or(0)
    }

    pub fn is_any_instance(&self, name: &str) -> bool {
        if name.bytes().any(|b| b.is_ascii_uppercase()) {
            let lower = name.to_ascii_lowercase();
            self.name_counts.contains_key(lower.as_str())
        } else {
            self.name_counts.contains_key(name)
        }
    }

    pub fn instances(&self, type_name: &str) -> &[(Arc<str>, TypeInstance)] {
        self.map.get(type_name).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Drain the names whose membership changed or was marked dirty since the
    /// last drain, sorted and deduplicated.
    pub fn take_instance_name_changes(&self) -> Vec<(String, Arc<str>)> {
        let mut changes = self.instance_name_changes.lock().unwrap();
        let InstanceNameChanges { changed, dirty } = std::mem::take(&mut *changes);
        let mut out: Vec<(String, Arc<str>)> = changed
            .into_iter()
            .chain(dirty)
            .flat_map(|(type_name, names)| {
                names.into_iter().map(move |name| (type_name.clone(), name))
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Whether [`TypeIndex::take_instance_name_changes`] would return anything.
    pub fn has_instance_name_changes(&self) -> bool {
        let changes = self.instance_name_changes.lock().unwrap();
        !changes.changed.is_empty() || !changes.dirty.is_empty()
    }

    /// Whether one specific name is waiting for alias reclassification.
    pub fn has_pending_instance_name_change(&self, type_name: &str, lower_name: &str) -> bool {
        let changes = self.instance_name_changes.lock().unwrap();
        changes
            .changed
            .get(type_name)
            .is_some_and(|names| names.contains(lower_name))
            || changes
                .dirty
                .get(type_name)
                .is_some_and(|names| names.contains(lower_name))
    }

    /// Mark a name for reclassification when an alias candidate was indexed
    /// while the export was absent. This survives a later restore of the name.
    pub fn mark_instance_name_dirty(&self, type_name: &str, lower_name: Arc<str>) {
        self.instance_name_changes
            .lock()
            .unwrap()
            .dirty
            .entry(type_name.to_string())
            .or_default()
            .insert(lower_name);
    }

    /// Begin an atomic remove/reindex/merge operation for one file. Membership
    /// changes are diffed at the end so unchanged exports coalesce locally.
    pub fn begin_file_replacement(&mut self, file_uri: &str) {
        debug_assert!(self.file_replacement.is_none());
        self.file_replacement = Some(FileReplacementChanges {
            file_uri: file_uri.to_string(),
            touched: FxHashMap::default(),
        });
    }

    /// End the current atomic file replacement and journal only names whose
    /// global membership differs from its starting state.
    pub fn finish_file_replacement(&mut self, file_uri: &str) {
        let Some(replacement) = self.file_replacement.take() else {
            return;
        };
        debug_assert_eq!(replacement.file_uri, file_uri);
        let mut changes = self.instance_name_changes.lock().unwrap();
        for ((type_name, name), was_present) in replacement.touched {
            if self.contains(&type_name, &name) != was_present {
                changes.changed.entry(type_name).or_default().insert(name);
            }
        }
    }

    /// Record that `lower_name` entered (`added`) or left the type's name set.
    fn toggle_instance_name(&mut self, type_name: &str, lower_name: Arc<str>, added: bool) {
        if let Some(touched) = self.file_replacement.as_mut() {
            touched
                .touched
                .entry((type_name.to_string(), lower_name))
                .or_insert(!added);
        } else {
            self.instance_name_changes
                .lock()
                .unwrap()
                .changed
                .entry(type_name.to_string())
                .or_default()
                .insert(lower_name);
        }
    }

    pub fn is_instance_at(
        &self,
        type_name: &str,
        uri: &str,
        name: &str,
        line: u32,
        col: u16,
    ) -> bool {
        let Some(positions) = self
            .file_positions
            .get(uri)
            .and_then(|types| types.get(type_name))
        else {
            return false;
        };
        let Some(entries) = self.map.get(type_name) else {
            return false;
        };
        positions.iter().any(|&idx| {
            let (_, inst) = &entries[idx];
            inst.location.line == line
                && inst.location.col == col
                && inst.name.eq_ignore_ascii_case(name)
        })
    }

    pub fn instance_locations(&self, name: &str) -> Vec<(Arc<str>, SourceLocation)> {
        let name = ascii_lowercase_if_needed(name);
        let mut locations = Vec::new();
        // One name lookup per type, in map order, instead of every instance.
        for (type_name, entries) in &self.map {
            let Some(refs) = self
                .instance_sets
                .get(type_name)
                .and_then(|names| names.get(name.as_ref()))
            else {
                continue;
            };
            let mut positions = refs.positions.clone();
            positions.sort_unstable();
            locations.extend(positions.into_iter().map(|position| {
                let (uri, instance) = &entries[position];
                (Arc::clone(uri), instance.location)
            }));
        }
        locations
    }

    /// The explicit-field primary loc key captured for `name`'s instance of `type_name`
    /// (e.g. an event's `title`), if any. Lets hover show the localized title.
    pub fn primary_loc_key(&self, type_name: &str, name: &str) -> Option<&str> {
        let name = ascii_lowercase_if_needed(name);
        let entries = self.map.get(type_name)?;
        self.instance_sets
            .get(type_name)?
            .get(name.as_ref())?
            .positions
            .iter()
            .filter_map(|&position| {
                entries[position]
                    .1
                    .primary_loc_key
                    .as_deref()
                    .map(|key| (position, key))
            })
            .min_by_key(|(position, _)| *position)
            .map(|(_, key)| key)
    }

    pub fn loc_bindable_names(&self) -> impl Iterator<Item = String> + '_ {
        self.loc_bindable_names_iter().map(str::to_string)
    }

    pub(crate) fn loc_bindable_names_iter(&self) -> impl Iterator<Item = &str> + '_ {
        self.name_counts
            .keys()
            .map(AsRef::as_ref)
            .chain(self.var_index.all_names().map(String::as_str))
    }

    pub fn instances_in_file<'a>(&'a self, file_uri: &str) -> Vec<(&'a str, &'a TypeInstance)> {
        let Some(type_positions) = self.file_positions.get(file_uri) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(type_positions.values().map(Vec::len).sum());
        for (type_name, positions) in type_positions {
            if is_subtype_key(type_name) {
                continue;
            }
            let Some(entries) = self.map.get(type_name.as_str()) else {
                continue;
            };
            for &pos in positions {
                let (_, inst) = &entries[pos];
                out.push((type_name.as_str(), inst));
            }
        }
        out
    }

    /// Find one instance through the per-file position index without
    /// materializing every instance in the file.
    pub fn instance_at(
        &self,
        file_uri: &str,
        type_name: &str,
        name: &str,
        line: u32,
        col: u16,
    ) -> Option<&TypeInstance> {
        let positions = self.file_positions.get(file_uri)?.get(type_name)?;
        let entries = self.map.get(type_name)?;
        positions.iter().find_map(|&idx| {
            let instance = &entries[idx].1;
            (instance.location.line == line
                && instance.location.col == col
                && instance.name == name)
                .then_some(instance)
        })
    }

    pub fn instance_type_names_in_file<'a>(
        &'a self,
        file_uri: &str,
        base_type_name: &str,
        name: &str,
        location: SourceLocation,
    ) -> Vec<&'a str> {
        let Some(type_positions) = self.file_positions.get(file_uri) else {
            return Vec::new();
        };
        let mut type_names = Vec::new();
        for (type_name, positions) in type_positions {
            if type_name != base_type_name
                && !type_name
                    .strip_prefix(base_type_name)
                    .is_some_and(|suffix| suffix.starts_with('.'))
            {
                continue;
            }
            let Some(entries) = self.map.get(type_name.as_str()) else {
                continue;
            };
            if positions.iter().any(|&pos| {
                let (_, instance) = &entries[pos];
                instance.name == name
                    && instance.location.line == location.line
                    && instance.location.col == location.col
            }) {
                type_names.push(type_name.as_str());
            }
        }
        type_names
    }

    #[tracing::instrument(skip_all, fields(types = per_type.len()))]
    pub fn merge(&mut self, file_uri: &str, per_type: HashMap<String, Vec<TypeInstance>>) {
        self.merge_from(file_uri, per_type, false);
    }

    pub fn merge_base_game(
        &mut self,
        file_uri: &str,
        per_type: HashMap<String, Vec<TypeInstance>>,
    ) {
        self.merge_from(file_uri, per_type, true);
    }

    fn merge_from(
        &mut self,
        file_uri: &str,
        per_type: HashMap<String, Vec<TypeInstance>>,
        base_game: bool,
    ) {
        let uri: Arc<str> = Arc::from(file_uri);
        if base_game {
            self.base_game_uris.insert(Arc::clone(&uri));
        }
        for (type_name, instances) in per_type {
            let subtype_key = is_subtype_key(&type_name);
            let set = self.instance_sets.entry(type_name.clone()).or_default();
            let entry = self.map.entry(type_name.clone()).or_default();
            let positions = self
                .file_positions
                .entry(Arc::clone(&uri))
                .or_default()
                .entry(type_name.clone())
                .or_default();
            let mut added_names = Vec::new();
            for inst in instances {
                let lower = Arc::<str>::from(inst.name.to_ascii_lowercase());
                let lower = if subtype_key {
                    lower
                } else {
                    match self.name_counts.entry(lower) {
                        std::collections::hash_map::Entry::Occupied(mut entry) => {
                            *entry.get_mut() += 1;
                            Arc::clone(entry.key())
                        }
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            let lower = Arc::clone(entry.key());
                            entry.insert(1);
                            lower
                        }
                    }
                };
                let position = entry.len();
                let refs = set.entry(Arc::clone(&lower)).or_default();
                if refs.total == 0 {
                    added_names.push(lower);
                }
                refs.add(base_game, position);
                positions.push(position);
                entry.push((Arc::clone(&uri), inst));
            }
            for name in added_names {
                self.toggle_instance_name(&type_name, name, true);
            }
        }
    }

    pub fn merge_derived(&mut self, file_uri: &str, names: DerivedNames) {
        self.derived.merge(file_uri, names);
    }

    pub fn merge_base_game_with_uris(
        &mut self,
        per_type: impl IntoIterator<Item = (String, Vec<(Arc<str>, TypeInstance)>)>,
    ) {
        for (type_name, instances) in per_type {
            let subtype_key = is_subtype_key(&type_name);
            let set = self.instance_sets.entry(type_name.clone()).or_default();
            let entry = self.map.entry(type_name.clone()).or_default();
            let mut added_names = Vec::new();
            for (uri, inst) in instances {
                let lower = Arc::<str>::from(inst.name.to_ascii_lowercase());
                let lower = if subtype_key {
                    lower
                } else {
                    match self.name_counts.entry(lower) {
                        std::collections::hash_map::Entry::Occupied(mut entry) => {
                            *entry.get_mut() += 1;
                            Arc::clone(entry.key())
                        }
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            let lower = Arc::clone(entry.key());
                            entry.insert(1);
                            lower
                        }
                    }
                };
                let pos = entry.len();
                let refs = set.entry(Arc::clone(&lower)).or_default();
                if refs.total == 0 {
                    added_names.push(lower);
                }
                refs.add(true, pos);
                self.base_game_uris.insert(Arc::clone(&uri));
                let type_positions = self.file_positions.entry(Arc::clone(&uri)).or_default();
                match type_positions.get_mut(type_name.as_str()) {
                    Some(positions) => positions.push(pos),
                    None => {
                        type_positions.insert(type_name.clone(), vec![pos]);
                    }
                }
                entry.push((uri, inst));
            }
            for name in added_names {
                self.toggle_instance_name(&type_name, name, true);
            }
        }
    }

    pub fn remove_files(&mut self, file_uris: &HashSet<Arc<str>>) {
        if file_uris.is_empty() {
            return;
        }
        self.derived.remove_files(|uri| file_uris.contains(uri));
        let mut membership_changes = Vec::new();
        for (type_name, v) in self.map.iter_mut() {
            let subtype_key = is_subtype_key(type_name);
            v.retain(|(uri, inst)| {
                let keep = !file_uris.contains(uri);
                if !keep {
                    let base_game = self.base_game_uris.contains(uri);
                    let lower = inst.name.to_ascii_lowercase();
                    if !subtype_key {
                        dec_ref(&mut self.name_counts, lower.as_str());
                    }
                    if let Some(set) = self.instance_sets.get_mut(type_name)
                        && release_name(set, lower.as_str(), base_game)
                    {
                        membership_changes.push((type_name.clone(), Arc::<str>::from(lower)));
                    }
                }
                keep
            });
        }
        for uri in file_uris {
            self.base_game_uris.remove(uri);
        }
        self.map.retain(|_, v| !v.is_empty());
        self.instance_sets.retain(|_, names| !names.is_empty());
        for (type_name, lower_name) in membership_changes {
            self.toggle_instance_name(&type_name, lower_name, false);
        }
        // `retain` shifted the survivors, so both position indexes start over.
        self.file_positions.clear();
        for (type_name, entries) in &self.map {
            let mut names = self.instance_sets.get_mut(type_name);
            for refs in names.iter_mut().flat_map(|names| names.values_mut()) {
                refs.positions.clear();
            }
            for (i, (uri, inst)) in entries.iter().enumerate() {
                self.file_positions
                    .entry(Arc::clone(uri))
                    .or_default()
                    .entry(type_name.clone())
                    .or_default()
                    .push(i);
                if let Some(refs) = names
                    .as_mut()
                    .and_then(|names| names.get_mut(ascii_lowercase_if_needed(&inst.name).as_ref()))
                {
                    refs.positions.push(i);
                }
            }
        }
    }

    pub fn remove_file(&mut self, file_uri: &str) {
        self.complex_enum_values.remove_file(file_uri);
        self.value_set_values.remove_file(file_uri);
        self.scripted_loc_index.remove_file(file_uri);
        self.scripted_gui_index.remove_file(file_uri);
        self.derived.remove_files(|uri| uri == file_uri);
        let Some(type_positions) = self.file_positions.remove(file_uri) else {
            return;
        };
        let base_game = self.base_game_uris.remove(file_uri);
        for (type_name, mut positions) in type_positions {
            let subtype_key = is_subtype_key(&type_name);
            positions.sort_unstable_by(|a, b| b.cmp(a));
            for index in positions {
                let Some(v) = self.map.get(type_name.as_str()) else {
                    continue;
                };
                let lower = v[index].1.name.to_ascii_lowercase();
                if !subtype_key {
                    dec_ref(&mut self.name_counts, lower.as_str());
                }
                let removed_name = self
                    .instance_sets
                    .get_mut(&type_name)
                    .is_some_and(|set| release_name_at(set, lower.as_str(), base_game, index));
                if removed_name {
                    self.toggle_instance_name(&type_name, Arc::from(lower), false);
                }
                self.swap_remove_instance(&type_name, index);
            }
            if self.map.get(type_name.as_str()).is_some_and(Vec::is_empty) {
                self.map.remove(&type_name);
                self.instance_sets.remove(&type_name);
            }
        }
    }

    fn swap_remove_instance(&mut self, type_name: &str, index: usize) {
        let Some(v) = self.map.get_mut(type_name) else {
            return;
        };
        let last = v.len() - 1;
        v.swap_remove(index);
        if index == last {
            return;
        }
        // The former last entry now sits at `index`; repoint both indexes.
        let (moved_uri, moved) = &v[index];
        if let Some(slot) = self
            .file_positions
            .get_mut(moved_uri)
            .and_then(|by_type| by_type.get_mut(type_name))
            .and_then(|positions| positions.iter_mut().find(|p| **p == last))
        {
            *slot = index;
        }
        if let Some(slot) = self
            .instance_sets
            .get_mut(type_name)
            .and_then(|names| names.get_mut(ascii_lowercase_if_needed(&moved.name).as_ref()))
            .and_then(|refs| refs.positions.iter_mut().find(|p| **p == last))
        {
            *slot = index;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_index_collapses_double_slashes() {
        let mut idx = FileIndex::new();
        idx.add_paths(vec!["gfx/interface/x.dds".to_string()]);
        assert!(
            idx.contains("gfx//interface/x.dds"),
            "double-slash reference must resolve"
        );
        assert!(idx.contains("gfx/interface/x.dds"));
    }

    #[test]
    fn file_index_exact_case_flag_enforces_original_case() {
        let mut idx = FileIndex::new();
        idx.set_case_sensitive(true);
        idx.add_paths(vec!["gfx/interface/x.dds".to_string()]);
        assert!(
            !idx.contains("GFX/interface/X.dds"),
            "case mismatch must be flagged in case-sensitive mode"
        );
        assert!(idx.contains("gfx/interface/x.dds"));
    }

    #[test]
    fn file_index_reports_the_on_disk_case_of_a_mismatch() {
        let mut idx = FileIndex::new();
        idx.set_case_sensitive(true);
        idx.add_paths(vec!["gfx/interface/x.dds".to_string()]);
        assert_eq!(
            idx.on_disk_case("GFX/interface/X.dds"),
            Some("gfx/interface/x.dds")
        );
        assert_eq!(
            idx.on_disk_case("gfx/interface/x.dds"),
            None,
            "a reference that already matches has nothing to report"
        );
        assert_eq!(idx.on_disk_case("gfx/interface/missing.dds"), None);
    }

    #[test]
    fn file_index_flag_off_skips_exact_case_collection() {
        let mut idx = FileIndex::new();
        idx.add_paths(vec!["gfx/interface/x.dds".to_string()]);
        assert!(idx.files_exact.is_empty());
        assert!(idx.contains("GFX/interface/X.dds"));
    }

    #[test]
    fn file_index_exact_case_applies_to_cache_restored_paths() {
        let mut idx = FileIndex::new();
        idx.set_case_sensitive(true);
        idx.add_paths(vec!["gfx/interface/y.dds".to_string()]);
        assert!(
            !idx.contains("GFX/interface/Y.dds"),
            "cache-restored case mismatch must be flagged too"
        );
        assert!(idx.contains("gfx/interface/y.dds"));
    }

    #[test]
    fn instance_locations_finds_dotted_id_case_insensitive() {
        // Goto-definition (#39): an event/decision reference resolves by its
        // dotted id (the instance name), case-insensitively.
        let mut idx = TypeIndex::new();
        let mut map = HashMap::new();
        map.insert(
            "event".to_string(),
            vec![TypeInstance {
                name: "GER_some.1".to_string(),
                location: SourceLocation {
                    line: 7,
                    col: 4,
                    end: (7, 4),
                },
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            }],
        );
        idx.merge("file://e.txt", map);
        let locs = idx.instance_locations("ger_some.1");
        assert_eq!(locs.len(), 1, "should resolve case-insensitively");
        assert_eq!(locs[0].1.line, 7);
        assert!(idx.instance_locations("nope.1").is_empty());
    }

    #[test]
    fn keyed_instance_lookups_preserve_duplicates_case_and_file_removal() {
        let mut idx = TypeIndex::new();
        idx.merge(
            "file://first.txt",
            HashMap::from([
                (
                    "event".to_string(),
                    vec![
                        TypeInstance {
                            primary_loc_key: Some("first_title".to_string()),
                            ..inst("Shared_Event", 1)
                        },
                        TypeInstance {
                            primary_loc_key: Some("duplicate_title".to_string()),
                            ..inst("shared_event", 5)
                        },
                    ],
                ),
                (
                    "decision".to_string(),
                    vec![TypeInstance {
                        primary_loc_key: Some("decision_title".to_string()),
                        ..inst("shared_event", 2)
                    }],
                ),
            ]),
        );
        idx.merge(
            "file://second.txt",
            HashMap::from([(
                "event".to_string(),
                vec![TypeInstance {
                    primary_loc_key: Some("second_title".to_string()),
                    ..inst("SHARED_EVENT", 3)
                }],
            )]),
        );

        assert_eq!(
            idx.primary_loc_key("event", "shared_event"),
            Some("first_title")
        );
        assert_eq!(idx.primary_loc_key("EVENT", "shared_event"), None);
        let locations = idx.instance_locations("sHaReD_eVeNt");
        assert_eq!(locations.len(), 4);
        let event_lines: Vec<_> = locations
            .iter()
            .filter(|(uri, _)| uri.as_ref() == "file://first.txt")
            .map(|(_, loc)| loc.line)
            .filter(|line| *line == 1 || *line == 5)
            .collect();
        assert_eq!(event_lines, vec![1, 5], "same-type duplicates keep order");
        let mut got: Vec<_> = locations
            .iter()
            .map(|(uri, loc)| (uri.as_ref(), loc.line))
            .collect();
        got.sort_by_key(|(_, line)| *line);
        assert_eq!(
            got,
            vec![
                ("file://first.txt", 1),
                ("file://first.txt", 2),
                ("file://second.txt", 3),
                ("file://first.txt", 5),
            ]
        );

        idx.remove_file("file://first.txt");
        assert_eq!(idx.instance_locations("shared_event").len(), 1);
        assert_eq!(
            idx.primary_loc_key("event", "shared_event"),
            Some("second_title")
        );
        assert_eq!(
            idx.primary_loc_key("decision", "shared_event"),
            None,
            "removed type/name entries must not remain in the keyed view"
        );
        assert!(idx.instance_locations("missing").is_empty());
    }

    #[test]
    fn keyed_instance_lookups_remove_bulk_files_and_keep_surviving_duplicates() {
        let mut idx = TypeIndex::new();
        idx.merge(
            "file://a.txt",
            HashMap::from([(
                "event".to_string(),
                vec![
                    TypeInstance {
                        primary_loc_key: Some("a_title".into()),
                        ..inst("same", 1)
                    },
                    TypeInstance {
                        primary_loc_key: Some("a_second_title".into()),
                        ..inst("same", 2)
                    },
                ],
            )]),
        );
        idx.merge_base_game_with_uris([(
            "event".to_string(),
            vec![(Arc::<str>::from("file://b.txt"), inst("SAME", 3))],
        )]);
        idx.merge(
            "file://c.txt",
            HashMap::from([("event".to_string(), vec![inst("same", 4)])]),
        );

        let removed = HashSet::from([
            Arc::<str>::from("file://a.txt"),
            Arc::<str>::from("file://b.txt"),
        ]);
        idx.remove_files(&removed);

        let locations = idx.instance_locations("SAME");
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].0.as_ref(), "file://c.txt");
        assert_eq!(locations[0].1.line, 4);
        assert_eq!(idx.primary_loc_key("event", "same"), None);
    }

    #[test]
    fn keyed_primary_lookup_skips_missing_keys_and_indexes_base_game_uris() {
        let mut idx = TypeIndex::new();
        idx.merge_base_game_with_uris([(
            "event".to_string(),
            vec![(Arc::<str>::from("file://vanilla.txt"), inst("shared", 1))],
        )]);
        idx.merge(
            "file://mod.txt",
            HashMap::from([(
                "event".to_string(),
                vec![TypeInstance {
                    primary_loc_key: Some("mod_title".to_string()),
                    ..inst("SHARED", 2)
                }],
            )]),
        );

        assert_eq!(
            idx.primary_loc_key("event", "shared"),
            Some("mod_title"),
            "a preceding instance without a primary key must not hide a later key"
        );
        assert_eq!(idx.instance_locations("shared").len(), 2);

        idx.remove_file("file://vanilla.txt");
        assert_eq!(idx.instance_locations("shared").len(), 1);
        assert_eq!(idx.primary_loc_key("event", "shared"), Some("mod_title"));
    }

    #[test]
    fn primary_lookup_tracks_the_last_instance_moved_by_file_removal() {
        let mut idx = TypeIndex::new();
        idx.merge(
            "file://a.txt",
            HashMap::from([("event".to_string(), vec![inst("shared", 1)])]),
        );
        idx.merge(
            "file://b.txt",
            HashMap::from([(
                "event".to_string(),
                vec![TypeInstance {
                    primary_loc_key: Some("b_title".into()),
                    ..inst("shared", 2)
                }],
            )]),
        );
        idx.merge(
            "file://c.txt",
            HashMap::from([(
                "event".to_string(),
                vec![TypeInstance {
                    primary_loc_key: Some("c_title".into()),
                    ..inst("shared", 3)
                }],
            )]),
        );
        assert_eq!(idx.primary_loc_key("event", "shared"), Some("b_title"));

        idx.remove_file("file://a.txt");
        assert_eq!(
            idx.primary_loc_key("event", "shared"),
            Some("c_title"),
            "swap_remove moves the last entry ahead of intervening matches"
        );
        assert_eq!(
            idx.instance_locations("shared")
                .iter()
                .map(|(_, location)| location.line)
                .collect::<Vec<_>>(),
            vec![3, 2]
        );
    }

    #[test]
    fn instances_in_file_only_this_files_entries_excludes_subtype_keys() {
        let mut idx = TypeIndex::new();
        idx.merge(
            "file://a.txt",
            HashMap::from([
                ("event".to_string(), vec![inst("a_ev", 1)]),
                ("tech".to_string(), vec![inst("a_tech", 2), inst("a_ev", 1)]),
            ]),
        );
        idx.merge_base_game_with_uris(vec![
            (
                "event".to_string(),
                vec![(Arc::<str>::from("file://b.txt"), inst("b_ev", 3))],
            ),
            (
                "event.subt".to_string(),
                vec![(Arc::<str>::from("file://a.txt"), inst("a_ev", 1))],
            ),
        ]);

        let mut got: Vec<(&str, &str)> = idx
            .instances_in_file("file://a.txt")
            .into_iter()
            .map(|(ty, i)| (ty, i.name.as_str()))
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![("event", "a_ev"), ("tech", "a_ev"), ("tech", "a_tech")]
        );
        let location = SourceLocation {
            line: 1,
            col: 0,
            end: (1, 0),
        };
        let mut type_names =
            idx.instance_type_names_in_file("file://a.txt", "event", "a_ev", location);
        type_names.sort();
        assert_eq!(type_names, vec!["event", "event.subt"]);
        assert_eq!(
            idx.instance_type_names_in_file("file://a.txt", "tech", "a_ev", location),
            vec!["tech"]
        );

        assert!(idx.instances_in_file("file://never.txt").is_empty());
    }

    #[test]
    fn instance_at_uses_the_requested_file_type_and_source_position() {
        let mut idx = TypeIndex::new();
        idx.merge(
            "file://a.txt",
            HashMap::from([("event".to_string(), vec![inst("a_ev", 1), inst("b_ev", 2)])]),
        );
        idx.merge(
            "file://b.txt",
            HashMap::from([("event".to_string(), vec![inst("a_ev", 1)])]),
        );

        assert_eq!(
            idx.instance_at("file://a.txt", "event", "b_ev", 2, 0)
                .map(|instance| instance.name.as_str()),
            Some("b_ev")
        );
        assert!(
            idx.instance_at("file://a.txt", "event", "b_ev", 1, 0)
                .is_none()
        );
        assert!(
            idx.instance_at("file://b.txt", "event", "b_ev", 2, 0)
                .is_none()
        );
        assert!(
            idx.instance_at("file://a.txt", "other", "b_ev", 2, 0)
                .is_none()
        );
    }

    #[test]
    fn loc_bindable_names_includes_instances_and_variables() {
        let mut idx = TypeIndex::new();
        let mut per_type: HashMap<String, Vec<TypeInstance>> = HashMap::new();
        per_type.insert(
            "ln".to_string(),
            vec![TypeInstance {
                name: "Education_Dynamic_Modifier".to_string(),
                location: SourceLocation {
                    line: 1,
                    col: 0,
                    end: (1, 0),
                },
                primary_loc_key: None,
                required_loc_keys: Vec::new(),
            }],
        );
        idx.merge("common/lns/x.txt", per_type);
        idx.var_index.add_name("My_Variable");

        let names: std::collections::HashSet<String> = idx.loc_bindable_names().collect();
        assert!(
            names.contains("education_dynamic_modifier"),
            "instance names (lowercased) must be bindable, got {:?}",
            names
        );
        assert!(
            names.contains("my_variable"),
            "defined variables (lowercased) must be bindable, got {:?}",
            names
        );
    }

    fn inst(name: &str, line: u32) -> TypeInstance {
        TypeInstance {
            name: name.to_string(),
            location: SourceLocation {
                line,
                col: 0,
                end: (line, 0),
            },
            primary_loc_key: None,
            required_loc_keys: Vec::new(),
        }
    }

    #[test]
    fn name_changes_track_distinct_membership_transitions() {
        let mut idx = TypeIndex::new();
        idx.merge(
            "a.txt",
            HashMap::from([(
                "effect".to_string(),
                vec![inst("Alpha", 0), inst("ALPHA", 1), inst("Beta", 2)],
            )]),
        );
        let added = idx.take_instance_name_changes();
        assert_eq!(added.len(), 2, "case-only duplicate is not a new name");

        idx.merge(
            "b.txt",
            HashMap::from([("effect".to_string(), vec![inst("alpha", 0)])]),
        );
        assert!(idx.take_instance_name_changes().is_empty());
        idx.remove_file("a.txt");
        let removed_beta = idx.take_instance_name_changes();
        assert_eq!(removed_beta.len(), 1);
        assert_eq!(removed_beta[0].0, "effect");
        assert_eq!(removed_beta[0].1.as_ref(), "beta");
        assert!(idx.contains("effect", "alpha"));

        idx.remove_file("b.txt");
        assert_eq!(idx.take_instance_name_changes().len(), 1);
        assert!(!idx.contains("effect", "alpha"));
    }

    #[test]
    fn lowercase_name_keys_share_one_allocation() {
        let mut idx = TypeIndex::new();
        idx.merge(
            "file://event.txt",
            HashMap::from([("event".to_string(), vec![inst("Shared_Event", 1)])]),
        );

        let name = idx.name_counts.keys().next().unwrap();
        let set_name = idx
            .instance_sets
            .get("event")
            .unwrap()
            .keys()
            .next()
            .unwrap();
        assert!(Arc::ptr_eq(name, set_name));
    }

    type Snap = (
        std::collections::BTreeMap<String, Vec<(String, String, u32)>>,
        std::collections::BTreeMap<String, usize>,
        std::collections::BTreeMap<String, std::collections::BTreeMap<String, (usize, usize)>>,
    );

    fn snapshot(idx: &TypeIndex) -> Snap {
        use std::collections::BTreeMap;
        let mut map = BTreeMap::new();
        for (ty, entries) in &idx.map {
            let mut v: Vec<(String, String, u32)> = entries
                .iter()
                .map(|(uri, i)| (uri.to_string(), i.name.clone(), i.location.line))
                .collect();
            v.sort();
            map.insert(ty.clone(), v);
        }
        let name_counts: BTreeMap<String, usize> = idx
            .name_counts
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect();
        let instance_sets: BTreeMap<String, BTreeMap<String, (usize, usize)>> = idx
            .instance_sets
            .iter()
            .map(|(k, m)| {
                (
                    k.clone(),
                    m.iter()
                        .map(|(kk, vv)| (kk.to_string(), (vv.total, vv.workspace)))
                        .collect(),
                )
            })
            .collect();
        // Every name's positions must point at exactly its own instances.
        for (ty, names) in &idx.instance_sets {
            for (name, refs) in names {
                let mut got = refs.positions.to_vec();
                got.sort_unstable();
                let want: Vec<usize> = idx
                    .map
                    .get(ty)
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .filter(|(_, (_, inst))| inst.name.to_ascii_lowercase() == **name)
                    .map(|(i, _)| i)
                    .collect();
                assert_eq!(got, want, "positions of {ty}/{name}");
            }
        }
        (map, name_counts, instance_sets)
    }

    #[test]
    fn remove_file_parity_removing_merge_file() {
        let build = |include_b: bool| -> TypeIndex {
            let mut idx = TypeIndex::new();
            idx.merge(
                "file://a.txt",
                HashMap::from([
                    (
                        "event".to_string(),
                        vec![inst("shared_ev", 1), inst("a_only", 2)],
                    ),
                    ("tech".to_string(), vec![inst("a_tech", 3)]),
                ]),
            );
            if include_b {
                idx.merge(
                    "file://b.txt",
                    HashMap::from([("event".to_string(), vec![inst("shared_ev", 5)])]),
                );
            }
            idx.merge_base_game_with_uris(vec![
                (
                    "event".to_string(),
                    vec![
                        (Arc::<str>::from("file://c.txt"), inst("shared_ev", 7)),
                        (Arc::<str>::from("file://d.txt"), inst("d_ev", 8)),
                    ],
                ),
                (
                    "event.subt".to_string(),
                    vec![(Arc::<str>::from("file://c.txt"), inst("shared_ev", 7))],
                ),
            ]);
            idx
        };

        let mut full = build(true);
        full.remove_file("file://b.txt");
        assert_eq!(snapshot(&full), snapshot(&build(false)));
        assert!(full.contains("event", "shared_ev"));
        assert!(full.is_any_instance("shared_ev"));
    }

    #[test]
    fn remove_file_parity_removing_merge_base_game_with_uris_file() {
        let build = |include_c: bool| -> TypeIndex {
            let mut idx = TypeIndex::new();
            idx.merge(
                "file://a.txt",
                HashMap::from([("event".to_string(), vec![inst("shared_ev", 1)])]),
            );
            let mut batch = vec![(
                "event".to_string(),
                vec![(Arc::<str>::from("file://d.txt"), inst("d_ev", 8))],
            )];
            if include_c {
                batch.push((
                    "event".to_string(),
                    vec![(Arc::<str>::from("file://c.txt"), inst("shared_ev", 7))],
                ));
                batch.push((
                    "event.subt".to_string(),
                    vec![(Arc::<str>::from("file://c.txt"), inst("shared_ev", 7))],
                ));
            }
            idx.merge_base_game_with_uris(batch);
            idx
        };

        let mut full = build(true);
        full.remove_file("file://c.txt");
        assert_eq!(snapshot(&full), snapshot(&build(false)));
        assert!(!full.contains("event.subt", "shared_ev"));
        assert!(full.contains("event", "shared_ev")); // still via a
    }

    #[test]
    fn merge_remove_remerge_cycles_stay_clean() {
        let mut idx = TypeIndex::new();
        let payload = || HashMap::from([("event".to_string(), vec![inst("ev", 1)])]);
        for _ in 0..3 {
            idx.merge("file://x.txt", payload());
            assert!(idx.contains("event", "ev"));
            idx.remove_file("file://x.txt");
            assert!(!idx.contains("event", "ev"));
            assert!(!idx.is_any_instance("ev"));
            assert!(idx.instances("event").is_empty());
            assert!(
                !idx.map.contains_key("event"),
                "empty bucket must be pruned"
            );
        }
        assert!(idx.map.is_empty());
        assert!(idx.name_counts.is_empty());
        assert!(idx.instance_sets.is_empty());
    }

    #[test]
    fn remove_files_bulk_then_remove_file_singular() {
        let mut idx = TypeIndex::new();
        idx.merge_base_game_with_uris(vec![(
            "event".to_string(),
            vec![
                (Arc::<str>::from("v1"), inst("e1", 1)),
                (Arc::<str>::from("v2"), inst("e2", 2)),
            ],
        )]);
        idx.merge(
            "m.txt",
            HashMap::from([("event".to_string(), vec![inst("me", 3)])]),
        );

        let mut bulk = HashSet::new();
        bulk.insert(Arc::<str>::from("v1"));
        bulk.insert(Arc::<str>::from("v2"));
        idx.remove_files(&bulk);
        assert!(!idx.contains("event", "e1"));
        assert!(!idx.contains("event", "e2"));
        assert!(idx.contains("event", "me"));

        idx.remove_file("m.txt");
        assert!(!idx.contains("event", "me"));
        assert!(idx.map.is_empty());
        assert!(idx.name_counts.is_empty());
        assert!(idx.instance_sets.is_empty());
    }

    #[test]
    fn remove_file_with_no_entries_is_noop() {
        let mut idx = TypeIndex::new();
        idx.merge(
            "file://a.txt",
            HashMap::from([("event".to_string(), vec![inst("ev", 1)])]),
        );
        let before = snapshot(&idx);
        idx.remove_file("file://never-merged.txt");
        assert_eq!(before, snapshot(&idx));
        assert!(idx.contains("event", "ev"));
    }

    #[test]
    fn removing_early_file_relocates_and_repairs_a_later_files_positions() {
        let build = |skip: &[usize]| -> TypeIndex {
            let mut idx = TypeIndex::new();
            for f in 0..20 {
                if skip.contains(&f) {
                    continue;
                }
                let uri = format!("file://f{f}.txt");
                let instances = (0..3).map(|n| inst(&format!("f{f}_s{n}"), n)).collect();
                idx.merge(&uri, HashMap::from([("state".to_string(), instances)]));
            }
            idx
        };
        let assert_survivors_intact = |idx: &TypeIndex, removed: &[usize]| {
            for f in (0..20).filter(|f| !removed.contains(f)) {
                let uri = format!("file://f{f}.txt");
                let mut got: Vec<String> = idx
                    .instances_in_file(&uri)
                    .into_iter()
                    .map(|(_, i)| i.name.clone())
                    .collect();
                got.sort();
                let mut want = vec![format!("f{f}_s0"), format!("f{f}_s1"), format!("f{f}_s2")];
                want.sort();
                assert_eq!(
                    got, want,
                    "file {uri} lost or gained instances after removing {removed:?}"
                );
            }
            for f in removed {
                assert!(
                    idx.instances_in_file(&format!("file://f{f}.txt"))
                        .is_empty()
                );
            }
        };

        let mut idx = build(&[]);
        idx.remove_file("file://f5.txt");
        assert_eq!(snapshot(&idx), snapshot(&build(&[5])));
        assert_survivors_intact(&idx, &[5]);

        idx.remove_file("file://f19.txt");
        assert_eq!(snapshot(&idx), snapshot(&build(&[5, 19])));
        assert_survivors_intact(&idx, &[5, 19]);
    }

    #[test]
    fn subtype_key_removal_preserves_name_counts_exemption() {
        let mut idx = TypeIndex::new();
        idx.merge_base_game_with_uris(vec![
            (
                "event".to_string(),
                vec![(Arc::<str>::from("f.txt"), inst("ev", 1))],
            ),
            (
                "event.subt".to_string(),
                vec![(Arc::<str>::from("f.txt"), inst("ev", 1))],
            ),
        ]);
        assert_eq!(idx.name_counts.get("ev").copied(), Some(1));
        assert!(idx.contains("event.subt", "ev"));
        idx.remove_file("f.txt");
        assert!(!idx.name_counts.contains_key("ev"));
        assert!(idx.map.is_empty());
        assert!(idx.instance_sets.is_empty());
    }

    #[test]
    fn file_index_resolves_reference_relative_to_asset_dir() {
        let mut fi = FileIndex::new();
        fi.add_paths([
            "sound/zom/zom_vo.asset".to_string(),
            "sound/zom/zom_idle_001.wav".to_string(),
        ]);

        assert!(
            fi.resolve_relative(
                "/home/user/Millennium-Dawn/sound/zom/zom_vo.asset",
                "zom_idle_001.wav"
            ),
            "a sibling beside the .asset should resolve"
        );
        assert!(
            !fi.resolve_relative(
                "/home/user/Millennium-Dawn/sound/zom/zom_vo.asset",
                "ku_move_007.wav"
            ),
            "a genuinely-missing sibling must not resolve"
        );
    }

    // ── ScriptedLocIndex (#348) ───────────────────────────────────────────────

    fn scripted_loc_index_with(file: &str, names: &[&str]) -> ScriptedLocIndex {
        let mut idx = ScriptedLocIndex::default();
        idx.merge_file(file, names.iter().map(|s| s.to_string()).collect());
        idx
    }

    #[test]
    fn scripted_loc_lookup_is_case_insensitive() {
        let idx = scripted_loc_index_with("a.txt", &["AST_GetNavyName"]);
        assert!(idx.contains("ast_getnavyname"));
        assert!(idx.contains("AST_GETNAVYNAME"));
        assert!(!idx.contains("AST_Typo"));
    }

    #[test]
    fn scripted_loc_empty_until_a_name_lands() {
        let mut idx = ScriptedLocIndex::default();
        assert!(idx.is_empty(), "no names means the check has no data");
        idx.merge_file("a.txt", vec!["Foo".into()]);
        assert!(!idx.is_empty());
    }

    #[test]
    fn scripted_loc_remove_file_drops_only_that_files_names() {
        let mut idx = scripted_loc_index_with("a.txt", &["Shared", "OnlyA"]);
        idx.merge_file("b.txt", vec!["Shared".into()]);
        idx.remove_file("a.txt");
        assert!(!idx.contains("OnlyA"), "a.txt's own name goes");
        assert!(idx.contains("Shared"), "b.txt still defines it");
        idx.remove_file("b.txt");
        assert!(idx.is_empty());
    }

    #[test]
    fn scripted_loc_reindex_replaces_a_files_names() {
        let mut idx = scripted_loc_index_with("a.txt", &["Old"]);
        idx.merge_file("a.txt", vec!["New".into()]);
        assert!(!idx.contains("Old"), "the previous set must not leak");
        assert!(idx.contains("New"));
    }

    #[test]
    fn scripted_loc_vanilla_survives_a_mod_file_clear() {
        let mut idx = scripted_loc_index_with("a.txt", &["Shared"]);
        idx.set_vanilla_names(&["Shared".into(), "VanillaOnly".into()]);
        idx.remove_file("a.txt");
        assert!(idx.contains("Shared"), "the base-game definition stays");
        assert!(idx.contains("VanillaOnly"));
    }

    #[test]
    fn scripted_loc_vanilla_remerge_replaces_not_accumulates() {
        let mut idx = ScriptedLocIndex::default();
        idx.set_vanilla_names(&["First".into()]);
        idx.set_vanilla_names(&["Second".into()]);
        assert!(!idx.contains("First"));
        assert!(idx.contains("Second"));
    }

    #[test]
    fn scripted_loc_names_are_the_workspace_half_only() {
        let mut idx = scripted_loc_index_with("a.txt", &["ModOne"]);
        idx.set_vanilla_names(&["VanillaOne".into()]);
        let names: Vec<&str> = idx.names().collect();
        assert_eq!(names, vec!["modone"], "the cache stores vanilla separately");
    }

    #[test]
    fn type_index_remove_file_drops_scripted_locs() {
        let mut idx = TypeIndex::new();
        idx.scripted_loc_index
            .merge_file("a.txt", vec!["Foo".into()]);
        idx.remove_file("a.txt");
        assert!(
            !idx.scripted_loc_index.contains("Foo"),
            "the LSP's clear_file path routes through remove_file"
        );
    }

    #[test]
    fn scripted_gui_lookup_is_case_insensitive() {
        let mut idx = ScriptedGuiIndex::default();
        idx.merge_file("a.txt", vec!["Topbar_Icon_Click".into()]);
        assert!(idx.contains("topbar_icon_click"));
        assert!(idx.contains("TOPBAR_ICON_CLICK"));
    }

    #[test]
    fn scripted_gui_reindex_and_removal_are_per_file() {
        let mut idx = ScriptedGuiIndex::default();
        idx.merge_file("a.txt", vec!["Old".into(), "Shared".into()]);
        idx.merge_file("b.txt", vec!["Shared".into()]);
        idx.merge_file("a.txt", vec!["New".into()]);
        assert!(!idx.contains("Old"));
        assert!(idx.contains("New"));
        assert!(idx.contains("Shared"));
        idx.remove_file("b.txt");
        assert!(!idx.contains("Shared"));
    }

    #[test]
    fn scripted_gui_vanilla_survives_workspace_removal() {
        let mut idx = ScriptedGuiIndex::default();
        idx.merge_file("a.txt", vec!["Shared".into()]);
        idx.set_vanilla_names(&["Shared".into(), "VanillaOnly".into()]);
        idx.remove_file("a.txt");
        assert!(idx.contains("Shared"));
        assert!(idx.contains("VanillaOnly"));
        assert_eq!(idx.names().count(), 0, "cache export excludes vanilla");
    }

    #[test]
    fn type_index_remove_file_drops_scripted_gui_callbacks() {
        let mut idx = TypeIndex::new();
        idx.scripted_gui_index
            .merge_file("a.txt", vec!["callback".into()]);
        idx.remove_file("a.txt");
        assert!(idx.scripted_gui_index.is_empty());
    }

    // ── VarIndex vanilla provenance (#306) ────────────────────────────────────

    #[test]
    fn var_index_vanilla_set_makes_contains_true() {
        let mut vi = VarIndex::new();
        vi.set_vanilla_names(&["vanilla_var".to_string()]);
        assert!(vi.contains("vanilla_var"));
    }

    #[test]
    fn var_index_vanilla_set_case_insensitive() {
        let mut vi = VarIndex::new();
        vi.set_vanilla_names(&["vanilla_var".to_string()]);
        assert!(vi.contains("VANILLA_VAR"));
    }

    #[test]
    fn var_index_vanilla_set_is_empty_and_len() {
        let mut vi = VarIndex::new();
        assert!(vi.is_empty());
        vi.set_vanilla_names(&["vanilla_var".to_string()]);
        assert!(!vi.is_empty());
        assert_eq!(vi.len(), 1);
    }

    #[test]
    fn var_index_vanilla_quoted_trim_normalized() {
        let mut vi = VarIndex::new();
        vi.set_vanilla_names(&["\"quoted_var\"".to_string()]);
        assert!(vi.contains("quoted_var"));
    }

    #[test]
    fn var_index_vanilla_dot_last_segment() {
        let mut vi = VarIndex::new();
        vi.set_vanilla_names(&["foo.bar^2".to_string()]);
        assert!(vi.contains("bar"));
    }

    #[test]
    fn var_index_vanilla_suffix_stripping() {
        let mut vi = VarIndex::new();
        vi.set_vanilla_names(&["my_var?100".to_string(), "my_var@GER".to_string()]);
        assert!(vi.contains("my_var"));
        assert_eq!(vi.len(), 1, "? and @ suffixes collapse to same key");
    }

    #[test]
    fn var_index_vanilla_empty_and_whitespace_skipped() {
        let mut vi = VarIndex::new();
        vi.set_vanilla_names(&["   ".to_string(), "".to_string()]);
        assert!(vi.is_empty());
        assert_eq!(vi.len(), 0);
    }

    #[test]
    fn var_index_vanilla_len_union_does_not_double_count_shared() {
        let mut vi = VarIndex::new();
        vi.add_name("shared_var");
        vi.set_vanilla_names(&["shared_var".to_string(), "vanilla_only".to_string()]);
        assert!(vi.contains("shared_var"));
        assert!(vi.contains("vanilla_only"));
        assert_eq!(vi.len(), 2, "shared name counts once");
        assert!(!vi.is_empty());
    }

    #[test]
    fn var_index_vanilla_loc_bindable_includes_vanilla() {
        let mut idx = TypeIndex::new();
        idx.var_index
            .set_vanilla_names(&["vanilla_loc_var".to_string()]);
        let names: HashSet<String> = idx.loc_bindable_names().collect();
        assert!(
            names.contains("vanilla_loc_var"),
            "vanilla vars must be loc-bindable, got {:?}",
            names
        );
        idx.var_index.add_name("mod_var");
        let names2: HashSet<String> = idx.loc_bindable_names().collect();
        assert!(names2.contains("mod_var"));
        assert!(names2.contains("vanilla_loc_var"));
    }

    #[test]
    fn var_index_vanilla_survives_clear_file_on_shared_name() {
        let mut vi = VarIndex::new();
        vi.add_name("shared_var");
        vi.set_vanilla_names(&["shared_var".to_string()]);
        vi.remove_name("shared_var");
        assert!(
            vi.contains("shared_var"),
            "vanilla must survive a mod clear_file on same name"
        );
        assert_eq!(vi.len(), 1);
    }

    #[test]
    fn var_index_vanilla_remerge_replaces_not_accumulates() {
        let mut vi = VarIndex::new();
        vi.set_vanilla_names(&["old_var".to_string()]);
        assert!(vi.contains("old_var"));
        vi.set_vanilla_names(&["new_var".to_string()]);
        assert!(
            !vi.contains("old_var"),
            "re-merge must replace, not accumulate"
        );
        assert!(vi.contains("new_var"));
        assert_eq!(vi.len(), 1);
    }

    #[test]
    fn var_index_vanilla_clear_drops_vanilla_retains_mod() {
        let mut vi = VarIndex::new();
        vi.add_name("mod_var");
        vi.set_vanilla_names(&["vanilla_var".to_string()]);
        vi.clear_vanilla_names();
        assert!(!vi.contains("vanilla_var"));
        assert!(vi.contains("mod_var"));
        assert!(!vi.is_empty());
    }

    #[test]
    fn var_index_vanilla_second_clear_idempotent() {
        let mut vi = VarIndex::new();
        vi.add_name("mod_var");
        vi.set_vanilla_names(&["vanilla_var".to_string()]);
        vi.clear_vanilla_names();
        vi.clear_vanilla_names();
        assert!(vi.contains("mod_var"));
        assert!(!vi.contains("vanilla_var"));
    }

    #[test]
    fn var_index_vanilla_empty_vector_clears_previous() {
        let mut vi = VarIndex::new();
        vi.set_vanilla_names(&["a".to_string()]);
        vi.set_vanilla_names(&[]);
        assert!(!vi.contains("a"));
        assert!(vi.is_empty());
    }

    #[test]
    fn var_index_merge_does_not_copy_vanilla_names() {
        let mut vanilla = VarIndex::new();
        vanilla.set_vanilla_names(&["vanilla_only".to_string()]);
        let mut mod_idx = VarIndex::new();
        mod_idx.merge(&vanilla);
        assert!(
            !mod_idx.contains("vanilla_only"),
            "merge copies only mod names, vanilla stays separate"
        );
    }

    #[test]
    fn var_index_all_names_dedups_shared() {
        let mut vi = VarIndex::new();
        vi.add_name("shared");
        vi.set_vanilla_names(&["shared".to_string(), "vanilla".to_string()]);
        assert_eq!(vi.all_names().count(), vi.len());
        assert_eq!(vi.len(), 2);
    }

    #[test]
    fn var_index_all_names_count_equals_len_when_shared_cross_form() {
        let mut vi = VarIndex::new();
        vi.add_name("SHARED_VAR");
        vi.set_vanilla_names(&[
            "shared_var".to_string(),
            "My_Var@GER".to_string(),
            "foo.bar".to_string(),
        ]);
        vi.add_name("my_var");
        vi.add_name("bar");
        assert_eq!(vi.all_names().count(), vi.len());
        assert_eq!(vi.len(), 3);
    }

    #[test]
    fn vanilla_names_iter_returns_staged() {
        let mut vi = VarIndex::new();
        vi.set_vanilla_names(&["b".to_string(), "a".to_string()]);
        let mut v: Vec<_> = vi.vanilla_names_iter().cloned().collect();
        v.sort();
        assert_eq!(v, vec!["a", "b"]);
    }

    #[test]
    fn var_index_add_name_quoted_stripped() {
        let mut vi = VarIndex::new();
        vi.add_name("\"My_Var\"");
        assert!(vi.contains("my_var"));
    }

    #[test]
    fn var_index_add_name_at_suffix_stripped() {
        let mut vi = VarIndex::new();
        vi.add_name("My_Var@GER");
        assert!(vi.contains("my_var"));
    }

    #[test]
    fn var_index_add_name_dot_and_selectors_stripped() {
        let mut vi = VarIndex::new();
        vi.add_name("foo.bar?100");
        assert!(vi.contains("bar"));
        vi.add_name("baz^2");
        assert!(vi.contains("baz"));
    }

    #[test]
    fn var_index_add_name_whitespace_skipped() {
        let mut vi = VarIndex::new();
        vi.add_name("\"My_Var\"");
        vi.add_name("My_Var@GER");
        vi.add_name("foo.bar?100");
        assert_eq!(vi.len(), 2);
        vi.add_name("   ");
        assert_eq!(vi.len(), 2);
    }

    #[test]
    fn clone_file_index_is_independent() {
        let mut idx = TypeIndex::new();
        idx.file_index.insert("gfx/a.dds");
        let snap = idx.clone();
        let mut mutated = snap.clone();
        mutated.file_index.insert("gfx/b.dds");
        assert!(!idx.file_index.contains("gfx/b.dds"));
        assert!(mutated.file_index.contains("gfx/b.dds"));
    }

    #[test]
    fn clone_file_index_exact_case_is_independent() {
        let mut idx = TypeIndex::new();
        idx.file_index.set_case_sensitive(true);
        idx.file_index.insert("gfx/a.dds");
        let snap = idx.clone();
        let mut mutated = snap.clone();
        mutated.file_index.insert("gfx/B.dds");
        assert!(!idx.file_index.contains("gfx/B.dds"));
        assert!(mutated.file_index.contains("gfx/B.dds"));
        assert_eq!(idx.file_index.on_disk_case("gfx/a.dds"), None);
        assert!(snap.file_index.contains("gfx/a.dds"));
    }

    #[test]
    fn clone_var_index_is_independent() {
        let mut idx = TypeIndex::new();
        idx.var_index.add_name("My_Var");
        let snap = idx.clone();
        let mut mutated = snap.clone();
        mutated.var_index.add_name("Other_Var");
        assert!(!idx.var_index.contains("other_var"));
        assert!(mutated.var_index.contains("other_var"));
    }

    #[test]
    fn clone_var_index_vanilla_is_independent() {
        let mut idx = TypeIndex::new();
        idx.var_index
            .set_vanilla_names(&["Vanilla_Var".to_string()]);
        let snap = idx.clone();
        let mut mutated = snap.clone();
        mutated
            .var_index
            .set_vanilla_names(&["Other_Vanilla".to_string()]);
        assert!(mutated.var_index.contains("other_vanilla"));
        assert!(snap.var_index.contains("vanilla_var"));
        assert!(!snap.var_index.contains("other_vanilla"));
        assert!(!idx.var_index.contains("other_vanilla"));
    }

    #[test]
    fn clone_remove_file_is_independent() {
        let mut idx = TypeIndex::new();
        idx.merge(
            "file://a.txt",
            HashMap::from([("event".to_string(), vec![inst("ev_a", 1)])]),
        );
        idx.merge(
            "file://b.txt",
            HashMap::from([("event".to_string(), vec![inst("ev_b", 2)])]),
        );
        let snap = idx.clone();
        let mut mutated = snap.clone();
        mutated.merge(
            "file://c.txt",
            HashMap::from([("event".to_string(), vec![inst("ev_c", 3)])]),
        );
        assert!(!idx.contains("event", "ev_c"));
        assert!(mutated.contains("event", "ev_c"));
        let snap2 = idx.clone();
        idx.remove_file("file://a.txt");
        assert!(!idx.contains("event", "ev_a"));
        assert!(snap2.contains("event", "ev_a"));
    }

    /// Scan pass 2 validates against a clone of the live index. A clone that dropped the
    /// dynamic-value indexes would leave every unit test green while silently moving scan
    /// diagnostics (the #332 trap). Pin that the snapshot still answers membership.
    #[test]
    fn clone_carries_dynamic_value_indexes() {
        let mut idx = TypeIndex::new();
        idx.value_set_values.merge_file(
            "file://a.txt",
            HashMap::from([("character_token".to_string(), vec!["tok_a".to_string()])]),
        );
        idx.complex_enum_values.merge_file(
            "file://a.txt",
            HashMap::from([(
                "equipment_stat".to_string(),
                vec!["build_cost_ic".to_string()],
            )]),
        );
        let snap = idx.clone();
        assert!(snap.value_set_values.contains("character_token", "tok_a"));
        assert!(
            snap.complex_enum_values
                .contains("equipment_stat", "build_cost_ic")
        );
    }

    #[test]
    fn clone_dynamic_value_indexes_are_independent() {
        let mut idx = TypeIndex::new();
        idx.value_set_values.merge_file(
            "file://a.txt",
            HashMap::from([("character_token".to_string(), vec!["tok_a".to_string()])]),
        );
        let snap = idx.clone();
        let mut mutated = snap.clone();
        mutated.value_set_values.merge_file(
            "file://b.txt",
            HashMap::from([("character_token".to_string(), vec!["tok_b".to_string()])]),
        );
        assert!(!idx.value_set_values.contains("character_token", "tok_b"));
        assert!(
            mutated
                .value_set_values
                .contains("character_token", "tok_b")
        );
        idx.value_set_values.remove_file("file://a.txt");
        assert!(snap.value_set_values.contains("character_token", "tok_a"));
    }

    /// A clone must keep answering with the view it was taken from. `remove_file`
    /// compacts the shared type vec, so a clone that aliased `map` or `file_positions`
    /// would read a surviving file's positions against a compacted vec (#328).
    #[test]
    fn clone_instances_in_file_is_a_snapshot_of_the_live_index() {
        let mut idx = TypeIndex::new();
        for f in 0..4 {
            let instances = (0..3).map(|n| inst(&format!("f{f}_s{n}"), n)).collect();
            idx.merge(
                &format!("file://f{f}.txt"),
                HashMap::from([("state".to_string(), instances)]),
            );
        }
        let snap = idx.clone();
        let names = |idx: &TypeIndex, uri: &str| -> Vec<String> {
            let mut got: Vec<String> = idx
                .instances_in_file(uri)
                .into_iter()
                .map(|(_, i)| i.name.clone())
                .collect();
            got.sort();
            got
        };

        idx.remove_file("file://f0.txt");
        idx.merge(
            "file://f9.txt",
            HashMap::from([("state".to_string(), vec![inst("f9_s0", 0)])]),
        );

        for f in 0..4 {
            assert_eq!(
                names(&snap, &format!("file://f{f}.txt")),
                vec![format!("f{f}_s0"), format!("f{f}_s1"), format!("f{f}_s2")],
                "snapshot answered for f{f} from the live index"
            );
        }
        assert!(snap.instances_in_file("file://f9.txt").is_empty());
        assert!(names(&idx, "file://f0.txt").is_empty());
    }

    #[test]
    fn clone_preserves_replacement_coalescing_and_shares_the_alias_journal() {
        let mut idx = TypeIndex::new();
        idx.merge(
            "file://a.txt",
            HashMap::from([("state".to_string(), vec![inst("kept", 1)])]),
        );
        idx.take_instance_name_changes();
        idx.begin_file_replacement("file://a.txt");
        idx.remove_file("file://a.txt");

        let mut cloned = idx.clone();
        assert!(Arc::ptr_eq(
            &idx.instance_name_changes,
            &cloned.instance_name_changes,
        ));
        cloned.merge(
            "file://a.txt",
            HashMap::from([("state".to_string(), vec![inst("kept", 5)])]),
        );
        cloned.finish_file_replacement("file://a.txt");
        assert!(!cloned.has_instance_name_changes());
        assert_eq!(cloned.instance_locations("kept")[0].1.line, 5);
        assert!(idx.instance_locations("kept").is_empty());

        cloned.remove_file("file://a.txt");
        assert_eq!(
            idx.take_instance_name_changes(),
            vec![("state".to_string(), Arc::<str>::from("kept"))],
        );
        assert!(!cloned.has_instance_name_changes());
    }

    /// A release drops a workspace-only key and keeps one a lower layer also
    /// provides, whether the workspace claimed it before or after that layer (#1067).
    #[test]
    fn file_index_release_keeps_lower_layer_keys() {
        let mut idx = FileIndex::new();
        assert!(idx.claim_workspace("GFX/Solo.dds"));
        assert!(idx.claim_workspace("gfx/shared.dds"));
        idx.add_paths(["gfx/Shared.DDS".to_string(), "gfx/base.dds".to_string()]);

        assert!(idx.release_workspace("gfx/solo.DDS"));
        assert!(!idx.contains("gfx/solo.dds"));
        assert!(!idx.release_workspace("gfx/shared.dds"));
        assert!(idx.contains("gfx/shared.dds"));
        assert!(!idx.release_workspace("gfx/base.dds"), "never claimed");

        assert!(!idx.claim_workspace("gfx/Base.dds"), "no new key");
        assert!(idx.is_workspace_owned("gfx/base.dds"));
        assert!(!idx.release_workspace("gfx/base.dds"));
        assert!(idx.contains("gfx/base.dds"));
        assert!(!idx.is_workspace_owned("gfx/base.dds"));
    }

    /// A folder delete reaches every workspace key below it, at any depth, on whole
    /// path segments, and never a key a lower layer alone provides (#1092).
    #[test]
    fn file_index_workspace_owned_under_matches_whole_segments() {
        let mut idx = FileIndex::new();
        for key in [
            "gfx/Pack/a.dds",
            "gfx/pack/deep/er/b.dds",
            "gfx/packed/c.dds",
            "gfx/pack.dds",
            "gfx/other/d.dds",
        ] {
            idx.claim_workspace(key);
        }
        idx.add_paths(["gfx/pack/base.dds".to_string()]);

        let mut found = idx.workspace_owned_under(&["GFX\\pack/".to_string()]);
        found.sort();
        assert_eq!(found, ["gfx/pack/a.dds", "gfx/pack/deep/er/b.dds"]);

        assert!(
            idx.workspace_owned_under(&["gfx/pack/a.dds".to_string()])
                .is_empty()
        );
        assert!(
            idx.workspace_owned_under(&["gfx/pac".to_string()])
                .is_empty()
        );
        assert!(
            idx.workspace_owned_under(&["".to_string(), "/".to_string()])
                .is_empty()
        );
        assert!(idx.workspace_owned_under(&[]).is_empty());
    }

    /// Case-sensitive: the workspace spelling wins while it exists, and the lower
    /// layer's key stays after the release with its case unchecked (#1067).
    #[test]
    fn file_index_case_sensitive_claim_keeps_the_workspace_spelling() {
        let mut idx = FileIndex::new();
        idx.set_case_sensitive(true);
        assert!(idx.claim_workspace("gfx/Shared.DDS"));
        idx.add_paths(["gfx/shared.dds".to_string()]);
        assert!(idx.contains("gfx/Shared.DDS"));
        assert!(!idx.contains("gfx/shared.dds"));

        assert!(!idx.release_workspace("gfx/Shared.DDS"));
        assert!(idx.contains("gfx/shared.dds"));
        assert!(idx.contains("gfx/Shared.DDS"));
    }
}
