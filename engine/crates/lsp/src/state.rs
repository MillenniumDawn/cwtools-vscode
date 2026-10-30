#[cfg(test)]
use parking_lot::Condvar;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tower_lsp::Client;
use tower_lsp::lsp_types::*;

use cwtools_info::{IndexedUseSite, TypeInstance, UseSite};
use cwtools_parser::ast::ParsedFile;
use cwtools_rules::rules_types::RuleSet;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::{InlineScripts, references};

use crate::scan::ScanSummary;

pub(crate) use crate::loc_locations::LocLocations;
pub(crate) use crate::loc_text::LocText;

/// Where a test may park a background pipeline mid-flight so it can observe
/// the state in between. Pass 2 of the workspace scan has three points; the
/// watched batch has one at the top of every per-file iteration, outside the
/// validation permit (#477). Validation has one inside its synchronous phase.
/// The open-buffer uses of a scan have one inside their rayon region (#1152), and its
/// type-uses merge has one before it takes the `type_uses` write guard (#1163).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HoldPoint {
    Pass2Before,
    Pass2Mid,
    Pass2After,
    OpenUses,
    TypeUsesMerge,
    WatchedFile,
    ValidatePrepared,
}

/// A one-shot condvar gate: the first `hold` at the configured point blocks
/// until `release`; later holds pass through.
#[cfg(test)]
pub(crate) struct HoldGate {
    hold_at: HoldPoint,
    state: Mutex<HoldGateState>,
    cv: Condvar,
}

#[cfg(test)]
struct HoldGateState {
    arrived: bool,
    released: bool,
}

#[cfg(test)]
impl HoldGate {
    pub(crate) fn new(hold_at: HoldPoint) -> Arc<Self> {
        Arc::new(Self {
            hold_at,
            state: Mutex::new(HoldGateState {
                arrived: false,
                released: false,
            }),
            cv: Condvar::new(),
        })
    }

    pub(crate) fn hold(&self, point: HoldPoint) {
        if self.hold_at != point {
            return;
        }
        let mut st = self.state.lock();
        if st.arrived {
            return;
        }
        st.arrived = true;
        self.cv.notify_all();
        while !st.released {
            self.cv.wait(&mut st);
        }
    }

    pub(crate) fn has_arrived(&self) -> bool {
        self.state.lock().arrived
    }

    pub(crate) fn release(&self) {
        let mut st = self.state.lock();
        st.released = true;
        self.cv.notify_all();
    }
}

pub(crate) struct Config {
    pub(crate) language: String,
    pub(crate) workspace_uri: Option<Arc<str>>,
    pub(crate) workspace_prefix: Option<Arc<str>>,
    pub(crate) workspace_roots: Vec<std::path::PathBuf>,
    pub(crate) authorized_roots: Arc<[std::path::PathBuf]>,
    pub(crate) editable_roots: Arc<[std::path::PathBuf]>,
    pub(crate) vanilla_dir: Option<std::path::PathBuf>,
    /// Parent mods in load order (#786): indexed between the base game and
    /// the workspace, readable but never edited or diagnosed.
    pub(crate) parent_roots: Vec<std::path::PathBuf>,
    pub(crate) cache_dir: Option<std::path::PathBuf>,
    pub(crate) loc_languages: Option<Vec<cwtools_localization::Lang>>,
    pub(crate) ignore_file_patterns: Vec<String>,
    pub(crate) ignore_dir_patterns: Vec<String>,
    pub(crate) ignored_error_codes: Vec<String>,
    pub(crate) rules_dir: Option<std::path::PathBuf>,
    pub(crate) scope_checks: bool,
    pub(crate) var_checks: bool,
    pub(crate) background_reindex_interval_minutes: u64,
    pub(crate) background_reindex_idle_seconds: u64,
    pub(crate) workspace_wide_diagnostics: bool,
    /// Position encoding negotiated with the client. LSP defaults to UTF-16.
    pub(crate) position_encoding: tower_lsp::lsp_types::PositionEncodingKind,
    pub(crate) formatting: cwtools_parser::format::FormatOptions,
}

impl Config {
    pub(crate) fn new() -> Self {
        let (scope_checks, var_checks) = cwtools_validation::checks_from_env();
        Self {
            language: "paradox".to_string(),
            workspace_uri: None,
            workspace_prefix: None,
            workspace_roots: Vec::new(),
            authorized_roots: Arc::from([]),
            editable_roots: Arc::from([]),
            vanilla_dir: None,
            parent_roots: Vec::new(),
            cache_dir: None,
            loc_languages: None,
            ignore_file_patterns: Vec::new(),
            ignore_dir_patterns: Vec::new(),
            ignored_error_codes: Vec::new(),
            rules_dir: None,
            scope_checks,
            var_checks,
            background_reindex_interval_minutes: 0,
            background_reindex_idle_seconds: 15,
            workspace_wide_diagnostics: true,
            position_encoding: tower_lsp::lsp_types::PositionEncodingKind::UTF16,
            formatting: cwtools_parser::format::FormatOptions::default(),
        }
    }

    pub(crate) fn game(&self) -> Option<cwtools_game::constants::Game> {
        cwtools_game::constants::Game::from_str(&self.language)
    }

    pub(crate) fn refresh_roots(&mut self) {
        let editable: Vec<std::path::PathBuf> = self
            .workspace_roots
            .iter()
            .filter_map(|root| std::fs::canonicalize(root).ok())
            .collect();
        let vanilla = self
            .vanilla_dir
            .iter()
            .filter_map(|root| std::fs::canonicalize(root).ok());
        let parents = self
            .parent_roots
            .iter()
            .filter_map(|root| std::fs::canonicalize(root).ok());
        let rules = self
            .rules_dir
            .iter()
            .filter_map(|root| std::fs::canonicalize(root).ok())
            .filter(|root| !editable.iter().any(|workspace| workspace.starts_with(root)));
        self.authorized_roots = editable
            .iter()
            .cloned()
            .chain(vanilla)
            .chain(parents)
            .chain(rules)
            .collect();
        self.editable_roots = editable.into();
    }
}

pub(crate) struct RuleData {
    pub(crate) ruleset: Option<Arc<RuleSet>>,
    pub(crate) scope_registry: Option<Arc<cwtools_game::scope_registry::ScopeRegistry>>,
    /// Cached modifier-key set; rebuilt after ruleset load and after each full workspace
    /// scan when the type index is complete. `Arc` so the workspace scan snapshots it with a
    /// cheap refcount bump instead of deep-copying the whole set (#78).
    pub(crate) modifier_keys: Arc<HashSet<String>>,
    pub(crate) modifier_scopes: Arc<HashMap<String, Vec<String>>>,
}

impl RuleData {
    pub(crate) fn new() -> Self {
        Self {
            ruleset: None,
            scope_registry: None,
            modifier_keys: Arc::new(HashSet::new()),
            modifier_scopes: Arc::new(HashMap::new()),
        }
    }

    /// Store modifier data built from `snapshot`, unless the ruleset was replaced since.
    pub(crate) fn store_modifier_data(
        &mut self,
        snapshot: Option<&Arc<RuleSet>>,
        keys: HashSet<String>,
        scopes: HashMap<String, Vec<String>>,
    ) {
        if self.ruleset.as_ref().map(Arc::as_ptr) == snapshot.map(Arc::as_ptr) {
            self.modifier_keys = Arc::new(keys);
            self.modifier_scopes = Arc::new(scopes);
        }
    }
}

/// Mutable base-game payload. Its pieces are staged and consumed at different
/// times, so a merge takes only the pending index while retaining the auxiliary
/// data for later workspace and localization rebuilds. Retained payloads are
/// shared through `Arc` so merges consume them without deep clones (#1154).
#[derive(Default)]
pub(crate) struct VanillaState {
    #[allow(clippy::type_complexity)]
    pub(crate) index: Option<HashMap<String, Vec<(Arc<str>, TypeInstance)>>>,
    /// URIs from the most recent merge. Kept through clear/reload so the next
    /// merge can remove their previous provenance from the type index.
    pub(crate) merged_uris: HashSet<Arc<str>>,
    #[allow(clippy::type_complexity)]
    pub(crate) loc_keys: Option<Vec<(String, Vec<String>)>>,
    pub(crate) file_paths: Option<Arc<Vec<String>>>,
    pub(crate) var_names: Option<Arc<Vec<String>>>,
    pub(crate) scripted_loc_names: Option<Arc<Vec<String>>>,
    pub(crate) scripted_gui_names: Option<Arc<Vec<String>>>,
}

