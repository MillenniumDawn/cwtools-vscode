//! The index data structures: cross-file type-instance index plus the file-path
//! and variable-name indexes it owns.

use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::dynamic_values;
use crate::{SourceLocation, dec_ref, is_subtype_key};

/// A single defined instance of a CW type (e.g. one event, one technology …).
#[derive(Debug, Clone)]
pub struct TypeInstance {
    /// The instance name (node key, or the value of `name_field` child).
    pub name: String,
    /// Where the definition starts in the source file.
    pub location: SourceLocation,
    /// The loc key for the type's `## primary` localisation when it is taken from
    /// an explicit field (e.g. an event's `title = <key>`), captured here so hover
    /// can show the localised title for a reference in another file without
    /// re-reading the definition. `None` when the type has no primary
    /// explicit-field localisation (name-derived keys are computed on demand).
    pub primary_loc_key: Option<String>,
    /// Loc keys this instance must provide that a `## required` localisation
    /// entry takes from a child field's value (`## required title = title`)
    /// rather than from the instance name. Resolved here because the node body
    /// is gone by the time CW100 runs off the index. Empty for the common case:
    /// a type with no required explicit-field loc entry, or an instance that
    /// omits the field.
    pub required_loc_keys: Vec<String>,
}

/// Holds all known instances for every type, aggregated across files.
/// An index of every file path under the game roots (mod + vanilla), used to
/// check that `filepath` references resolve (CW113). Paths are stored
/// forward-slashed, relative to their root. Lookups are case-insensitive by
/// default (Windows-authored mods); after [`FileIndex::set_case_sensitive`] the
/// files are matched by exact on-disk case, so a reference that only differs
/// from the on-disk path by case is caught for the case-sensitive filesystems
/// (Linux/Mac). The on-disk case is collected only while `case_sensitive` is
/// set, so the default run stores nothing extra. Cache-restored paths carry
/// their on-disk case (the cache stores it), so they are case-checked too.
#[derive(Debug, Clone, Default)]
pub struct FileIndex {
    /// Lowercased relative paths; the case-insensitive membership set.
    files: FxHashSet<String>,
    /// Lowercased relative path -> on-disk (original) case. Populated only
    /// while [`FileIndex::set_case_sensitive`] is true, so the default
    /// (case-insensitive) run pays nothing extra.
    files_exact: FxHashMap<String, String>,
    /// When true, [`FileIndex::contains`] enforces exact on-disk case for every
    /// indexed path, and [`FileIndex::add_root`]/[`FileIndex::add_paths`] record
    /// the original case needed to do so.
    case_sensitive: bool,
}

impl FileIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Walk `root` recursively and add every file's path relative to `root`.
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
            // Single pass: split on both separators, drop empty segments
            // (collapsing repeated/leading slashes), join with '/', lowercase ASCII.
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

    /// Exact-case membership. A reference that matches a known path only
    /// case-insensitively is treated as absent (it would fail to load on a
    /// case-sensitive filesystem). Falls back to case-insensitive membership
    /// for a path whose on-disk case was never recorded (possible only when the
    /// flag was turned on after the paths were added), so it isn't spuriously
    /// flagged.
    fn contains_exact(&self, path: &str) -> bool {
        thread_local! {
            static NORM_BUF: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
        }
        NORM_BUF.with(|buf| {
            let mut norm = buf.borrow_mut();
            norm.clear();
            // Single pass: split on both separators, drop empty segments
            // (collapsing repeated/leading slashes), join with '/'. Case kept.
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
                // We know this file's true case: require an exact match.
                Some(orig) => orig.as_str() == norm.as_str(),
                // On-disk case never recorded: case-insensitive membership.
                None => self.files.contains(norm_ci.as_str()),
            }
        })
    }

    /// The on-disk spelling of `path`, for a reference that names an indexed
    /// file but writes its case differently. `None` when the reference already
    /// matches, when nothing of that name is indexed, or on a case-insensitive
    /// run (which records no original case). Answers "then what is it called?"
    /// for a case-mismatch CW113.
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

    /// Insert a single workspace-relative path (forward slashes). Normalizes
    /// separators and case the same way as [`add_root`] and [`add_paths`].
    pub fn insert(&mut self, path: &str) {
        let norm = path.replace('\\', "/");
        let ci = norm.to_ascii_lowercase();
        self.files.insert(ci.clone());
        if self.case_sensitive {
            self.files_exact.insert(ci, norm);
        }
    }

    /// Remove a single workspace-relative path. Normalizes the same way as
    /// [`insert`] so a watched DELETE can drop the entry `insert` added.
    pub fn remove(&mut self, path: &str) {
        let norm = path.replace('\\', "/").to_ascii_lowercase();
        self.files.remove(&norm);
        if self.case_sensitive {
            self.files_exact.remove(&norm);
        }
    }

    /// Add relative paths (the vanilla-cache restore path), each carrying its
    /// on-disk case. Lowercased into the case-insensitive set always; recorded
    /// into the exact-case map only while `case_sensitive` is set, so the
    /// default run pays nothing extra.
    pub fn add_paths<I: IntoIterator<Item = String>>(&mut self, paths: I) {
        if self.case_sensitive {
            for p in paths {
                let ci = p.to_ascii_lowercase();
                self.files.insert(ci.clone());
                self.files_exact.insert(ci, p);
            }
        } else {
            for p in paths {
                self.files.insert(p.to_ascii_lowercase());
            }
        }
    }

    /// Toggle exact-case matching. Off by default (Windows-authored mods);
    /// enable for mods that also target case-sensitive filesystems (Linux/Mac),
    /// so a reference that only differs from the file by case is flagged. While
    /// on, every path added records its on-disk case. Call before building the
    /// index.
    pub fn set_case_sensitive(&mut self, case_sensitive: bool) {
        self.case_sensitive = case_sensitive;
    }

    /// The lowercased relative paths, for the case-insensitive membership set.
    pub fn paths(&self) -> impl Iterator<Item = &String> {
        self.files.iter()
    }

    /// The on-disk-case relative paths, for persisting to the vanilla cache so a
    /// later case-sensitive run can restore exact case. Empty unless
    /// [`FileIndex::set_case_sensitive`] was set before the paths were added.
    pub fn paths_exact(&self) -> impl Iterator<Item = &String> {
        self.files_exact.values()
    }

    /// Resolve `value` as a reference made relative to `referencing_file`'s own
    /// directory (the engine resolves a `.asset` `file =` beside the .asset, not
    /// under a fixed root prefix). `referencing_file` is the absolute on-disk
    /// path; its root-relative directory is recovered as the longest path-suffix
    /// that is itself an indexed file. Returns true when the directory-relative
    /// `value` resolves to an indexed path.
    pub fn resolve_relative(&self, referencing_file: &str, value: &str) -> bool {
        let segs: Vec<String> = referencing_file
            .split(['/', '\\'])
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase())
            .collect();
        if segs.len() < 2 {
            return false;
        }
        // Longest suffix first: the first suffix that is an indexed file is the
        // referencing file's own root-relative path. Everything before its
        // directory is the (un-indexed) root prefix.
        for start in 0..segs.len() - 1 {
            let self_path = segs[start..].join("/");
            if self.files.contains(&self_path) {
                let dir = &segs[start..segs.len() - 1];
                let sibling = if dir.is_empty() {
                    value.to_string()
                } else {
                    format!("{}/{}", dir.join("/"), value)
                };
                // `.asset`-relative resolution stays case-insensitive even in
                // case-sensitive mode: the directory segments are recovered from
                // the referencing file's lowercased path and can't be trusted
                // for an exact compare.
                return self.contains_ci(&sibling);
            }
        }
        false
    }
}

