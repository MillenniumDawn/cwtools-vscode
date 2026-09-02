#[cfg(test)]
use parking_lot::Condvar;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tower_lsp::Client;
use tower_lsp::lsp_types::*;

use cwtools_info::TypeInstance;
use cwtools_parser::ast::ParsedFile;
use cwtools_rules::rules_types::RuleSet;
use cwtools_string_table::string_table::{StringId, StringTable};
use cwtools_validation::{InlineScripts, references};

use crate::scan::ScanSummary;

pub(crate) type LocTextMap = FxHashMap<Arc<str>, Vec<(cwtools_localization::Lang, String)>>;
pub(crate) type LocLocationMap = FxHashMap<Arc<str>, (Arc<str>, u32)>;

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pass2HoldPoint {
    Before,
    Mid,
    After,
}

#[cfg(test)]
pub(crate) struct Pass2Gate {
    hold_at: Pass2HoldPoint,
    state: Mutex<Pass2GateState>,
    cv: Condvar,
}

#[cfg(test)]
struct Pass2GateState {
    arrived: bool,
    released: bool,
}

#[cfg(test)]
impl Pass2Gate {
    pub(crate) fn new(hold_at: Pass2HoldPoint) -> Arc<Self> {
        Arc::new(Self {
            hold_at,
            state: Mutex::new(Pass2GateState {
                arrived: false,
                released: false,
            }),
            cv: Condvar::new(),
        })
    }

    pub(crate) fn hold(&self, point: Pass2HoldPoint) {
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
        self.authorized_roots = editable
            .iter()
            .cloned()
            .chain(
                self.vanilla_dir
                    .iter()
                    .chain(self.rules_dir.iter())
                    .filter_map(|root| std::fs::canonicalize(root).ok()),
            )
            .collect();
        self.editable_roots = editable.into();
    }
}

pub(crate) struct RuleData {
    pub(crate) ruleset: Option<Arc<RuleSet>>,
    pub(crate) scope_registry: Option<Arc<cwtools_game::scope_registry::ScopeRegistry>>,
    /// whole set (#78).
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
}