/// LOCK ORDER: snapshot or take `vanilla_state` under its short-lived guard,
/// then release it before acquiring `config` or `info_service` or doing I/O.
/// Other multi-lock paths document their order at their use sites.
pub(crate) struct DocumentState {
    /// Orders versioned diagnostic sends against document version changes. A
    /// validation may finish while didChange is queued, so the version check
    /// and its client send must be one serialized operation.
    pub(crate) diagnostic_publication_lock: tokio::sync::Mutex<()>,
    pub(crate) baseline: Mutex<crate::baseline_runtime::State>,
    pub(crate) documents: Mutex<DocumentStore>,
    pub(crate) config: parking_lot::RwLock<Config>,
    pub(crate) workspace_roots_generation: AtomicU64,
    pub(crate) rules: parking_lot::RwLock<RuleData>,
    /// Held across a whole `rebuild_modifier_keys` so rebuilds store in the order they start.
    /// Only that function takes it, outside every other lock.
    pub(crate) modifier_rebuild_lock: parking_lot::Mutex<()>,
    pub(crate) string_table: StringTable,
    pub(crate) info_service: parking_lot::RwLock<cwtools_info::InfoService>,
    pub(crate) vanilla_state: Mutex<VanillaState>,
    /// The base-game install's loc maps, reused for the session (#89): vanilla loc cannot
    /// change while the editor runs. Dropped by `cacheVanilla` / `clearAllCaches`, which is
    /// how a user who updated the game picks up the new files.
    #[allow(clippy::type_complexity)]
    pub(crate) vanilla_loc:
        Mutex<Option<(crate::scan::VanillaLocKey, Arc<crate::scan::VanillaLoc>)>>,
    /// Parent-mod loc (#786), rebuilt only when the parent files themselves
    /// change, not on every workspace loc edit.
    #[allow(clippy::type_complexity)]
    pub(crate) parent_loc: Mutex<Option<(crate::scan::ParentLocKey, Arc<crate::scan::VanillaLoc>)>>,
    pub(crate) loc_index: parking_lot::RwLock<Option<Arc<cwtools_localization::LocIndex>>>,
    /// The mod's `common/inline_scripts` bodies, so an `inline_script` call site expands and
    /// validates against them instead of standing unchecked (#259). The editor's counterpart
    /// of the batch driver's `Session::inline_scripts`, rebuilt wholesale on each full scan.
    pub(crate) inline_scripts: parking_lot::RwLock<InlineScripts>,
    pub(crate) loc_key_index: parking_lot::RwLock<Option<Arc<crate::completion::LocKeyIndex>>>,
    pub(crate) loc_text: parking_lot::RwLock<LocText>,
    pub(crate) loc_locations: parking_lot::RwLock<LocLocations>,
    /// Live loc keys (lowercased) of currently-open loc files, keyed by URI. Overlays the
    /// scanned `loc_index` so a key added to an open `.yml` resolves in `$ref$` checks
    /// without a full rescan (#36). It only adds keys, never subtracts from the baseline.
    pub(crate) loc_live_overlay: parking_lot::RwLock<HashMap<String, HashSet<String>>>,
    pub(crate) loc_watched_overlay: parking_lot::RwLock<HashMap<String, HashSet<String>>>,
    pub(crate) loc_overlay_revision: AtomicU64,
    pub(crate) loc_overlay_keys_cache: parking_lot::Mutex<Option<(u64, Arc<HashSet<String>>)>>,
    pub(crate) loc_ref_names_cache: parking_lot::Mutex<Option<(u64, Arc<HashSet<String>>)>>,
    pub(crate) hover_show_all_languages: std::sync::atomic::AtomicBool,
    pub(crate) hover_debug: std::sync::atomic::AtomicBool,
    /// When `true` (the `hover.scopeDisplay = "resolved"` setting), hover adds a `Resolves to`
    /// line showing the scope the hovered link/keyword evaluates to, alongside the ambient
    /// current scope. Off by default, where the ambient scope is shown alone (#37).
    pub(crate) hover_resolved_scope: std::sync::atomic::AtomicBool,
    pub(crate) inlay_hints_loc_titles: std::sync::atomic::AtomicBool,
    pub(crate) inlay_hints_scopes: std::sync::atomic::AtomicBool,
    pub(crate) hierarchical_symbols: std::sync::atomic::AtomicBool,
    pub(crate) workspace_edit_document_changes: std::sync::atomic::AtomicBool,
    pub(crate) completion_label_details: std::sync::atomic::AtomicBool,
    pub(crate) client_work_done_progress: std::sync::atomic::AtomicBool,
    pub(crate) semantic_tokens_refresh_support: std::sync::atomic::AtomicBool,
    pub(crate) code_lens_refresh_support: std::sync::atomic::AtomicBool,
    pub(crate) watched_files_dynamic_registration: std::sync::atomic::AtomicBool,
    pub(crate) scan_progress_active: std::sync::atomic::AtomicBool,
    pub(crate) command_cancels: parking_lot::Mutex<HashMap<String, Arc<AtomicBool>>>,
    pub(crate) loading_bar_active: std::sync::atomic::AtomicBool,
    pub(crate) index_ready: std::sync::atomic::AtomicBool,
    pub(crate) handshake_complete: std::sync::atomic::AtomicBool,
    /// Broken-`.cwt` diagnostics, parked because the rules load runs inside `initialize`
    /// where the gate above would swallow them (#98).
    pub(crate) deferred_rule_diagnostics: parking_lot::Mutex<Vec<(String, Vec<Diagnostic>)>>,
    /// The rules load's user-visible messages (per-error log lines, the error toast), parked
    /// the same way as the diagnostics above (#98).
    pub(crate) deferred_rules_messages: parking_lot::Mutex<Vec<DeferredRulesMessage>>,
    pub(crate) last_rules_toast: parking_lot::Mutex<Option<String>>,
    /// Unclosed-quote format refusals already shown, so format on save does not repeat the toast.
    pub(crate) format_refusal_toasts: parking_lot::Mutex<HashSet<(String, u32)>>,
    /// Document open/change refusals already warned about, keyed by URI and reason, so one
    /// refusal toasts once (#1137). An entry is dropped when the URI is admitted or closed.
    pub(crate) document_refusal_toasts: parking_lot::Mutex<HashSet<(String, &'static str)>>,
    pub(crate) published_rule_uris: parking_lot::Mutex<HashSet<String>>,
    pub(crate) published_loc_uris: parking_lot::Mutex<HashSet<String>>,
    pub(crate) edit_generation: AtomicU64,
    /// Case-folded content hashes, not `StringId`s: a token interned into a
    /// document's mid-edit overlay and the same name interned into the base table
    /// are different ids, so the dependent sweep would silently skip the document
    /// (#475).
    pub(crate) doc_tokens: parking_lot::RwLock<HashMap<String, HashSet<u64>>>,
    pub(crate) pending_changed_names: Mutex<HashSet<String>>,
    pub(crate) type_uses: parking_lot::RwLock<HashMap<String, references::UsedInstances>>,
    #[allow(clippy::type_complexity)]
    pub(crate) type_uses_merged: parking_lot::Mutex<Option<(u64, Arc<references::UsedInstances>)>>,
    pub(crate) type_uses_revision: AtomicU64,
    pub(crate) vanilla_merged: std::sync::atomic::AtomicBool,
    pub(crate) scan_in_progress: AtomicBool,
    /// Set by `shutdown` (#1146); the watch also wakes the reindex loop's sleeps.
    pub(crate) shutdown: tokio::sync::watch::Sender<bool>,
    pub(crate) debounce_handles: Mutex<HashMap<String, DebounceTask>>,
    pub(crate) next_debounce_id: AtomicU64,
    pub(crate) validation_permits: tokio::sync::Semaphore,
    pub(crate) info_revision: AtomicU64,
    pub(crate) fallback_cache: parking_lot::Mutex<Option<CompletionCacheEntry>>,
    #[allow(clippy::type_complexity)]
    pub(crate) fresh_ast_cache: parking_lot::Mutex<Option<(String, i32, Arc<ParsedFile>)>>,
    pub(crate) completion_generation: parking_lot::Mutex<HashMap<String, u64>>,
    pub(crate) next_completion_id: AtomicU64,
    pub(crate) last_loc_signature: parking_lot::Mutex<Option<u64>>,
    #[allow(clippy::type_complexity)]
    pub(crate) loc_discovery_cache:
        parking_lot::Mutex<Option<(std::path::PathBuf, Vec<std::path::PathBuf>, Option<u64>)>>,
    pub(crate) last_scan_fingerprint: parking_lot::Mutex<Option<(u64, u64)>>,
    #[cfg(test)]
    pub(crate) hold_gate: parking_lot::Mutex<Option<Arc<HoldGate>>>,
    #[cfg(test)]
    pub(crate) navigation_snapshot_budget_override: parking_lot::Mutex<Option<usize>>,
    #[cfg(test)]
    pub(crate) workspace_scan_budget_override: parking_lot::Mutex<Option<u64>>,
    #[cfg(test)]
    pub(crate) scan_source_reads: std::sync::atomic::AtomicUsize,
    pub(crate) settings_generation: AtomicU64,
    pub(crate) start: std::time::Instant,
    pub(crate) last_activity_ms: AtomicU64,
    /// URIs of non-open watched files (create/modify) waiting for the next coalescing window,
    /// so a burst of `didChangeWatchedFiles` events collapses into one drain (#90).
    pub(crate) watched_pending: Mutex<HashSet<String>>,
    /// The `watched_pending` URIs that arrived as CREATED. Only these can be a moved-in
    /// folder to expand; a folder reported CHANGED is not walked.
    pub(crate) watched_created: Mutex<HashSet<String>>,
    pub(crate) watched_deleted: Mutex<HashSet<String>>,
    pub(crate) watched_debounce: Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub(crate) watched_signatures: Mutex<HashMap<String, (u64, u128)>>,
    /// Per-URI cached semantic tokens for `full/delta` support. `None` when never requested
    /// or after invalidation (file change, rename, encoding switch, rules reload). Bounded by
    /// distinct URIs that requested tokens.
    pub(crate) semantic_tokens_cache: Mutex<HashMap<String, SemanticCacheEntry>>,
    pub(crate) semantic_tokens_seq: AtomicU64,
    pub(crate) fixable_edits: Mutex<HashMap<String, FixableEdits>>,
    pub(crate) last_scan_summary: Mutex<Option<ScanSummary>>,
    pub(crate) published_workspace_uris: Mutex<HashSet<String>>,
    /// Document and workspace notifications run here rather than on the message
    /// pump, in the order the client sent them (#470).
    pub(crate) notifications: NotificationQueue,
}

pub(crate) struct LocOverlayWrite<'a> {
    pub(crate) guard: parking_lot::RwLockWriteGuard<'a, HashMap<String, HashSet<String>>>,
    pub(crate) revision: &'a AtomicU64,
}