/// Project-wide set of defined script-variable names (every `value_set[...]`
/// definition collected across the mod + base game), used to check that a
/// `variable_field` reference resolves (CW246). Names are normalised to a
/// canonical key so a definition like `morale@ROOT` and a read like
/// `morale@GER` both resolve to `morale`. The CLI fills it during the batch
/// index; the LSP fills it incrementally as files are indexed.
#[derive(Debug, Clone, Default)]
pub struct VarIndex {
    /// Normalized variable name → how many definitions carry it. A refcount so the
    /// LSP can drop a name on `clear_file` only when its last definition goes,
    /// while the bulk CLI path (which never removes) just keeps incrementing.
    names: HashMap<String, usize>,
    /// Base-game variables staged from the vanilla cache or a live walk of the
    /// install. Distinct from `names` so a `clear_file` on a mod file that
    /// shares a name does not strip the base-game definition, and a re-merge
    /// replaces the previous contribution instead of double-counting (#306).
    vanilla_names: FxHashSet<String>,
}

impl VarIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty() && self.vanilla_names.is_empty()
    }

    /// Distinct union size; shared names counted once.
    pub fn len(&self) -> usize {
        let mut len = self.names.len();
        for v in &self.vanilla_names {
            if !self.names.contains_key(v.as_str()) {
                len += 1;
            }
        }
        len
    }

    /// Canonical lookup key for a raw variable token: lowercased, unquoted, the
    /// base before any `@`-concatenation, the last `.`-segment of that base, and
    /// before any `?`/`^` selector. Mirrors F# `getVariableFromString` plus the
    /// read-side dot-split in `changeScope`.
    pub fn normalize(raw: &str) -> String {
        let mut buf = String::new();
        Self::normalize_into(raw, &mut buf);
        buf
    }

    /// Like [`normalize`](Self::normalize) but writes the canonical key into a
    /// reusable buffer (cleared first), avoiding a per-call allocation on the hot
    /// `contains` path (and the validation crate's loop-var check). Identifiers
    /// are ASCII, so the lowercase fold is ASCII.
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

    /// Drop one definition of a name; removes the entry when its refcount hits 0.
    /// Used by the LSP's per-file `clear_file` so re-indexing a file refreshes its
    /// variables instead of leaking the old set.
    pub fn remove_name(&mut self, raw: &str) {
        let n = Self::normalize(raw);
        dec_ref(&mut self.names, n.as_str());
    }

    /// Whether a raw reference resolves to a known defined variable.
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

    /// Replace the base-game contribution with `names` (normalized). A re-merge
    /// drops the previous set before inserting the fresh one, so the count does
    /// not inflate, and a name the mod also defines survives a `clear_file` on
    /// that mod file (#306).
    pub fn set_vanilla_names(&mut self, names: Vec<String>) {
        self.vanilla_names.clear();
        for raw in names {
            let n = Self::normalize(&raw);
            if !n.is_empty() {
                self.vanilla_names.insert(n);
            }
        }
    }

    /// Drop the base-game contribution (e.g. on `clearAllCaches`).
    pub fn clear_vanilla_names(&mut self) {
        self.vanilla_names.clear();
    }

    /// Folds only `names` (workspace provenance); vanilla stays separate —
    /// use `set_vanilla_names` for base-game.
    pub fn merge(&mut self, other: &VarIndex) {
        for (name, count) in &other.names {
            *self.names.entry(name.clone()).or_insert(0) += count;
        }
    }

    /// The normalized defined names, for persisting to the vanilla cache.
    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.names.keys()
    }

    /// Base-game names staged via [`set_vanilla_names`](Self::set_vanilla_names).
    #[cfg(test)]
    pub(crate) fn vanilla_names_iter(&self) -> impl Iterator<Item = &String> {
        self.vanilla_names.iter()
    }

    /// Distinct union of workspace + base-game names (shared filtered).
    pub(crate) fn all_names(&self) -> impl Iterator<Item = &String> {
        self.names.keys().chain(
            self.vanilla_names
                .iter()
                .filter(|k| !self.names.contains_key(k.as_str())),
        )
    }
}