/// LOCK ORDER: when holding more than one guard, acquire in this order —
pub(crate) struct DocumentState {
    pub(crate) documents: Mutex<DocumentStore>,
    pub(crate) config: parking_lot::RwLock<Config>,
    pub(crate) workspace_roots_generation: AtomicU64,
    pub(crate) rules: parking_lot::RwLock<RuleData>,
    pub(crate) string_table: StringTable,
    pub(crate) info_service: parking_lot::RwLock<cwtools_info::InfoService>,
    #[allow(clippy::type_complexity)]
    pub(crate) vanilla_index: Mutex<Option<HashMap<String, Vec<(Arc<str>, TypeInstance)>>>>,
    pub(crate) vanilla_merged_uris: Mutex<HashSet<Arc<str>>>,
    #[allow(clippy::type_complexity)]
    pub(crate) vanilla_loc_keys: Mutex<Option<Vec<(String, Vec<String>)>>>,
    /// so the two halves have to land in the same write (#283).
    pub(crate) vanilla_file_paths: Mutex<Option<Vec<String>>>,
    pub(crate) vanilla_var_names: Mutex<Option<Vec<String>>>,
    pub(crate) vanilla_scripted_loc_names: Mutex<Option<Vec<String>>>,
    pub(crate) vanilla_scripted_gui_names: Mutex<Option<Vec<String>>>,
    /// (#89) — vanilla is ~2000 loc files that cannot change while the editor is
    #[allow(clippy::type_complexity)]
    pub(crate) vanilla_loc:
        Mutex<Option<(crate::scan::VanillaLocKey, Arc<crate::scan::VanillaLoc>)>>,
    pub(crate) loc_index: parking_lot::RwLock<Option<Arc<cwtools_localization::LocIndex>>>,
    /// (#259) — the editor's counterpart of the batch driver's
    pub(crate) inline_scripts: parking_lot::RwLock<InlineScripts>,
    pub(crate) loc_key_index: parking_lot::RwLock<Option<Arc<crate::completion::LocKeyIndex>>>,
    #[allow(clippy::type_complexity)]
    pub(crate) loc_text: parking_lot::RwLock<LocTextMap>,
    pub(crate) loc_locations: parking_lot::RwLock<LocLocationMap>,
    /// full rescan (#36). Bounded by the number of open loc files, so it stays
    pub(crate) loc_live_overlay: parking_lot::RwLock<HashMap<String, HashSet<String>>>,
    pub(crate) loc_watched_overlay: parking_lot::RwLock<HashMap<String, HashSet<String>>>,
    pub(crate) loc_overlay_revision: AtomicU64,
    pub(crate) loc_overlay_keys_cache: parking_lot::Mutex<Option<(u64, Arc<HashSet<String>>)>>,
    pub(crate) loc_ref_names_cache: parking_lot::Mutex<Option<(u64, Arc<HashSet<String>>)>>,
    pub(crate) hover_show_all_languages: std::sync::atomic::AtomicBool,
    pub(crate) hover_debug: std::sync::atomic::AtomicBool,
    /// default — the ambient scope is shown alone. (#37)
    pub(crate) hover_resolved_scope: std::sync::atomic::AtomicBool,
    pub(crate) inlay_hints_loc_titles: std::sync::atomic::AtomicBool,
    pub(crate) inlay_hints_scopes: std::sync::atomic::AtomicBool,
    pub(crate) hierarchical_symbols: std::sync::atomic::AtomicBool,
    pub(crate) workspace_edit_document_changes: std::sync::atomic::AtomicBool,
    pub(crate) completion_label_details: std::sync::atomic::AtomicBool,
    pub(crate) client_work_done_progress: std::sync::atomic::AtomicBool,
    pub(crate) semantic_tokens_refresh_support: std::sync::atomic::AtomicBool,
    pub(crate) code_lens_refresh_support: std::sync::atomic::AtomicBool,
    pub(crate) scan_progress_active: std::sync::atomic::AtomicBool,
    pub(crate) command_cancels: parking_lot::Mutex<HashMap<String, Arc<AtomicBool>>>,
    pub(crate) loading_bar_active: std::sync::atomic::AtomicBool,
    pub(crate) index_ready: std::sync::atomic::AtomicBool,
    pub(crate) handshake_complete: std::sync::atomic::AtomicBool,
    /// `initialize` where the gate above would swallow them (#98).
    pub(crate) deferred_rule_diagnostics: parking_lot::Mutex<Vec<(String, Vec<Diagnostic>)>>,
    /// toast), parked the same way as the diagnostics above (#98).
    pub(crate) deferred_rules_messages: parking_lot::Mutex<Vec<DeferredRulesMessage>>,
    pub(crate) last_rules_toast: parking_lot::Mutex<Option<String>>,
    pub(crate) published_rule_uris: parking_lot::Mutex<HashSet<String>>,
    pub(crate) published_loc_uris: parking_lot::Mutex<HashSet<String>>,
    pub(crate) edit_generation: AtomicU64,
    pub(crate) doc_tokens: parking_lot::RwLock<HashMap<String, HashSet<StringId>>>,
    pub(crate) pending_changed_names: Mutex<HashSet<String>>,
    pub(crate) type_uses: parking_lot::RwLock<HashMap<String, references::UsedInstances>>,
    #[allow(clippy::type_complexity)]
    pub(crate) type_uses_merged: parking_lot::Mutex<Option<(u64, Arc<references::UsedInstances>)>>,
    pub(crate) type_uses_revision: AtomicU64,
    pub(crate) vanilla_merged: std::sync::atomic::AtomicBool,
    pub(crate) scan_in_progress: AtomicBool,
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
    pub(crate) pass2_gate: parking_lot::Mutex<Option<Arc<Pass2Gate>>>,
    pub(crate) settings_generation: AtomicU64,
    pub(crate) start: std::time::Instant,
    pub(crate) last_activity_ms: AtomicU64,
    /// bounded request queue (#90).
    pub(crate) watched_pending: Mutex<HashSet<String>>,
    pub(crate) watched_deleted: Mutex<HashSet<String>>,
    pub(crate) watched_debounce: Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub(crate) watched_signatures: Mutex<HashMap<String, (u64, u128)>>,
    /// requested or after invalidation (file change, rename, encoding switch,
    pub(crate) semantic_tokens_cache: Mutex<HashMap<String, SemanticCacheEntry>>,
    pub(crate) semantic_tokens_seq: AtomicU64,
    pub(crate) fixable_edits: Mutex<HashMap<String, FixableEdits>>,
    pub(crate) last_scan_summary: Mutex<Option<ScanSummary>>,
    pub(crate) published_workspace_uris: Mutex<HashSet<String>>,
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
const MAX_CONCURRENT_VALIDATIONS: usize = 2;

pub(crate) struct DocumentStore {
    documents: HashMap<String, ParsedDoc>,
    pub(crate) retained_text_bytes: usize,
}

impl DocumentStore {
    pub(crate) fn new() -> Self {
        Self {
            documents: HashMap::new(),
            retained_text_bytes: 0,
        }
    }

    pub(crate) fn open(
        &mut self,
        uri: String,
        document: ParsedDoc,
    ) -> std::result::Result<(), DocumentRejection> {
        let old_len = self
            .documents
            .get(&uri)
            .map_or(0, ParsedDoc::retained_bytes);
        if !self.documents.contains_key(&uri) && self.documents.len() >= MAX_OPEN_DOCUMENTS {
            return Err(DocumentRejection::TooManyOpen);
        }
        let retained = self.replacement_total(old_len, document.retained_bytes())?;
        self.documents.insert(uri, document);
        self.retained_text_bytes = retained;
        Ok(())
    }