impl std::ops::Deref for LocOverlayWrite<'_> {
    type Target = HashMap<String, HashSet<String>>;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl std::ops::DerefMut for LocOverlayWrite<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl Drop for LocOverlayWrite<'_> {
    fn drop(&mut self) {
        self.revision.fetch_add(1, Ordering::AcqRel);
    }
}

pub(crate) struct CompletionCacheEntry {
    pub(crate) revision: u64,
    pub(crate) items: Vec<CompletionItem>,
}

pub(crate) const MAX_DOCUMENT_URI_BYTES: usize = 8 * 1024;
pub(crate) const MAX_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_OPEN_DOCUMENTS: usize = 128;
pub(crate) const MAX_RETAINED_DOCUMENT_BYTES: usize = 128 * 1024 * 1024;
/// Refusal records kept at once; past this a refusal is only logged (#1137).
pub(crate) const MAX_DOCUMENT_REFUSAL_TOASTS: usize = 128;
/// Slots in `validation_permits`, each held for one file's parse+validate plus
/// the sweep it triggers (`Backend::validation_permit`). Two is enough to keep
/// one keystroke validation moving while a bulk path works through its files.
const MAX_CONCURRENT_VALIDATIONS: usize = 2;

pub(crate) struct DocumentStore {
    documents: HashMap<String, ParsedDoc>,
    content_hashes: HashMap<String, u64>,
    open_use_sites: OpenUseSiteIndex,
    line_indices: HashMap<String, Arc<LineIndex>>,
    pub(crate) retained_text_bytes: usize,
}

#[derive(Default)]
struct OpenUseSiteIndex {
    by_key: HashMap<(String, String), HashMap<String, Vec<OpenUseSite>>>,
    file_keys: HashMap<String, Vec<(String, String)>>,
    file_bytes: HashMap<String, usize>,
    capacity_bytes: usize,
}

#[derive(Clone)]
struct OpenUseSite {
    name: String,
    case_insensitive: bool,
    site: UseSite,
}

impl OpenUseSiteIndex {
    fn string_arc_bytes(len: usize) -> usize {
        let alignment = std::mem::align_of::<usize>();
        let size = std::mem::size_of::<usize>() * 2 + len;
        size.saturating_add(alignment - 1) / alignment * alignment
    }

    fn hash_bytes<K, V>(capacity: usize) -> usize {
        capacity
            .saturating_mul(4)
            .saturating_mul(std::mem::size_of::<(K, V)>())
    }

    fn vec_bytes<T>(capacity: usize) -> usize {
        capacity
            .saturating_mul(4)
            .saturating_mul(std::mem::size_of::<T>())
    }

    fn shrink_hash<K: Eq + std::hash::Hash, V>(map: &mut HashMap<K, V>) {
        if map.capacity() > map.len().saturating_mul(3) {
            map.shrink_to_fit();
        }
    }

    fn shrink_vec<T>(values: &mut Vec<T>) {
        if values.capacity() > values.len().saturating_mul(3) {
            values.shrink_to_fit();
        }
    }

    fn retained_bytes_for_sites(uri: &str, sites: &[IndexedUseSite]) -> usize {
        if sites.is_empty() {
            return 0;
        }
        let mut bytes = uri.len().saturating_mul(2);
        let mut keys = HashSet::new();
        for site in sites {
            bytes = bytes
                .saturating_add(site.name.capacity())
                .saturating_add(Self::string_arc_bytes(uri.len()));
            let lower = site.name.to_ascii_lowercase();
            let key = (site.type_name.clone(), lower.clone());
            if keys.insert(key) {
                bytes = bytes
                    .saturating_add(site.type_name.capacity())
                    .saturating_add(site.type_name.len())
                    .saturating_add(lower.capacity())
                    .saturating_add(lower.len())
                    .saturating_add(uri.len());
            }
        }
        bytes
    }

    fn capacity_bound(key_count: usize, site_count: usize) -> usize {
        let buckets = key_count.saturating_mul(4);
        let sites = site_count.saturating_mul(4);
        Self::hash_bytes::<String, usize>(4)
            .saturating_add(Self::hash_bytes::<String, Vec<(String, String)>>(4))
            .saturating_add(Self::hash_bytes::<
                (String, String),
                HashMap<String, Vec<OpenUseSite>>,
            >(buckets))
            .saturating_add(Self::hash_bytes::<String, Vec<OpenUseSite>>(buckets))
            .saturating_add(Self::vec_bytes::<(String, String)>(buckets))
            .saturating_add(Self::vec_bytes::<OpenUseSite>(sites))
    }

    fn capacity_bound_for_sites(sites: &[IndexedUseSite]) -> usize {
        let mut keys = HashSet::new();
        for site in sites {
            keys.insert((site.type_name.as_str(), site.name.to_ascii_lowercase()));
        }
        Self::capacity_bound(keys.len(), sites.len())
    }

    fn capacity_bound_for_file(&self, uri: &str) -> usize {
        let Some(keys) = self.file_keys.get(uri) else {
            return 0;
        };
        let site_count = keys
            .iter()
            .filter_map(|key| self.by_key.get(key).and_then(|files| files.get(uri)))
            .map(Vec::len)
            .sum();
        Self::capacity_bound(keys.len(), site_count)
    }

    fn replacement_capacity_bound(&self, uri: &str, sites: &[IndexedUseSite]) -> usize {
        self.capacity_bytes.saturating_add(
            Self::capacity_bound_for_sites(sites).saturating_sub(self.capacity_bound_for_file(uri)),
        )
    }

    fn file_bytes(&self, uri: &str) -> usize {
        self.file_bytes.get(uri).copied().unwrap_or(0)
    }

    fn capacity_bytes(&self) -> usize {
        self.capacity_bytes
    }

    fn touched_keys(&self, uri: &str) -> HashSet<(String, String)> {
        self.file_keys
            .get(uri)
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    }

    fn touched_capacity(&self, uri: &str, keys: &HashSet<(String, String)>) -> usize {
        let mut bytes = Self::hash_bytes::<String, usize>(self.file_bytes.capacity())
            .saturating_add(Self::hash_bytes::<String, Vec<(String, String)>>(
                self.file_keys.capacity(),
            ))
            .saturating_add(Self::hash_bytes::<
                (String, String),
                HashMap<String, Vec<OpenUseSite>>,
            >(self.by_key.capacity()));
        if let Some(file_keys) = self.file_keys.get(uri) {
            bytes = bytes.saturating_add(Self::vec_bytes::<(String, String)>(file_keys.capacity()));
        }
        for key in keys {
            if let Some(files) = self.by_key.get(key) {
                bytes = bytes.saturating_add(Self::hash_bytes::<String, Vec<OpenUseSite>>(
                    files.capacity(),
                ));
                for sites in files.values() {
                    bytes = bytes.saturating_add(Self::vec_bytes::<OpenUseSite>(sites.capacity()));
                }
            }
        }
        bytes
    }