/// Scripted-localisation names (`defined_text = { name = X }`) read straight off
/// the files in a scripted-loc folder, not through a ruleset type.
///
/// The loc-command check needs to tell a scripted localisation apart from a
/// typo, and asking the ruleset for it does not work: the HOI4 config declares
/// `type[scripted_loc]` at Stellaris's `game/common/scripted_loc`, so nothing
/// under HOI4's `common/scripted_localisation` is ever typed and every use of
/// one read as an unknown command (#348). The engine already fixes that folder
/// name elsewhere (`cwtools_validation::initial_scope_context`), so collecting
/// by path here keeps the two agreeing.
///
/// Names are stored lowercased; Paradox identifiers are case-insensitive.
/// Refcounted per file so the LSP's re-index of one file refreshes its names
/// instead of leaking the old set, same as [`VarIndex`].
#[derive(Debug, Clone, Default)]
pub struct ScriptedLocIndex {
    names: FxHashMap<Arc<str>, usize>,
    per_file: FxHashMap<Arc<str>, Vec<Arc<str>>>,
    /// Base-game names staged from the vanilla cache or a live walk. Kept apart
    /// from `names` so a `remove_file` on a mod file sharing a name does not
    /// strip the base-game definition (same split as [`VarIndex`]).
    vanilla_names: FxHashSet<Arc<str>>,
}

impl ScriptedLocIndex {
    /// Replace `file_uri`'s contribution with `names`.
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

    /// Drop `file_uri`'s contribution (refcounted).
    pub fn remove_file(&mut self, file_uri: &str) {
        let Some(flat) = self.per_file.remove(file_uri) else {
            return;
        };
        for name in flat {
            dec_ref(&mut self.names, name.as_ref());
        }
    }

    /// Replace the base-game contribution with `names`. A re-merge drops the
    /// previous set rather than accumulating.
    pub fn set_vanilla_names(&mut self, names: Vec<String>) {
        self.vanilla_names = names
            .into_iter()
            .filter(|n| !n.is_empty())
            .map(|n| Arc::from(n.to_ascii_lowercase().as_str()))
            .collect();
    }

    /// Whether any scripted localisation is known. `true` means the check has no
    /// data and must stay lenient rather than call every command a typo.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty() && self.vanilla_names.is_empty()
    }

    pub fn contains(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        self.names.contains_key(lower.as_str()) || self.vanilla_names.contains(lower.as_str())
    }

    /// The workspace names, for persisting to the vanilla cache.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.names.keys().map(Arc::as_ref)
    }
}

/// Scripted-GUI callback names read from direct keys in each GUI's `effects`
/// and `triggers` containers.
#[derive(Debug, Clone, Default)]
pub struct ScriptedGuiIndex {
    names: FxHashMap<Arc<str>, usize>,
    per_file: FxHashMap<Arc<str>, Vec<Arc<str>>>,
    vanilla_names: FxHashSet<Arc<str>>,
}

impl ScriptedGuiIndex {
    /// Replace `file_uri`'s contribution with `names`.
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

    /// Drop `file_uri`'s contribution.
    pub fn remove_file(&mut self, file_uri: &str) {
        let Some(flat) = self.per_file.remove(file_uri) else {
            return;
        };
        for name in flat {
            dec_ref(&mut self.names, name.as_ref());
        }
    }

    /// Replace the base-game contribution with `names`.
    pub fn set_vanilla_names(&mut self, names: Vec<String>) {
        self.vanilla_names = names
            .into_iter()
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

    /// The workspace names, for persisting to the vanilla cache.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.names.keys().map(Arc::as_ref)
    }
}

/// How many definitions of one instance name a type holds, split by where they
/// came from. `total` answers "does this name exist" (`contains`); `workspace`
/// answers "did the project define it more than once" (CW261), which a
/// base-game definition the mod is overriding must not contribute to.
#[derive(Debug, Default, Clone, Copy)]
struct NameRefs {
    total: usize,
    workspace: usize,
}

/// Drop one definition of `name` from a type's name set, removing the entry
/// once its last definition goes. The [`NameRefs`] counterpart of `dec_ref`.
fn release_name(set: &mut FxHashMap<Arc<str>, NameRefs>, name: &str, base_game: bool) {
    if let Some(refs) = set.get_mut(name)
        && refs.release(base_game)
    {
        set.remove(name);
    }
}

impl NameRefs {
    fn add(&mut self, base_game: bool) {
        self.total += 1;
        if !base_game {
            self.workspace += 1;
        }
    }