    pub(crate) fn change(
        &mut self,
        uri: &str,
        version: i32,
        text: Arc<str>,
    ) -> std::result::Result<(), DocumentRejection> {
        let Some(document) = self.documents.get(uri) else {
            return Err(DocumentRejection::NotOpen);
        };
        let old_len = document.retained_bytes();
        let new_len = text.len().max(document.ast_source_bytes);
        let retained = self.replacement_total(old_len, new_len)?;
        let Some(document) = self.documents.get_mut(uri) else {
            return Err(DocumentRejection::NotOpen);
        };
        document.version = version;
        document.text = text;
        document.loc_cache = None;
        self.retained_text_bytes = retained;
        Ok(())
    }

    pub(crate) fn set_ast(&mut self, uri: &str, version: i32, ast: Arc<ParsedFile>) -> bool {
        let Some(document) = self.documents.get_mut(uri) else {
            return false;
        };
        if document.version != version {
            return false;
        }
        let old_len = document.retained_bytes();
        document.ast = Some(ast);
        document.ast_version = Some(version);
        document.ast_source_bytes = document.text.len();
        let new_len = document.retained_bytes();
        self.retained_text_bytes = self.retained_text_bytes - old_len + new_len;
        true
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
        let old_len = document.retained_bytes();
        let new_len = document
            .text
            .len()
            .max(document.ast_source_bytes)
            .saturating_add(cache.retained_bytes);
        let retained = self.replacement_total(old_len, new_len)?;
        let Some(document) = self.documents.get_mut(uri) else {
            return Ok(false);
        };
        document.loc_cache = Some(cache);
        self.retained_text_bytes = retained;
        Ok(true)
    }

    pub(crate) fn remove(&mut self, uri: &str) -> Option<ParsedDoc> {
        let document = self.documents.remove(uri)?;
        self.retained_text_bytes -= document.retained_bytes();
        Some(document)
    }

    pub(crate) fn replacement_total(
        &self,
        old_len: usize,
        new_len: usize,
    ) -> std::result::Result<usize, DocumentRejection> {
        if new_len > MAX_DOCUMENT_BYTES {
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
    pub(crate) text: String,
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

/// legible in the server log (issue #90) instead of a wall of identical lines.
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

impl DocumentState {
    pub(crate) fn new() -> Self {
        Self {
            documents: Mutex::new(DocumentStore::new()),
            config: parking_lot::RwLock::new(Config::new()),
            workspace_roots_generation: AtomicU64::new(0),
            rules: parking_lot::RwLock::new(RuleData::new()),
            string_table: StringTable::new(),
            info_service: parking_lot::RwLock::new(cwtools_info::InfoService::new()),
            vanilla_index: Mutex::new(None),
            vanilla_merged_uris: Mutex::new(HashSet::new()),
            vanilla_loc_keys: Mutex::new(None),
            vanilla_file_paths: Mutex::new(None),
            vanilla_var_names: Mutex::new(None),
            vanilla_scripted_loc_names: Mutex::new(None),
            vanilla_scripted_gui_names: Mutex::new(None),
            vanilla_loc: Mutex::new(None),
            loc_index: parking_lot::RwLock::new(None),
            inline_scripts: parking_lot::RwLock::new(InlineScripts::default()),
            loc_key_index: parking_lot::RwLock::new(None),
            loc_text: parking_lot::RwLock::new(LocTextMap::default()),
            loc_locations: parking_lot::RwLock::new(LocLocationMap::default()),
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
            scan_progress_active: std::sync::atomic::AtomicBool::new(false),
            command_cancels: parking_lot::Mutex::new(HashMap::new()),
            loading_bar_active: std::sync::atomic::AtomicBool::new(false),
            index_ready: std::sync::atomic::AtomicBool::new(false),
            handshake_complete: std::sync::atomic::AtomicBool::new(false),
            deferred_rule_diagnostics: parking_lot::Mutex::new(Vec::new()),
            deferred_rules_messages: parking_lot::Mutex::new(Vec::new()),
            last_rules_toast: parking_lot::Mutex::new(None),
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
            pass2_gate: parking_lot::Mutex::new(None),
            settings_generation: AtomicU64::new(0),
            start: std::time::Instant::now(),
            last_activity_ms: AtomicU64::new(0),
            watched_pending: Mutex::new(HashSet::new()),
            watched_deleted: Mutex::new(HashSet::new()),
            watched_debounce: Mutex::new(None),
            watched_signatures: Mutex::new(HashMap::new()),
            semantic_tokens_cache: Mutex::new(HashMap::new()),
            semantic_tokens_seq: AtomicU64::new(0),
            fixable_edits: Mutex::new(HashMap::new()),
            last_scan_summary: Mutex::new(None),
            published_workspace_uris: Mutex::new(HashSet::new()),
        }
    }

    pub(crate) fn open_workspace_document(
        &self,
        uri: String,
        document: ParsedDoc,
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
            return documents.open(uri, document);
        }
    }
}

pub(crate) struct Backend {
    pub(crate) client: Client,
    pub(crate) state: Arc<DocumentState>,
}