    fn shrink_touched(&mut self, uri: &str, keys: &HashSet<(String, String)>) {
        if let Some(file_keys) = self.file_keys.get_mut(uri) {
            Self::shrink_vec(file_keys);
        }
        for key in keys {
            if let Some(files) = self.by_key.get_mut(key) {
                for sites in files.values_mut() {
                    Self::shrink_vec(sites);
                }
                Self::shrink_hash(files);
            }
        }
        Self::shrink_hash(&mut self.by_key);
        Self::shrink_hash(&mut self.file_keys);
        Self::shrink_hash(&mut self.file_bytes);
    }

    fn replace_file(&mut self, uri: &str, sites: Vec<IndexedUseSite>) {
        self.remove_file(uri);
        let keys = sites
            .iter()
            .map(|site| (site.type_name.clone(), site.name.to_ascii_lowercase()))
            .collect();
        let before = self.touched_capacity(uri, &keys);
        let retained_bytes = Self::retained_bytes_for_sites(uri, &sites);
        let mut seen = HashSet::new();
        for IndexedUseSite {
            type_name,
            name,
            case_insensitive,
            site,
        } in sites
        {
            let key = (type_name, name.to_ascii_lowercase());
            let file_keys = self.file_keys.entry(uri.to_string()).or_default();
            if seen.insert(key.clone()) {
                file_keys.push(key.clone());
            }
            self.by_key
                .entry(key)
                .or_default()
                .entry(uri.to_string())
                .or_default()
                .push(OpenUseSite {
                    name,
                    case_insensitive,
                    site,
                });
        }
        if retained_bytes > 0 {
            self.file_bytes.insert(uri.to_string(), retained_bytes);
        }
        self.shrink_touched(uri, &keys);
        let after = self.touched_capacity(uri, &keys);
        self.capacity_bytes = self
            .capacity_bytes
            .saturating_sub(before)
            .saturating_add(after);
    }

    fn remove_file(&mut self, uri: &str) {
        let keys = self.touched_keys(uri);
        if keys.is_empty() && !self.file_keys.contains_key(uri) {
            return;
        }
        let before = self.touched_capacity(uri, &keys);
        self.file_keys.remove(uri);
        self.file_bytes.remove(uri);
        for key in &keys {
            if let Some(files) = self.by_key.get_mut(key) {
                files.remove(uri);
                if files.is_empty() {
                    self.by_key.remove(key);
                }
            }
        }
        self.shrink_touched(uri, &keys);
        let after = self.touched_capacity(uri, &keys);
        self.capacity_bytes = self
            .capacity_bytes
            .saturating_sub(before)
            .saturating_add(after);
    }

    fn references(&self, type_name: &str, name: &str) -> Vec<UseSite> {
        let key = (type_name.to_string(), name.to_ascii_lowercase());
        self.by_key
            .get(&key)
            .into_iter()
            .flat_map(HashMap::values)
            .flatten()
            .filter(|site| site.case_insensitive || site.name == name)
            .map(|site| site.site.clone())
            .collect()
    }
}

/// Byte starts for each physical source line. Kept with the open document so
/// range semantic-token requests can jump directly to their first line.
#[derive(Debug)]
pub struct LineIndex {
    starts: Vec<u32>,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut starts = Vec::new();
        starts.push(0);
        for (byte, value) in text.bytes().enumerate() {
            if value == b'\n' {
                starts.push((byte + 1) as u32);
            }
        }
        Self { starts }
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.starts.capacity() * std::mem::size_of::<u32>()
    }

    pub(crate) fn line_count(&self) -> u32 {
        self.starts.len() as u32
    }

    pub(crate) fn line<'a>(&self, text: &'a str, line: u32) -> &'a str {
        let Some(&start) = self.starts.get(line as usize) else {
            return "";
        };
        let end = self
            .starts
            .get(line as usize + 1)
            .copied()
            .unwrap_or(text.len() as u32) as usize;
        let mut end = if end > start as usize && text.as_bytes()[end - 1] == b'\n' {
            end - 1
        } else {
            end
        };
        if end > start as usize && text.as_bytes()[end - 1] == b'\r' {
            end -= 1;
        }
        &text[start as usize..end]
    }
}

impl DocumentStore {
    pub(crate) fn new() -> Self {
        Self {
            documents: HashMap::new(),
            content_hashes: HashMap::new(),
            open_use_sites: OpenUseSiteIndex::default(),
            line_indices: HashMap::new(),
            retained_text_bytes: 0,
        }
    }

    pub(crate) fn content_hash(&self, uri: &str) -> Option<u64> {
        self.content_hashes.get(uri).copied()
    }

    /// The open buffer's text as a refcount clone. `ParsedDoc.text` is an
    /// `Arc<str>` precisely so a reader that only looks at the text does not
    /// copy the document; every such read goes through here.
    pub(crate) fn text_of(&self, uri: &str) -> Option<Arc<str>> {
        self.documents
            .get(uri)
            .map(|document| document.text.clone())
    }

    /// The open buffer shared by reference with its stored hash, so a reader
    /// neither copies nor rehashes the text. Text and hash are only ever
    /// written together (`open_with_hash`, `change_with_hash`).
    pub(crate) fn text_snapshot(&self, uri: &str) -> Option<FileTextSnapshot> {
        let document = self.documents.get(uri)?;
        Some(FileTextSnapshot {
            text: document.text.clone(),
            version: Some(document.version),
            content_hash: self.content_hash(uri)?,
        })
    }

    #[cfg(test)]
    pub(crate) fn line_index_of(&self, uri: &str) -> Option<Arc<LineIndex>> {
        self.line_indices.get(uri).cloned()
    }

    fn line_index_bytes(&self, uri: &str) -> usize {
        self.line_indices
            .get(uri)
            .map_or(0, |index| index.retained_bytes())
    }

    /// Return the cached line index only when it belongs to the exact text
    /// snapshot supplied by the caller. The document and index are checked
    /// under the same store lock so concurrent changes cannot pair snapshots.
    pub(crate) fn line_index_for_text(&self, uri: &str, text: &Arc<str>) -> Option<Arc<LineIndex>> {
        let document = self.documents.get(uri)?;
        if !Arc::ptr_eq(&document.text, text) {
            return None;
        }
        self.line_indices.get(uri).cloned()
    }

    #[cfg(test)]
    pub(crate) fn open(
        &mut self,
        uri: String,
        document: ParsedDoc,
    ) -> std::result::Result<(), DocumentRejection> {
        let content_hash = cwtools_cache::workspace::content_hash(&document.text);
        self.open_with_hash(uri, document, content_hash)
    }