    /// Drop one definition, reporting whether the name is now gone entirely.
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
    /// type_name → Vec<(file_uri, instance)>
    pub map: FxHashMap<String, Vec<(Arc<str>, TypeInstance)>>,
    /// lowercased instance name → how many definitions carry that name (across all
    /// types and files). Lets `is_any_instance` be O(1) instead of scanning every
    /// instance. A refcount so `remove_file` can drop a name only when its last
    /// definition goes. Keyed lowercase because Paradox identifiers are
    /// case-insensitive (same normalization as `contains`/`instance_sets`).
    name_counts: FxHashMap<Arc<str>, usize>,
    /// type_name → (lowercased instance name → refcount). Makes `contains` an O(1)
    /// hash lookup instead of a linear scan over every instance of the type, which
    /// was quadratic over the corpus for high-cardinality types (state, character,
    /// country_event). The refcount lets `remove_file` drop a name only when its
    /// last definition in that type goes.
    instance_sets: FxHashMap<String, FxHashMap<Arc<str>, NameRefs>>,
    /// The URIs a base-game merge contributed. Keeps the removal paths able to
    /// tell which half of a [`NameRefs`] an instance belongs to, and stays empty
    /// on a mod-only run.
    base_game_uris: FxHashSet<Arc<str>>,
    /// file_uri → type_name → this file's own positions within `map[type_name]`.
    /// Lets [`instances_in_file`](Self::instances_in_file) and
    /// [`remove_file`](Self::remove_file) both cost O(the file's own entries)
    /// instead of scanning a type's whole instance vec looking for this file's
    /// matches. `remove_file` drops entries via `swap_remove`, which relocates
    /// the vec's last element into the freed slot, so every swap repairs the
    /// position recorded for whichever other file owned that relocated entry
    /// (see `swap_remove_instance`). Kept in sync by every insertion (`merge`,
    /// `merge_base_game_with_uris`) and removal (`remove_file`, `remove_files`)
    /// path. Not serialized: the vanilla cache reloads through
    /// `merge_base_game_with_uris`, which
    /// rebuilds this map (same as `name_counts` / `instance_sets`).
    file_positions: FxHashMap<Arc<str>, FxHashMap<String, Vec<usize>>>,
    /// Index of every asset/file path under the game roots, for `filepath`
    /// reference checks (CW113). Empty unless the CLI populated it.
    pub file_index: FileIndex,
    /// Project-wide set of defined variable names, for `variable_field`
    /// reference checks (CW246). The CLI fills it during the batch index;
    /// the LSP fills it incrementally via `InfoService`.
    pub var_index: VarIndex,
    /// Project-wide set of scripted-localisation names, for the loc-command
    /// checks (CW226/CW266). Filled by path rather than by ruleset type; see
    /// [`ScriptedLocIndex`].
    pub scripted_loc_index: ScriptedLocIndex,
    /// Project-wide scripted-GUI callbacks used by `[!name]` localisation calls.
    pub scripted_gui_index: ScriptedGuiIndex,
    /// Whether this index includes vanilla (base-game) definitions. When
    /// `false`, CW500 type-reference checks are skipped to avoid false
    /// positives on valid vanilla cross-references. The driver sets this
    /// to `true` after merging vanilla data.
    pub complete: bool,
    /// Complex-enum members collected from indexed files (enum name -> values),
    /// e.g. `equipment_stat`, `country_tags`, `idea_name`. Completion-only.
    pub complex_enum_values: dynamic_values::NamedValueIndex,
    /// `value_set[...]` members collected from indexed files (namespace ->
    /// values), e.g. `country_flag`, `global_flag`. Feeds completion, and also
    /// validation: a `from_data` scope link with `data_source = value[<set>]`
    /// makes any member of that set a scope-opening key, so dropping this from
    /// an index the rule engine reads moves diagnostics
    /// (`validation::rule_core::matching::is_from_data_value_set_member`).
    pub value_set_values: dynamic_values::NamedValueIndex,
}