    pub(crate) fn open_with_hash(
        &mut self,
        uri: String,
        document: ParsedDoc,
        content_hash: u64,
    ) -> std::result::Result<(), DocumentRejection> {
        let old_document_bytes = self
            .documents
            .get(&uri)
            .map_or(0, ParsedDoc::retained_bytes)
            .saturating_add(self.line_index_bytes(&uri));
        let old_index_bytes = self
            .open_use_sites
            .file_bytes(&uri)
            .saturating_add(self.open_use_sites.capacity_bytes());
        if !self.documents.contains_key(&uri) && self.documents.len() >= MAX_OPEN_DOCUMENTS {
            return Err(DocumentRejection::TooManyOpen);
        }
        let line_index = Arc::new(LineIndex::new(&document.text));
        let new_document_bytes = document
            .retained_bytes()
            .saturating_add(line_index.retained_bytes());
        self.replacement_total(
            old_document_bytes.saturating_add(old_index_bytes),
            new_document_bytes.saturating_add(self.open_use_sites.capacity_bytes()),
        )?;
        self.open_use_sites.remove_file(&uri);
        let new_index_bytes = self.open_use_sites.capacity_bytes();
        self.documents.insert(uri.clone(), document);
        self.line_indices.insert(uri.clone(), line_index);
        self.content_hashes.insert(uri, content_hash);
        self.retained_text_bytes = self
            .retained_text_bytes
            .saturating_sub(old_document_bytes.saturating_add(old_index_bytes))
            .saturating_add(new_document_bytes.saturating_add(new_index_bytes));
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn change(
        &mut self,
        uri: &str,
        version: i32,
        text: Arc<str>,
    ) -> std::result::Result<(), DocumentRejection> {
        let content_hash = cwtools_cache::workspace::content_hash(&text);
        self.change_with_hash(uri, version, text, content_hash)
    }

    pub(crate) fn change_with_hash(
        &mut self,
        uri: &str,
        version: i32,
        text: Arc<str>,
        content_hash: u64,
    ) -> std::result::Result<(), DocumentRejection> {
        let Some(document) = self.documents.get(uri) else {
            return Err(DocumentRejection::NotOpen);
        };
        let index_bytes = self
            .open_use_sites
            .file_bytes(uri)
            .saturating_add(self.open_use_sites.capacity_bytes());
        let old_len = document
            .retained_bytes()
            .saturating_add(self.line_index_bytes(uri))
            .saturating_add(index_bytes);
        let line_index = Arc::new(LineIndex::new(&text));
        let new_len = text
            .len()
            .max(document.ast_source_bytes)
            .saturating_add(line_index.retained_bytes())
            .saturating_add(index_bytes);
        self.replacement_total(old_len, new_len)?;
        let Some(document) = self.documents.get_mut(uri) else {
            return Err(DocumentRejection::NotOpen);
        };
        document.version = version;
        document.text = text;
        document.loc_cache = None;
        self.line_indices.insert(uri.to_string(), line_index);
        self.content_hashes.insert(uri.to_string(), content_hash);
        self.retained_text_bytes = self
            .retained_text_bytes
            .saturating_sub(old_len)
            .saturating_add(new_len);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_ast(&mut self, uri: &str, version: i32, ast: Arc<ParsedFile>) -> bool {
        self.set_ast_with_use_sites(uri, version, ast, Vec::new())
    }

    pub(crate) fn set_ast_with_use_sites(
        &mut self,
        uri: &str,
        version: i32,
        ast: Arc<ParsedFile>,
        use_sites: Vec<IndexedUseSite>,
    ) -> bool {
        let Some(document) = self.documents.get(uri) else {
            return false;
        };
        if document.version != version {
            return false;
        }
        let line_index_bytes = self.line_index_bytes(uri);
        let old_document_bytes = document.retained_bytes().saturating_add(line_index_bytes);
        let old_file_bytes = self.open_use_sites.file_bytes(uri);
        let old_index_bytes = old_file_bytes.saturating_add(self.open_use_sites.capacity_bytes());
        let new_document_bytes = document
            .text
            .len()
            .saturating_add(
                document
                    .loc_cache
                    .as_ref()
                    .map_or(0, |cache| cache.retained_bytes),
            )
            .saturating_add(line_index_bytes);
        let new_file_bytes = OpenUseSiteIndex::retained_bytes_for_sites(uri, &use_sites);
        let new_capacity_bytes = self
            .open_use_sites
            .replacement_capacity_bound(uri, &use_sites);
        if self
            .replacement_total_with_capacity(
                old_document_bytes.saturating_add(old_index_bytes),
                new_document_bytes
                    .saturating_add(new_file_bytes)
                    .saturating_add(new_capacity_bytes),
                new_capacity_bytes,
            )
            .is_err()
        {
            return false;
        }
        let Some(document) = self.documents.get_mut(uri) else {
            return false;
        };
        document.ast = Some(ast);
        document.ast_version = Some(version);
        document.ast_source_bytes = document.text.len();
        self.open_use_sites.replace_file(uri, use_sites);
        let new_index_bytes = self
            .open_use_sites
            .file_bytes(uri)
            .saturating_add(self.open_use_sites.capacity_bytes());
        self.retained_text_bytes = self
            .retained_text_bytes
            .saturating_sub(old_document_bytes.saturating_add(old_index_bytes))
            .saturating_add(new_document_bytes.saturating_add(new_index_bytes));
        true
    }

    pub(crate) fn replace_reference_sites(
        &mut self,
        uri: &str,
        version: i32,
        use_sites: Vec<IndexedUseSite>,
    ) -> bool {
        let Some(document) = self.documents.get(uri) else {
            return false;
        };
        if document.version != version || document.ast_version != Some(version) {
            return false;
        }
        let document_bytes = document
            .retained_bytes()
            .saturating_add(self.line_index_bytes(uri));
        let old_file_bytes = self.open_use_sites.file_bytes(uri);
        let old_index_bytes = old_file_bytes.saturating_add(self.open_use_sites.capacity_bytes());
        let new_file_bytes = OpenUseSiteIndex::retained_bytes_for_sites(uri, &use_sites);
        let new_capacity_bytes = self
            .open_use_sites
            .replacement_capacity_bound(uri, &use_sites);
        if self
            .replacement_total_with_capacity(
                document_bytes.saturating_add(old_index_bytes),
                document_bytes
                    .saturating_add(new_file_bytes)
                    .saturating_add(new_capacity_bytes),
                new_capacity_bytes,
            )
            .is_err()
        {
            return false;
        }
        self.open_use_sites.replace_file(uri, use_sites);
        let new_index_bytes = self
            .open_use_sites
            .file_bytes(uri)
            .saturating_add(self.open_use_sites.capacity_bytes());
        self.retained_text_bytes = self
            .retained_text_bytes
            .saturating_sub(document_bytes.saturating_add(old_index_bytes))
            .saturating_add(document_bytes.saturating_add(new_index_bytes));
        true
    }

    pub(crate) fn reference_sites(&self, type_name: &str, name: &str) -> Vec<UseSite> {
        self.open_use_sites.references(type_name, name)
    }

    pub(crate) fn set_loc_cache(
        &mut self,
        uri: &str,
        version: i32,
        cache: Arc<LocDocumentCache>,
    ) -> std::result::Result<bool, DocumentRejection> {
        let Some(document) = self.documents.get(uri) else {
            return Ok(false);
        };
        if document.version != version || cache.version != version {
            return Ok(false);
        }
        let index_bytes = self
            .open_use_sites
            .file_bytes(uri)
            .saturating_add(self.open_use_sites.capacity_bytes())
            .saturating_add(self.line_index_bytes(uri));
        let old_len = document.retained_bytes().saturating_add(index_bytes);
        let new_len = document
            .text
            .len()
            .max(document.ast_source_bytes)
            .saturating_add(cache.retained_bytes)
            .saturating_add(index_bytes);
        self.replacement_total(old_len, new_len)?;
        let Some(document) = self.documents.get_mut(uri) else {
            return Ok(false);
        };
        document.loc_cache = Some(cache);
        self.retained_text_bytes = self
            .retained_text_bytes
            .saturating_sub(old_len)
            .saturating_add(new_len);
        Ok(true)
    }

    pub(crate) fn remove(&mut self, uri: &str) -> Option<ParsedDoc> {
        let document = self.documents.remove(uri)?;
        let old_index_bytes = self
            .open_use_sites
            .file_bytes(uri)
            .saturating_add(self.open_use_sites.capacity_bytes());
        self.open_use_sites.remove_file(uri);
        let new_index_bytes = self.open_use_sites.capacity_bytes();
        self.content_hashes.remove(uri);
        let line_index_bytes = self
            .line_indices
            .remove(uri)
            .map_or(0, |index| index.retained_bytes());
        self.retained_text_bytes = self
            .retained_text_bytes
            .saturating_sub(
                document
                    .retained_bytes()
                    .saturating_add(old_index_bytes)
                    .saturating_add(line_index_bytes),
            )
            .saturating_add(new_index_bytes);
        Some(document)
    }

    pub(crate) fn replacement_total(
        &self,
        old_len: usize,
        new_len: usize,
    ) -> std::result::Result<usize, DocumentRejection> {
        self.replacement_total_with_capacity(old_len, new_len, self.open_use_sites.capacity_bytes())
    }

    fn replacement_total_with_capacity(
        &self,
        old_len: usize,
        new_len: usize,
        capacity_bytes: usize,
    ) -> std::result::Result<usize, DocumentRejection> {
        let document_bytes = new_len
            .checked_sub(capacity_bytes)
            .ok_or(DocumentRejection::RetainedTextLimit)?;
        if document_bytes > MAX_DOCUMENT_BYTES {
            return Err(DocumentRejection::TooLarge);
        }
        let retained = self
            .retained_text_bytes
            .checked_sub(old_len)
            .and_then(|bytes| bytes.checked_add(new_len))
            .ok_or(DocumentRejection::RetainedTextLimit)?;
        if retained > MAX_RETAINED_DOCUMENT_BYTES {
            return Err(DocumentRejection::RetainedTextLimit);
        }
        Ok(retained)
    }
}

impl std::ops::Deref for DocumentStore {
    type Target = HashMap<String, ParsedDoc>;

    fn deref(&self) -> &Self::Target {
        &self.documents
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DocumentRejection {
    NotOpen,
    OutsideWorkspace,
    TooLarge,
    TooManyOpen,
    RetainedTextLimit,
}

impl DocumentRejection {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::NotOpen => "the document is not open",
            Self::OutsideWorkspace => "the document is outside the workspace folders",
            Self::TooLarge => "the document exceeds the per-document byte limit",
            Self::TooManyOpen => "the open-document count limit was reached",
            Self::RetainedTextLimit => "the retained open-document byte limit was reached",
        }
    }

    /// Opening a base-game or parent-mod file is routine (go-to-definition), and a
    /// change to a document that is not open follows a refusal already reported,
    /// so those two are only logged (#1137).
    pub(crate) fn warns_user(self) -> bool {
        !matches!(self, Self::NotOpen | Self::OutsideWorkspace)
    }
}

#[cfg(test)]
mod semantic_line_index_tests {
    use super::*;

    fn document(text: &str) -> ParsedDoc {
        ParsedDoc {
            version: 1,
            text: Arc::from(text),
            ast: None,
            ast_version: None,
            ast_source_bytes: 0,
            loc_cache: None,
        }
    }

    #[test]
    fn line_index_matches_source_lines_for_empty_newline_crlf_and_unicode_text() {
        for text in ["", "one line", "one line\n", "a\r\nβ😀\r\nlast"] {
            let index = LineIndex::new(text);
            for (line, expected) in text.lines().enumerate() {
                assert_eq!(
                    index.line(text, line as u32),
                    expected,
                    "{text:?} line {line}"
                );
            }
            if text.ends_with('\n') {
                assert_eq!(index.line(text, text.lines().count() as u32), "");
            }
        }
    }

    #[test]
    fn document_store_accounts_for_line_and_reference_indexes_together() {
        let mut docs = DocumentStore::new();
        let uri = "file:///workspace/test.txt";
        let table = StringTable::new();
        let location = cwtools_info::SourceLocation {
            line: 0,
            col: 0,
            end: (0, 1),
        };
        let site = IndexedUseSite {
            type_name: "focus".to_string(),
            name: "first".to_string(),
            case_insensitive: false,
            site: UseSite {
                file: Arc::from(uri),
                key: location,
                value: location,
            },
        };
        let assert_retained = |docs: &DocumentStore| {
            let expected = docs
                .documents
                .values()
                .map(ParsedDoc::retained_bytes)
                .sum::<usize>()
                + docs
                    .line_indices
                    .values()
                    .map(|index| index.retained_bytes())
                    .sum::<usize>()
                + docs.open_use_sites.file_bytes.values().sum::<usize>()
                + docs.open_use_sites.capacity_bytes();
            assert_eq!(docs.retained_text_bytes, expected);
        };
        docs.open(uri.to_string(), document("root = first\n"))
            .unwrap();
        assert!(docs.set_ast_with_use_sites(
            uri,
            1,
            Arc::new(cwtools_parser::parser::parse_string(
                "root = first\n",
                &table
            )),
            vec![site.clone()],
        ));
        assert_retained(&docs);
        let reference_snapshot = |docs: &DocumentStore| {
            docs.reference_sites("focus", "first")
                .into_iter()
                .map(|site| {
                    (
                        site.file,
                        site.key.line,
                        site.key.col,
                        site.key.end,
                        site.value.line,
                        site.value.col,
                        site.value.end,
                    )
                })
                .collect::<Vec<_>>()
        };
        let references = reference_snapshot(&docs);
        docs.change(uri, 2, Arc::from("\n\nroot = first\n"))
            .unwrap();
        assert_eq!(reference_snapshot(&docs), references);
        assert_retained(&docs);
        assert!(docs.set_ast_with_use_sites(
            uri,
            2,
            Arc::new(cwtools_parser::parser::parse_string(
                "\n\nroot = first\n",
                &table
            )),
            vec![site.clone()],
        ));
        assert_retained(&docs);
        assert!(docs.replace_reference_sites(uri, 2, vec![site]));
        assert_retained(&docs);
        docs.set_loc_cache(
            uri,
            2,
            Arc::new(LocDocumentCache {
                version: 2,
                retained_bytes: 10,
                files: Vec::new(),
                references: HashSet::new(),
            }),
        )
        .unwrap();
        assert_retained(&docs);
        docs.open(uri.to_string(), document("reopened\n")).unwrap();
        assert!(docs.reference_sites("focus", "first").is_empty());
        assert_retained(&docs);
        docs.remove(uri).unwrap();
        assert_retained(&docs);
        assert_eq!(docs.retained_text_bytes, 0);
    }

    #[test]
    fn document_store_replaces_and_releases_line_index_with_memory_accounting() {
        let mut docs = DocumentStore::new();
        let uri = "file:///workspace/test.txt";
        docs.open_with_hash(uri.to_string(), document("a\nb\n"), 1)
            .unwrap();
        let old_index = docs.line_index_of(uri).unwrap();
        assert_eq!(old_index.line("a\nb\n", 1), "b");
        assert_eq!(
            docs.retained_text_bytes,
            docs.get(uri).unwrap().retained_bytes() + old_index.retained_bytes()
        );

        docs.change_with_hash(uri, 2, Arc::from("new text\r\n"), 2)
            .unwrap();
        let new_index = docs.line_index_of(uri).unwrap();
        assert_eq!(new_index.line("new text\r\n", 0), "new text");
        assert_eq!(new_index.line("new text\r\n", 1), "");
        assert_eq!(
            docs.retained_text_bytes,
            docs.get(uri).unwrap().retained_bytes() + new_index.retained_bytes()
        );

        docs.remove(uri).unwrap();
        assert_eq!(docs.retained_text_bytes, 0);
        assert!(docs.line_index_of(uri).is_none());
    }

    #[test]
    fn line_index_lookup_rejects_a_different_text_snapshot() {
        let mut docs = DocumentStore::new();
        let uri = "file:///workspace/test.txt";
        docs.open_with_hash(uri.to_string(), document("new\ntext\nwith\nlines"), 1)
            .unwrap();

        // This old snapshot is shorter than several starts in the current
        // index. Returning that index would make `line()` slice out of bounds.
        let old_text: Arc<str> = Arc::from("x");
        assert!(docs.line_index_for_text(uri, &old_text).is_none());

        let safe_index = docs
            .line_index_for_text(uri, &old_text)
            .unwrap_or_else(|| Arc::new(LineIndex::new(&old_text)));
        assert_eq!(safe_index.line(&old_text, 0), "x");
        assert_eq!(safe_index.line(&old_text, 1), "");
    }
}

pub(crate) struct DebounceTask {
    pub(crate) id: u64,
    pub(crate) abort: tokio::task::AbortHandle,
    pub(crate) finished: tokio::sync::oneshot::Receiver<()>,
}

pub(crate) fn remove_debounce_task(
    tasks: &mut HashMap<String, DebounceTask>,
    uri: &str,
    completed_id: u64,
) {
    if tasks.get(uri).is_some_and(|task| task.id == completed_id) {
        tasks.remove(uri);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SemanticCacheEntry {
    pub(crate) result_id: String,
    pub(crate) data: Vec<SemanticToken>,
    pub(crate) hash: u64,
}

pub(crate) struct LocDocumentCache {
    pub(crate) version: i32,
    pub(crate) retained_bytes: usize,
    pub(crate) files: Vec<cwtools_localization::LocFile>,
    pub(crate) references: HashSet<String>,
}

pub(crate) struct ParsedDoc {
    pub(crate) version: i32,
    pub(crate) text: Arc<str>,
    pub(crate) ast: Option<Arc<ParsedFile>>,
    pub(crate) ast_version: Option<i32>,
    pub(crate) ast_source_bytes: usize,
    pub(crate) loc_cache: Option<Arc<LocDocumentCache>>,
}

impl ParsedDoc {
    pub(crate) fn retained_bytes(&self) -> usize {
        self.text.len().max(self.ast_source_bytes).saturating_add(
            self.loc_cache
                .as_ref()
                .map_or(0, |cache| cache.retained_bytes),
        )
    }
}

pub(crate) struct FileTextSnapshot {
    pub(crate) text: Arc<str>,
    pub(crate) version: Option<i32>,
    pub(crate) content_hash: u64,
}

#[derive(Clone, PartialEq)]
pub(crate) struct FixableEdits {
    pub(crate) entries: Vec<(String, cwtools_parser::fix::SpanEdit)>,
    pub(crate) version: Option<i32>,
    pub(crate) content_hash: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AstSource {
    StoredCurrent,
    StoredStale,
    FreshParse,
    None,
}

impl AstSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            AstSource::StoredCurrent => "stored_current",
            AstSource::StoredStale => "stored_stale",
            AstSource::FreshParse => "fresh_parse",
            AstSource::None => "none",
        }
    }

    pub(crate) fn is_current(self) -> bool {
        matches!(self, AstSource::StoredCurrent | AstSource::FreshParse)
    }
}

pub(crate) struct AstSnapshot {
    pub(crate) ast: Arc<ParsedFile>,
    pub(crate) source: AstSource,
}

/// A user-visible rules-load message parked until `initialized` (#98): `Log`
pub(crate) enum DeferredRulesMessage {
    Log(String),
    Toast(String),
}

/// What kicked off a `parse_and_validate` call. Threaded through so the `[validate]` log
/// names its trigger, which makes a validate storm's source legible in the server log
/// (issue #90) instead of a wall of identical lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValidateTrigger {
    DidOpen,
    DidSave,
    DidClose,
    Watched,
    DidChange,
    ConfigChange,
    Reindex,
}

impl ValidateTrigger {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ValidateTrigger::DidOpen => "didOpen",
            ValidateTrigger::DidSave => "didSave",
            ValidateTrigger::DidClose => "didClose",
            ValidateTrigger::Watched => "watched",
            ValidateTrigger::DidChange => "didChange",
            ValidateTrigger::ConfigChange => "configChange",
            ValidateTrigger::Reindex => "reindex",
        }
    }
}

pub(crate) enum DiskState {
    Parsed {
        parsed: ParsedFile,
        discarded_edits: bool,
        text: String,
    },
    Absent,
}

pub(crate) type NotificationJob = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

/// Deep enough that no realistic burst of edits blocks on it, small enough that
/// a client which stops being answered cannot grow the queue without bound.
const NOTIFICATION_QUEUE_CAPACITY: usize = 1024;

/// A completion point in the notification stream. `Notify` alone is not enough:
/// a waiter can be created just after the completion signal. The atomic flag
/// closes that race while keeping the common already-complete path lock-free.
pub(crate) struct NotificationCompletion {
    done: AtomicBool,
    notify: tokio::sync::Notify,
    skipped_to: parking_lot::Mutex<Option<Arc<NotificationCompletion>>>,
}

impl NotificationCompletion {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            done: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
            skipped_to: parking_lot::Mutex::new(None),
        })
    }

    fn completed() -> Arc<Self> {
        let completion = Self::new();
        completion.complete();
        completion
    }

    fn complete(&self) {
        if !self.done.swap(true, Ordering::Release) {
            self.notify.notify_waiters();
        }
    }

    fn skip_to(&self, previous: Arc<Self>) {
        if self.done.load(Ordering::Acquire) {
            return;
        }
        let mut skipped_to = self.skipped_to.lock();
        if self.done.load(Ordering::Acquire) || skipped_to.is_some() {
            return;
        }
        *skipped_to = Some(previous);
        self.notify.notify_waiters();
    }

    pub(crate) async fn wait(self: Arc<Self>) {
        enum Step {
            Done,
            Next(Arc<NotificationCompletion>),
            Wake,
        }

        let mut current = self;
        loop {
            let step = {
                let notified = current.notify.notified();
                tokio::pin!(notified);
                // Register before checking the state. Without `enable`,
                // completion could signal between the load and the first poll
                // of `notified`, losing the wake-up and stranding the request.
                notified.as_mut().enable();
                if current.done.load(Ordering::Acquire) {
                    Step::Done
                } else if let Some(previous) = { current.skipped_to.lock().clone() } {
                    Step::Next(previous)
                } else {
                    notified.await;
                    Step::Wake
                }
            };
            match step {
                Step::Done => return,
                Step::Next(previous) => current = previous,
                Step::Wake => {}
            }
        }
    }
}