impl TypeIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return true if `type_name` has a known instance called `instance`.
    /// Paradox script identifiers are case-insensitive, so a reference like
    /// `LBA_AI_BEHAVIOR` resolves to the `LBA_ai_behavior` definition.
    pub fn contains(&self, type_name: &str, instance: &str) -> bool {
        let Some(names) = self.instance_sets.get(type_name) else {
            return false;
        };
        // Borrow the key directly when it's already lowercase (the common case),
        // only allocating a lowercase copy when it actually has uppercase bytes.
        if instance.bytes().any(|b| b.is_ascii_uppercase()) {
            let lower = instance.to_ascii_lowercase();
            names.contains_key(lower.as_str())
        } else {
            names.contains_key(instance)
        }
    }

    /// How many times the workspace defines `instance` as a `type_name`
    /// (case-insensitive). Base-game definitions are excluded, so a mod that
    /// redefines a base-game instance reads as an override, not a duplicate.
    /// More than one is CW261's whole question.
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

    /// Return true if `name` is a known instance of ANY type. Used to recognise
    /// scope-opening keys: HOI4 from-data scope links (links.cwt) let an instance
    /// of a referenced type (character, state, ideology, ...) open its own scope,
    /// e.g. `LBA_some_character = { ... }`.
    pub fn is_any_instance(&self, name: &str) -> bool {
        if name.bytes().any(|b| b.is_ascii_uppercase()) {
            let lower = name.to_ascii_lowercase();
            self.name_counts.contains_key(lower.as_str())
        } else {
            self.name_counts.contains_key(name)
        }
    }

    /// All instances for a type (across all files).
    pub fn instances(&self, type_name: &str) -> &[(Arc<str>, TypeInstance)] {
        self.map.get(type_name).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Every definition site of an instance named `name` (case-insensitive),
    /// across all types. Used by goto-definition's fallback for dotted ids
    /// (events, decisions) that the heuristic def index keys by node-key rather
    /// than by the instance id. Scans the index (rare interactive path).
    pub fn instance_locations(&self, name: &str) -> Vec<(Arc<str>, SourceLocation)> {
        self.map
            .values()
            .flatten()
            .filter(|(_, inst)| inst.name.eq_ignore_ascii_case(name))
            .map(|(uri, inst)| (uri.clone(), inst.location))
            .collect()
    }

    /// The explicit-field primary loc key captured for `name`'s instance of
    /// `type_name` (e.g. an event's `title` loc key), if any. Lets hover show the
    /// localised title for a reference. Case-insensitive on the instance name.
    pub fn primary_loc_key(&self, type_name: &str, name: &str) -> Option<&str> {
        self.map
            .get(type_name)?
            .iter()
            .filter(|(_, inst)| inst.name.eq_ignore_ascii_case(name))
            .find_map(|(_, inst)| inst.primary_loc_key.as_deref())
    }

    /// Names a loc `$ref$` may bind to besides loc keys: every type-instance
    /// name (dynamic modifiers, ideas, buildings, …) and every defined variable,
    /// lowercased. The caller unions modifiers / vanilla loc keys on top. Lets
    /// loc validation accept `$education_dynamic_modifier$` / `$some_variable$`
    /// embeds without a CW225 while genuine typos (matching nothing) still flag.
    pub fn loc_bindable_names(&self) -> impl Iterator<Item = String> + '_ {
        // `name_counts` keys and `var_index` names are already lowercased /
        // normalised, matching the loc validator's case-insensitive lookup.
        self.loc_bindable_names_iter().map(str::to_string)
    }

    /// Borrowing form of [`loc_bindable_names`](Self::loc_bindable_names): yields
    /// each bindable name by reference, no per-name allocation.
    pub(crate) fn loc_bindable_names_iter(&self) -> impl Iterator<Item = &str> + '_ {
        self.name_counts
            .keys()
            .map(AsRef::as_ref)
            .chain(self.var_index.all_names().map(String::as_str))
    }

    /// Every `(type_name, instance)` defined in `file_uri`. Used by
    /// document-symbol/outline, which is on-demand and infrequent, and by the
    /// per-file CW100/unused-instance validation passes, which run on every
    /// file every time. Reads straight off the reverse map (`file_positions`),
    /// so the cost is proportional to the file's own entries rather than the
    /// whole index, even for a type with thousands of instances spread across
    /// many other files (same narrowing as `remove_file`).
    pub fn instances_in_file<'a>(&'a self, file_uri: &str) -> Vec<(&'a str, &'a TypeInstance)> {
        let Some(type_positions) = self.file_positions.get(file_uri) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(type_positions.values().map(Vec::len).sum());
        for (type_name, positions) in type_positions {
            // Skip subtype-qualified membership keys: the instance already
            // appears under its base `type`, so listing it again would duplicate
            // the outline / document-symbol entry.
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

    /// Every type name that indexes a definition in `file_uri`, including its
    /// subtype-qualified membership names. `instances_in_file` deliberately
    /// omits those membership entries for outline consumers, but reference
    /// lookups need them to find uses written as `<type.subtype>`.
    pub fn instance_type_names_in_file<'a>(
        &'a self,
        file_uri: &str,
        name: &str,
        location: SourceLocation,
    ) -> Vec<&'a str> {
        let Some(type_positions) = self.file_positions.get(file_uri) else {
            return Vec::new();
        };
        let mut type_names = Vec::new();
        for (type_name, positions) in type_positions {
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

    /// Merge per-file results into the index.
    ///
    /// A subtype-qualified key (`"type.subtype"`, recognised by the `.`) is a
    /// membership entry produced by [`SubtypeCollector`]. Such entries feed
    /// `contains` (so `<type.subtype>` references resolve) but are deliberately
    /// kept out of `name_counts` — they share the instance's name with the base
    /// `type` entry, and double-counting would skew `is_any_instance` refcounts
    /// and document-symbol output without adding a distinct definition.
    #[tracing::instrument(skip_all, fields(types = per_type.len()))]
    pub fn merge(&mut self, file_uri: &str, per_type: HashMap<String, Vec<TypeInstance>>) {
        self.merge_from(file_uri, per_type, false);
    }

    /// As [`merge`](Self::merge), but for base-game content. The instances are
    /// indexed exactly the same way; they just don't count toward
    /// [`workspace_definition_count`](Self::workspace_definition_count), so a
    /// mod redefining a base-game instance reads as an override rather than a
    /// duplicate.
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
                .entry(type_name)
                .or_default();
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
                set.entry(lower).or_default().add(base_game);
                positions.push(entry.len());
                entry.push((Arc::clone(&uri), inst));
            }
        }
    }

    /// Merge base-game instances that each carry their own source URI. Like
    /// [`merge_base_game`](Self::merge_base_game), but the per-instance URI is
    /// stored as-is instead of a single shared key, so a batch spanning many
    /// files (the vanilla index, where every base-game file contributes a few
    /// instances) keeps each instance pointing at its real source file.
    /// `remove_files` drops such a batch by URI.
    pub fn merge_base_game_with_uris(
        &mut self,
        per_type: impl IntoIterator<Item = (String, Vec<(Arc<str>, TypeInstance)>)>,
    ) {
        for (type_name, instances) in per_type {
            let subtype_key = is_subtype_key(&type_name);
            let set = self.instance_sets.entry(type_name.clone()).or_default();
            let entry = self.map.entry(type_name.clone()).or_default();
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
                set.entry(lower).or_default().add(true);
                // Each instance can come from a different file, so key on its own
                // uri; clone the type name only the first time it's seen per uri.
                let pos = entry.len();
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
        }
    }

    /// Remove every instance contributed by any file in `file_uris`, in a single
    /// pass over the index. Use this to drop a large multi-file contribution (the
    /// whole vanilla index) at once: a plain linear pass over every bucket beats
    /// calling [`remove_file`](Self::remove_file) once per file, whose per-file
    /// bookkeeping overhead adds up across thousands of files even though each
    /// individual call is cheap. Only touches the type instances; the
    /// dynamic-value indexes are keyed separately and untouched.
    pub fn remove_files(&mut self, file_uris: &HashSet<Arc<str>>) {
        if file_uris.is_empty() {
            return;
        }
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
                    if let Some(set) = self.instance_sets.get_mut(type_name) {
                        release_name(set, lower.as_str(), base_game);
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
        // `retain` shifted every surviving entry's position within its type's
        // vec, so `file_positions` (which records those positions) can't be
        // patched incrementally; rebuild it wholesale. Still O(total surviving
        // instances), matching the rest of this bulk pass.
        self.file_positions.clear();
        for (type_name, entries) in &self.map {
            for (i, (uri, _)) in entries.iter().enumerate() {
                self.file_positions
                    .entry(Arc::clone(uri))
                    .or_default()
                    .entry(type_name.clone())
                    .or_default()
                    .push(i);
            }
        }
    }

    /// Remove all instances contributed by `file_uri`.
    ///
    /// Drops each of the file's own entries from `map` via `swap_remove`
    /// (constant time), guided by the positions the reverse map
    /// (`file_positions`) recorded for this file, so the cost is proportional to
    /// the file's own entries rather than the type's whole instance vec. A
    /// bucket empties only when its last contributor is removed, and that
    /// contributor always has the bucket in its `file_positions` entry, so every
    /// emptied bucket is still visited and pruned here.
    pub fn remove_file(&mut self, file_uri: &str) {
        self.complex_enum_values.remove_file(file_uri);
        self.value_set_values.remove_file(file_uri);
        self.scripted_loc_index.remove_file(file_uri);
        self.scripted_gui_index.remove_file(file_uri);
        // No entry means the file contributed no type instances.
        let Some(type_positions) = self.file_positions.remove(file_uri) else {
            return;
        };
        let base_game = self.base_game_uris.remove(file_uri);
        for (type_name, mut positions) in type_positions {
            // Subtype-qualified keys never contributed to `name_counts` (see
            // `merge`), so they must not decrement it here.
            let subtype_key = is_subtype_key(&type_name);
            // Largest index first: each `swap_remove` only ever displaces the
            // vec's current last element, whose index is always >= every
            // position still queued for this file (they're all distinct indices
            // into the same original vec), so earlier removals never invalidate
            // a later one in this loop.
            positions.sort_unstable_by(|a, b| b.cmp(a));
            for index in positions {
                let Some(v) = self.map.get(type_name.as_str()) else {
                    continue;
                };
                let lower = v[index].1.name.to_ascii_lowercase();
                if !subtype_key {
                    dec_ref(&mut self.name_counts, lower.as_str());
                }
                if let Some(set) = self.instance_sets.get_mut(&type_name) {
                    release_name(set, lower.as_str(), base_game);
                }
                self.swap_remove_instance(&type_name, index);
            }
            if self.map.get(type_name.as_str()).is_some_and(Vec::is_empty) {
                self.map.remove(&type_name);
                self.instance_sets.remove(&type_name);
            }
        }
    }

    /// Drop `map[type_name][index]` via `swap_remove`. When the removed slot
    /// wasn't already the vec's last element, the element that used to be last
    /// now lives at `index`; repair the position [`remove_file`](Self::remove_file)
    /// recorded for whichever other file owns it so `file_positions` stays a
    /// faithful inverse of `map`. `remove_file` only ever calls this with an
    /// `index` at or before the current last element (see its own ordering
    /// invariant), so the relocated entry is never one of that same file's
    /// still-pending positions.
    fn swap_remove_instance(&mut self, type_name: &str, index: usize) {
        let Some(v) = self.map.get_mut(type_name) else {
            return;
        };
        let last = v.len() - 1;
        v.swap_remove(index);
        if index == last {
            return;
        }
        let moved_uri = Arc::clone(&v[index].0);
        if let Some(slot) = self
            .file_positions
            .get_mut(&moved_uri)
            .and_then(|by_type| by_type.get_mut(type_name))
            .and_then(|positions| positions.iter_mut().find(|p| **p == last))
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
        // The engine collapses repeated slashes, so a `gfx//interface/x.dds`
        // reference (as some MD .gfx files write) must resolve to the indexed
        // `gfx/interface/x.dds`, not flag CW113.
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
        // Live-walked paths record on-disk case when the flag is on.
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
        // With the flag off, no on-disk case is recorded (no memory overhead),
        // and lookup stays case-insensitive.
        let mut idx = FileIndex::new();
        idx.add_paths(vec!["gfx/interface/x.dds".to_string()]);
        assert!(idx.files_exact.is_empty());
        assert!(idx.contains("GFX/interface/X.dds"));
    }

    #[test]
    fn file_index_exact_case_applies_to_cache_restored_paths() {
        // Cache-restored paths carry on-disk case, so they are case-checked too.
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
        // goto-definition (#39): an event/decision reference resolves by its
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
    fn instances_in_file_only_this_files_entries_excludes_subtype_keys() {
        let mut idx = TypeIndex::new();
        idx.merge(
            "file://a.txt",
            HashMap::from([
                ("event".to_string(), vec![inst("a_ev", 1)]),
                ("tech".to_string(), vec![inst("a_tech", 2)]),
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
        assert_eq!(got, vec![("event", "a_ev"), ("tech", "a_tech")]);
        let location = SourceLocation {
            line: 1,
            col: 0,
            end: (1, 0),
        };
        let mut type_names = idx.instance_type_names_in_file("file://a.txt", "a_ev", location);
        type_names.sort();
        assert_eq!(type_names, vec!["event", "event.subt"]);

        assert!(idx.instances_in_file("file://never.txt").is_empty());
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

    // ── removal parity (reverse-map narrowed removal) ────────────────────────

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

    /// Comparable projection of every observable index structure. Sorted so the
    /// comparison is order-independent (removal preserves order, a from-scratch
    /// rebuild reproduces it, but sorting keeps the assertion robust either way).
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
        (map, name_counts, instance_sets)
    }

    /// Removing a `merge`-contributed file must leave exactly the state of a
    /// rebuild that never saw it. Exercises the single-uri insertion path.
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

    /// Removing a `merge_base_game_with_uris`-contributed file must likewise match a
    /// rebuild without it. Exercises the per-instance-uri insertion path, whose
    /// reverse-map bookkeeping differs from the single-uri path.
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
        // c was the only source of the subtype membership.
        assert!(!full.contains("event.subt", "shared_ev"));
        assert!(full.contains("event", "shared_ev")); // still via a
    }

    /// merge → remove → re-merge → remove cycles leave the index bit-empty each
    /// time, including the reverse map, with empty buckets pruned.
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

    /// Bulk `remove_files` followed by a singular `remove_file`: the vanilla
    /// batch drops in one pass, then the last mod file drops, leaving nothing.
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

    /// Removing a URI that never contributed anything is a no-op.
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

    /// Removing a file merged early in a shared type's vec relocates a later
    /// file's entries into the freed slots, exercising the repair
    /// `swap_remove_instance` does for whichever other file owns the entry a
    /// swap displaces: 20 files each contribute 3 `state` instances in merge
    /// order f0..f19, so removing f5 (positions 15..18 of 60) drains the vec's
    /// tail (f19's 3 entries) into f5's freed slots. `instances_in_file`
    /// checked right after proves those relocations produced the right
    /// answer for every survivor, including f19 whose positions just got
    /// rewritten. That alone doesn't prove the rewritten position values
    /// themselves are right, only that reading through them still lands on
    /// the correct instances (a read is order-agnostic; `instances_in_file`
    /// doesn't care which position a name is filed under, just that it maps
    /// to the right one), so the test then removes f19, which consumes its
    /// just-repaired positions as indices into `map`. A wrong repair there
    /// would make this second removal panic (an out-of-bounds index) or
    /// silently remove the wrong instances, either of which the final
    /// snapshot-parity and survivor checks catch.
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

        // f19's positions were just rewritten by the relocations above; if the
        // rewrite were wrong, consuming them as indices here would panic or
        // remove the wrong entries instead of f19's own.
        idx.remove_file("file://f19.txt");
        assert_eq!(snapshot(&idx), snapshot(&build(&[5, 19])));
        assert_survivors_intact(&idx, &[5, 19]);
    }

    /// A subtype-qualified membership key never feeds `name_counts`, so removing
    /// the file must drive the base name's count to zero exactly once.
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
        // A sound `.asset` `file =` resolves beside the .asset, not under the
        // field's `sound/` root prefix. The referencing file's path is absolute;
        // its root-relative dir is recovered as the longest indexed path-suffix.
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
        idx.set_vanilla_names(vec!["Shared".into(), "VanillaOnly".into()]);
        idx.remove_file("a.txt");
        assert!(idx.contains("Shared"), "the base-game definition stays");
        assert!(idx.contains("VanillaOnly"));
    }

    #[test]
    fn scripted_loc_vanilla_remerge_replaces_not_accumulates() {
        let mut idx = ScriptedLocIndex::default();
        idx.set_vanilla_names(vec!["First".into()]);
        idx.set_vanilla_names(vec!["Second".into()]);
        assert!(!idx.contains("First"));
        assert!(idx.contains("Second"));
    }

    #[test]
    fn scripted_loc_names_are_the_workspace_half_only() {
        let mut idx = scripted_loc_index_with("a.txt", &["ModOne"]);
        idx.set_vanilla_names(vec!["VanillaOne".into()]);
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
        idx.set_vanilla_names(vec!["Shared".into(), "VanillaOnly".into()]);
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
        vi.set_vanilla_names(vec!["vanilla_var".to_string()]);
        assert!(vi.contains("vanilla_var"));
    }

    #[test]
    fn var_index_vanilla_set_case_insensitive() {
        let mut vi = VarIndex::new();
        vi.set_vanilla_names(vec!["vanilla_var".to_string()]);
        assert!(vi.contains("VANILLA_VAR"));
    }

    #[test]
    fn var_index_vanilla_set_is_empty_and_len() {
        let mut vi = VarIndex::new();
        assert!(vi.is_empty());
        vi.set_vanilla_names(vec!["vanilla_var".to_string()]);
        assert!(!vi.is_empty());
        assert_eq!(vi.len(), 1);
    }

    #[test]
    fn var_index_vanilla_quoted_trim_normalized() {
        let mut vi = VarIndex::new();
        vi.set_vanilla_names(vec!["\"quoted_var\"".to_string()]);
        assert!(vi.contains("quoted_var"));
    }

    #[test]
    fn var_index_vanilla_dot_last_segment() {
        let mut vi = VarIndex::new();
        vi.set_vanilla_names(vec!["foo.bar^2".to_string()]);
        assert!(vi.contains("bar"));
    }

    #[test]
    fn var_index_vanilla_suffix_stripping() {
        let mut vi = VarIndex::new();
        vi.set_vanilla_names(vec!["my_var?100".to_string(), "my_var@GER".to_string()]);
        assert!(vi.contains("my_var"));
        assert_eq!(vi.len(), 1, "? and @ suffixes collapse to same key");
    }

    #[test]
    fn var_index_vanilla_empty_and_whitespace_skipped() {
        let mut vi = VarIndex::new();
        vi.set_vanilla_names(vec!["   ".to_string(), "".to_string()]);
        assert!(vi.is_empty());
        assert_eq!(vi.len(), 0);
    }

    #[test]
    fn var_index_vanilla_len_union_does_not_double_count_shared() {
        let mut vi = VarIndex::new();
        vi.add_name("shared_var");
        vi.set_vanilla_names(vec!["shared_var".to_string(), "vanilla_only".to_string()]);
        assert!(vi.contains("shared_var"));
        assert!(vi.contains("vanilla_only"));
        assert_eq!(vi.len(), 2, "shared name counts once");
        assert!(!vi.is_empty());
    }

    #[test]
    fn var_index_vanilla_loc_bindable_includes_vanilla() {
        let mut idx = TypeIndex::new();
        idx.var_index
            .set_vanilla_names(vec!["vanilla_loc_var".to_string()]);
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
        vi.set_vanilla_names(vec!["shared_var".to_string()]);
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
        vi.set_vanilla_names(vec!["old_var".to_string()]);
        assert!(vi.contains("old_var"));
        vi.set_vanilla_names(vec!["new_var".to_string()]);
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
        vi.set_vanilla_names(vec!["vanilla_var".to_string()]);
        vi.clear_vanilla_names();
        assert!(!vi.contains("vanilla_var"));
        assert!(vi.contains("mod_var"));
        assert!(!vi.is_empty());
    }

    #[test]
    fn var_index_vanilla_second_clear_idempotent() {
        let mut vi = VarIndex::new();
        vi.add_name("mod_var");
        vi.set_vanilla_names(vec!["vanilla_var".to_string()]);
        vi.clear_vanilla_names();
        vi.clear_vanilla_names();
        assert!(vi.contains("mod_var"));
        assert!(!vi.contains("vanilla_var"));
    }

    #[test]
    fn var_index_vanilla_empty_vector_clears_previous() {
        let mut vi = VarIndex::new();
        vi.set_vanilla_names(vec!["a".to_string()]);
        vi.set_vanilla_names(vec![]);
        assert!(!vi.contains("a"));
        assert!(vi.is_empty());
    }

    #[test]
    fn var_index_merge_does_not_copy_vanilla_names() {
        let mut vanilla = VarIndex::new();
        vanilla.set_vanilla_names(vec!["vanilla_only".to_string()]);
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
        vi.set_vanilla_names(vec!["shared".to_string(), "vanilla".to_string()]);
        assert_eq!(vi.all_names().count(), vi.len());
        assert_eq!(vi.len(), 2);
    }

    #[test]
    fn var_index_all_names_count_equals_len_when_shared_cross_form() {
        let mut vi = VarIndex::new();
        vi.add_name("SHARED_VAR");
        vi.set_vanilla_names(vec![
            "shared_var".to_string(),
            "My_Var@GER".to_string(),
            "foo.bar".to_string(),
        ]);
        vi.add_name("my_var");
        vi.add_name("bar");
        // SHARED_VAR/shared_var and My_Var@GER/my_var and foo.bar/bar all collapse
        assert_eq!(vi.all_names().count(), vi.len());
        assert_eq!(vi.len(), 3);
    }

    #[test]
    fn vanilla_names_iter_returns_staged() {
        let mut vi = VarIndex::new();
        vi.set_vanilla_names(vec!["b".to_string(), "a".to_string()]);
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
        // My_Var and My_Var@GER collapse, so 3 adds -> 2 distinct (my_var, bar)
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
        // exact-case map must not alias
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
            .set_vanilla_names(vec!["Vanilla_Var".to_string()]);
        let snap = idx.clone();
        let mut mutated = snap.clone();
        mutated
            .var_index
            .set_vanilla_names(vec!["Other_Vanilla".to_string()]);
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

    /// Workspace scan pass 2 validates against a *clone* of the live index, and
    /// the rule engine reads `value_set_values` for `from_data` scope links. A
    /// clone that dropped the dynamic-value indexes would leave every unit test
    /// green (they build their index directly) while silently moving scan
    /// diagnostics, which is the trap #332's "keep them out of the snapshot"
    /// plan sets. Pin that the snapshot still answers membership.
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

    /// Workspace scan pass 2 validates against a *clone* of the live index and
    /// reads `instances_in_file` off that clone for the unused-instance pass,
    /// long after an edit may have merged or removed files in the live one. The
    /// clone has to keep answering with the view it was taken from.
    ///
    /// `remove_file` is the sharp case, because it compacts the shared type vec
    /// by swapping tail entries into the freed slots and rewriting the displaced
    /// file's positions. A clone that aliased `map` or `file_positions` would
    /// not merely lose an instance: it would read a surviving file's positions
    /// against a compacted vec and hand back somebody else's (#328).
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

        // Removing f0 drains the vec's tail (f3's entries) into the freed slots.
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
}