/// A notification's slot is reserved when the transport sees the notification,
/// before its handler future is polled. This is what lets a request between two
/// notifications capture exactly the first one rather than whichever jobs the
/// executor happened to poll first.
pub(crate) struct NotificationReservation {
    queue: NotificationQueue,
    previous: Arc<NotificationCompletion>,
    completion: Arc<NotificationCompletion>,
    claimed: AtomicBool,
}

impl NotificationReservation {
    pub(crate) fn claim(&self) -> Option<NotificationJobReservation> {
        if self.claimed.swap(true, Ordering::AcqRel) {
            return None;
        }
        Some(NotificationJobReservation {
            queue: self.queue.clone(),
            previous: self.previous.clone(),
            completion: self.completion.clone(),
            armed: true,
        })
    }
}

impl Drop for NotificationReservation {
    fn drop(&mut self) {
        if !self.claimed.swap(true, Ordering::AcqRel) {
            self.completion.skip_to(self.previous.clone());
        }
    }
}

pub(crate) struct NotificationJobReservation {
    queue: NotificationQueue,
    previous: Arc<NotificationCompletion>,
    completion: Arc<NotificationCompletion>,
    armed: bool,
}

impl Drop for NotificationJobReservation {
    fn drop(&mut self) {
        if self.armed {
            self.completion.skip_to(self.previous.clone());
        }
    }
}

impl NotificationJobReservation {
    pub(crate) async fn send(self, job: NotificationJob) {
        self.send_inner(job, None).await;
    }

    pub(crate) async fn send_background(self, job: NotificationJob) {
        let permit = self
            .queue
            .sender_slots
            .clone()
            .acquire_owned()
            .await
            .expect("notification sender slots stay open");
        tokio::spawn(async move { self.send_inner(job, Some(permit)).await });
    }

    async fn send_inner(
        mut self,
        job: NotificationJob,
        permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) {
        let queue = self.queue.clone();
        let previous = self.previous.clone();
        let completion = self.completion.clone();
        // A detached sender task can be dropped before its future is first
        // polled. Once this method owns the fallback guard, that drop path is
        // covered by the guard instead of this reservation.
        let mut fallback = ReservationDropGuard::new(completion.clone(), previous.clone());
        self.armed = false;
        // Wait before handing the job to the single worker. If a later handler
        // is polled first, its job must not occupy the worker while waiting for
        // an earlier job that is still behind it in the channel.
        previous.clone().wait().await;
        let job = QueuedNotification {
            job,
            completion,
            previous,
            permit,
            finished: false,
        };
        if !queue.is_running() {
            fallback.disarm();
            job.await;
            return;
        }
        if let Err(returned) = queue.sender.send(Box::pin(job)).await {
            tracing::warn!("notification worker is gone; running the handler inline");
            fallback.disarm();
            returned.0.await;
        } else {
            fallback.disarm();
        }
    }
}

struct ReservationDropGuard {
    completion: Option<Arc<NotificationCompletion>>,
    previous: Arc<NotificationCompletion>,
}

impl ReservationDropGuard {
    fn new(completion: Arc<NotificationCompletion>, previous: Arc<NotificationCompletion>) -> Self {
        Self {
            completion: Some(completion),
            previous,
        }
    }

    fn disarm(&mut self) {
        self.completion = None;
    }
}

impl Drop for ReservationDropGuard {
    fn drop(&mut self) {
        if let Some(completion) = self.completion.take() {
            completion.skip_to(self.previous.clone());
        }
    }
}

struct QueuedNotification {
    job: NotificationJob,
    completion: Arc<NotificationCompletion>,
    previous: Arc<NotificationCompletion>,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    finished: bool,
}

impl std::future::Future for QueuedNotification {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let result = self.job.as_mut().poll(cx);
        if result.is_ready() {
            self.finished = true;
            self.completion.complete();
        }
        result
    }
}

impl Drop for QueuedNotification {
    fn drop(&mut self) {
        if !self.finished {
            self.completion.skip_to(self.previous.clone());
        }
        drop(self.permit.take());
    }
}

/// One worker draining one bounded queue, so document notifications run off
/// tower-lsp's message pump without losing their order (#470).
///
/// Both halves of that matter. `didClose` runs two full-file validations and
/// `didChangeConfiguration` can start a whole workspace scan, and doing that on
/// the pump froze the protocol channel. But spawning each notification
/// separately would let `didOpen` → `didChange` → `didClose` land out of order,
/// which the document store has no way to recover from. A single worker gives
/// both properties at once.
///
/// `enqueue` is backpressured rather than blocking: a full queue makes the pump
/// *yield*, which is exactly the right behaviour, unlike a parked thread.
#[derive(Clone)]
pub(crate) struct NotificationQueue {
    sender: tokio::sync::mpsc::Sender<NotificationJob>,
    receiver: Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::Receiver<NotificationJob>>>>,
    started: Arc<AtomicBool>,
    tail: Arc<parking_lot::Mutex<Option<Arc<NotificationCompletion>>>>,
    sender_slots: Arc<tokio::sync::Semaphore>,
}

impl NotificationQueue {
    pub(crate) fn new() -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
        Self {
            sender,
            receiver: Arc::new(parking_lot::Mutex::new(Some(receiver))),
            started: Arc::new(AtomicBool::new(false)),
            tail: Arc::new(parking_lot::Mutex::new(None)),
            sender_slots: Arc::new(tokio::sync::Semaphore::new(NOTIFICATION_QUEUE_CAPACITY)),
        }
    }

    /// Hands the draining half to `main`, once. The unit tests build a bare
    /// `DocumentState` and never call this; `Backend::enqueue_notification`
    /// then runs each job inline instead, which is what those tests want and
    /// what keeps a notification from being silently dropped.
    pub(crate) fn take_receiver(&self) -> Option<tokio::sync::mpsc::Receiver<NotificationJob>> {
        let receiver = self.receiver.lock().take();
        if receiver.is_some() {
            self.started.store(true, Ordering::Release);
        }
        receiver
    }

    pub(crate) fn is_running(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }

    /// Reserve a slot in the order in which the transport received a
    /// notification. Dropping an unclaimed reservation completes it, so a
    /// notification that is shut down before its handler starts cannot strand
    /// every request behind it.
    pub(crate) fn reserve(&self) -> NotificationReservation {
        let completion = NotificationCompletion::new();
        let previous = self
            .tail
            .lock()
            .replace(completion.clone())
            .unwrap_or_else(NotificationCompletion::completed);
        NotificationReservation {
            queue: self.clone(),
            previous,
            completion,
            claimed: AtomicBool::new(false),
        }
    }

    pub(crate) fn barrier(&self) -> Arc<NotificationCompletion> {
        self.tail
            .lock()
            .clone()
            .unwrap_or_else(NotificationCompletion::completed)
    }
}

impl DocumentState {
    pub(crate) fn new() -> Self {
        let string_table = StringTable::new();
        // Before any document can be parsed into an overlay region, so the ids the
        // per-game validators compare against are always base ids (#475).
        cwtools_validation::per_game::seed_comparison_literals(&string_table);
        Self {
            diagnostic_publication_lock: tokio::sync::Mutex::new(()),
            baseline: Mutex::new(crate::baseline_runtime::State::default()),
            documents: Mutex::new(DocumentStore::new()),
            config: parking_lot::RwLock::new(Config::new()),
            workspace_roots_generation: AtomicU64::new(0),
            rules: parking_lot::RwLock::new(RuleData::new()),
            modifier_rebuild_lock: parking_lot::Mutex::new(()),
            string_table,
            info_service: parking_lot::RwLock::new(cwtools_info::InfoService::new()),
            vanilla_state: Mutex::new(VanillaState::default()),
            vanilla_loc: Mutex::new(None),
            parent_loc: Mutex::new(None),
            loc_index: parking_lot::RwLock::new(None),
            inline_scripts: parking_lot::RwLock::new(InlineScripts::default()),
            loc_key_index: parking_lot::RwLock::new(None),
            loc_text: parking_lot::RwLock::new(LocText::default()),
            loc_locations: parking_lot::RwLock::new(LocLocations::default()),
            loc_live_overlay: parking_lot::RwLock::new(HashMap::new()),
            loc_watched_overlay: parking_lot::RwLock::new(HashMap::new()),
            loc_overlay_revision: AtomicU64::new(0),
            loc_overlay_keys_cache: parking_lot::Mutex::new(None),
            loc_ref_names_cache: parking_lot::Mutex::new(None),
            hover_show_all_languages: std::sync::atomic::AtomicBool::new(false),
            hover_debug: std::sync::atomic::AtomicBool::new(false),
            hover_resolved_scope: std::sync::atomic::AtomicBool::new(false),
            inlay_hints_loc_titles: std::sync::atomic::AtomicBool::new(true),
            inlay_hints_scopes: std::sync::atomic::AtomicBool::new(false),
            hierarchical_symbols: std::sync::atomic::AtomicBool::new(false),
            workspace_edit_document_changes: std::sync::atomic::AtomicBool::new(false),
            completion_label_details: std::sync::atomic::AtomicBool::new(false),
            client_work_done_progress: std::sync::atomic::AtomicBool::new(false),
            semantic_tokens_refresh_support: std::sync::atomic::AtomicBool::new(false),
            code_lens_refresh_support: std::sync::atomic::AtomicBool::new(false),
            watched_files_dynamic_registration: std::sync::atomic::AtomicBool::new(false),
            scan_progress_active: std::sync::atomic::AtomicBool::new(false),
            command_cancels: parking_lot::Mutex::new(HashMap::new()),
            loading_bar_active: std::sync::atomic::AtomicBool::new(false),
            index_ready: std::sync::atomic::AtomicBool::new(false),
            handshake_complete: std::sync::atomic::AtomicBool::new(false),
            deferred_rule_diagnostics: parking_lot::Mutex::new(Vec::new()),
            deferred_rules_messages: parking_lot::Mutex::new(Vec::new()),
            last_rules_toast: parking_lot::Mutex::new(None),
            format_refusal_toasts: parking_lot::Mutex::new(HashSet::new()),
            document_refusal_toasts: parking_lot::Mutex::new(HashSet::new()),
            published_rule_uris: parking_lot::Mutex::new(HashSet::new()),
            published_loc_uris: parking_lot::Mutex::new(HashSet::new()),
            edit_generation: AtomicU64::new(0),
            doc_tokens: parking_lot::RwLock::new(HashMap::new()),
            pending_changed_names: Mutex::new(HashSet::new()),
            type_uses: parking_lot::RwLock::new(HashMap::new()),
            type_uses_merged: parking_lot::Mutex::new(None),
            type_uses_revision: AtomicU64::new(0),
            vanilla_merged: std::sync::atomic::AtomicBool::new(false),
            scan_in_progress: AtomicBool::new(false),
            shutdown: tokio::sync::watch::channel(false).0,
            debounce_handles: Mutex::new(HashMap::new()),
            next_debounce_id: AtomicU64::new(0),
            validation_permits: tokio::sync::Semaphore::new(MAX_CONCURRENT_VALIDATIONS),
            info_revision: AtomicU64::new(0),
            fallback_cache: parking_lot::Mutex::new(None),
            fresh_ast_cache: parking_lot::Mutex::new(None),
            completion_generation: parking_lot::Mutex::new(HashMap::new()),
            next_completion_id: AtomicU64::new(0),
            last_loc_signature: parking_lot::Mutex::new(None),
            loc_discovery_cache: parking_lot::Mutex::new(None),
            last_scan_fingerprint: parking_lot::Mutex::new(None),
            #[cfg(test)]
            hold_gate: parking_lot::Mutex::new(None),
            #[cfg(test)]
            navigation_snapshot_budget_override: parking_lot::Mutex::new(None),
            #[cfg(test)]
            workspace_scan_budget_override: parking_lot::Mutex::new(None),
            #[cfg(test)]
            scan_source_reads: std::sync::atomic::AtomicUsize::new(0),
            settings_generation: AtomicU64::new(0),
            start: std::time::Instant::now(),
            last_activity_ms: AtomicU64::new(0),
            watched_pending: Mutex::new(HashSet::new()),
            watched_created: Mutex::new(HashSet::new()),
            watched_deleted: Mutex::new(HashSet::new()),
            watched_debounce: Mutex::new(None),
            watched_signatures: Mutex::new(HashMap::new()),
            semantic_tokens_cache: Mutex::new(HashMap::new()),
            semantic_tokens_seq: AtomicU64::new(0),
            fixable_edits: Mutex::new(HashMap::new()),
            last_scan_summary: Mutex::new(None),
            published_workspace_uris: Mutex::new(HashSet::new()),
            notifications: NotificationQueue::new(),
        }
    }

    pub(crate) fn open_workspace_document(
        &self,
        uri: String,
        document: ParsedDoc,
        content_hash: u64,
        mut is_workspace_document: impl FnMut() -> bool,
    ) -> std::result::Result<(), DocumentRejection> {
        loop {
            let roots_generation = self.workspace_roots_generation.load(Ordering::Acquire);
            if !is_workspace_document() {
                return Err(DocumentRejection::OutsideWorkspace);
            }
            let mut documents = self.documents.lock();
            if roots_generation != self.workspace_roots_generation.load(Ordering::Acquire) {
                continue;
            }
            return documents.open_with_hash(uri, document, content_hash);
        }
    }

    /// Record a refusal of `key` for `reason`. False when it is already recorded
    /// or the record is full, so the caller toasts only on true (#1137).
    pub(crate) fn record_document_refusal(&self, key: &str, reason: &'static str) -> bool {
        let mut toasts = self.document_refusal_toasts.lock();
        toasts.len() < MAX_DOCUMENT_REFUSAL_TOASTS && toasts.insert((key.to_string(), reason))
    }

    /// Drop the refusal records for `uri` once it is admitted or closed, so a
    /// later refusal reports anew (#1137).
    pub(crate) fn clear_document_refusal(&self, uri: &str) {
        self.document_refusal_toasts
            .lock()
            .retain(|(reported, _)| reported != uri);
    }
}

/// `Client` is a handle and `state` is an `Arc`, so a clone is two refcount
/// bumps — cheap enough to hand one to every spawned handler.
#[derive(Clone)]
pub(crate) struct Backend {
    pub(crate) client: Client,
    pub(crate) state: Arc<DocumentState>,
}
