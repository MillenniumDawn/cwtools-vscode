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
use cwtools_validation::references;

use crate::scan::ScanSummary;

pub(crate) type LocTextMap = FxHashMap<Arc<str>, Vec<(cwtools_localization::Lang, String)>>;
pub(crate) type LocLocationMap = FxHashMap<Arc<str>, (Arc<str>, u32)>;

/// Where a pass-2 cancel test latches the flag. `After` is after extend,
/// before `yield_now`.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pass2HoldPoint {
    Before,
    Mid,
    After,
}

/// First matching caller parks; later Mid workers skip so a partial chunk can
/// finish.
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

/// Settings group: values set once at `initialize` / `didChangeConfiguration`
/// and only read (clone-and-drop) everywhere else. Held behind a single
/// `RwLock<Config>` so a config read never serializes behind an unrelated
/// write. The guard is never held across another lock or an await — every
/// reader clones what it needs and drops the guard immediately.
pub(crate) struct Config {
    /// game language from init options
    pub(crate) language: String,
    /// workspace folder URI captured from initialize params. `Arc<str>` so the
    /// per-handler reads clone a cheap refcount bump, not the whole string.
    pub(crate) workspace_uri: Option<Arc<str>>,
    /// Normalized, decoded workspace path prefix, precomputed from
    /// `workspace_uri` so per-request logical-path derivation doesn't re-parse
    /// the constant workspace URI (see `paths::workspace_prefix_of`).
    pub(crate) workspace_prefix: Option<Arc<str>>,
    /// Every workspace folder the client reported, `workspace_uri` first.
    /// Only the URI access boundary reads the whole list — the scan, the
    /// logical paths and the type index are all built from the primary folder
    /// alone. Kept so a multi-root window doesn't lose closed-file features in
    /// its other folders (see `access`).
    pub(crate) workspace_roots: Vec<std::path::PathBuf>,
    /// Canonicalized directories a client URI is allowed to name:
    /// `workspace_roots` plus `vanilla_dir` and `rules_dir`. Recomputed by
    /// [`Config::refresh_roots`] whenever one of those is set, so a
    /// request pays one `canonicalize` of its target instead of re-canonicalizing
    /// every root. `Arc` so the per-request read is a refcount bump.
    pub(crate) authorized_roots: Arc<[std::path::PathBuf]>,
    /// Canonicalized directories a server-generated edit is allowed to write
    /// to: the workspace folders alone. A strict subset of `authorized_roots` —
    /// the base-game install and the rules dir are readable but never writable
    /// (see `access::editable_path`). Refreshed with `authorized_roots`, so a
    /// folder added or removed mid-session moves both.
    pub(crate) editable_roots: Arc<[std::path::PathBuf]>,
    /// base-game install dir (from the `vanilla` init option, or auto-discovered).
    /// Indexed lazily into `vanilla_index` on the first full-workspace scan.
    pub(crate) vanilla_dir: Option<std::path::PathBuf>,
    /// Writable directory for persistent caches (from the `cacheDir` init
    /// option, else an OS cache dir). The base-game type index is cached here
    /// keyed by game + version, so it isn't re-parsed on every startup.
    pub(crate) cache_dir: Option<std::path::PathBuf>,
    /// languages to validate loc against, from the `localisationLanguages` init
    /// option. `None` = all languages with data (the default). When set, the
    /// missing-translation check and per-file loc checks are scoped to these,
    /// so an english-targeted mod isn't flagged for every other language vanilla
    /// happens to ship.
    pub(crate) loc_languages: Option<Vec<cwtools_localization::Lang>>,
    /// Extra filename glob patterns to skip during the workspace scan (on top
    /// of the engine baseline like Changelog.txt / README.md). Sourced from
    /// `ignoreFilePatterns` in `initializationOptions` and the
    /// `workspace/didChangeConfiguration` payload.
    pub(crate) ignore_file_patterns: Vec<String>,
    /// Extra directory glob patterns to skip during the workspace scan. Sourced
    /// from `ignoreDirectories` in `initializationOptions` and
    /// `workspace/didChangeConfiguration`.
    pub(crate) ignore_dir_patterns: Vec<String>,
    /// Diagnostic codes (e.g. `CW100`) the user suppressed via `errors.ignore`
    /// (`ignoredErrorCodes`). Stored lowercased; matched case-insensitively
    /// against each diagnostic's code just before publishing.
    pub(crate) ignored_error_codes: Vec<String>,
    /// Rules-config directory loaded at `initialize` (the `rulesCache` init
    /// option). Retained so the `reloadrulesconfig` command can re-read it.
    pub(crate) rules_dir: Option<std::path::PathBuf>,
    pub(crate) scope_checks: bool,
    pub(crate) var_checks: bool,
    /// Minutes between quiet background re-index passes (0 = off, the
    /// default). Sourced from `backgroundReindexIntervalMinutes` in
    /// `initializationOptions` and `workspace/didChangeConfiguration`. A raw
    /// client that never sends either keeps this at 0, so the periodic loop
    /// stays disabled unless explicitly configured.
    pub(crate) background_reindex_interval_minutes: u64,
    /// Seconds the user must be idle before a background pass runs (default
    /// 15). Sourced from `backgroundReindexIdleSeconds` in
    /// `initializationOptions` and `workspace/didChangeConfiguration`; the
    /// `CWTOOLS_REINDEX_IDLE_SECS` test override wins over this value. A live
    /// change applies on the next reindex cycle.
    pub(crate) background_reindex_idle_seconds: u64,
    /// Whether the scan publishes diagnostics for closed workspace files.
    /// Default `true` so a mod author's Problems panel stays up to date; set
    /// to `false` to limit diagnostics to open documents only (the old
    /// behaviour). Sourced from `workspaceWideDiagnostics` in
    /// `initializationOptions` and `workspace/didChangeConfiguration`.
    pub(crate) workspace_wide_diagnostics: bool,
    /// Position encoding negotiated with the client. LSP defaults to UTF-16.
    pub(crate) position_encoding: tower_lsp::lsp_types::PositionEncodingKind,
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
        }
    }

    /// Resolve the configured language to an engine [`Game`], for the many
    /// sites that only need the typed game (not the raw language string).
    pub(crate) fn game(&self) -> Option<cwtools_game::constants::Game> {
        cwtools_game::constants::Game::from_str(&self.language)
    }

    /// Rebuild [`Config::authorized_roots`] and [`Config::editable_roots`] from
    /// the workspace folders, the base-game install and the rules dir. Call
    /// after writing any of them. A root that doesn't resolve is dropped rather
    /// than kept unresolved: it could never match a canonicalized target anyway.
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

/// Ruleset-derived group: rebuilt together whenever a ruleset is loaded.
/// One `RwLock<RuleData>` so the readers that need all three (hover,
/// completion, the workspace scan) take a single guard instead of three.
pub(crate) struct RuleData {
    /// loaded .cwt ruleset. The many readers (hover, completion, validation,
    /// the cross-file sweep) share the guard and don't serialize behind a
    /// debounced validate; only the rare ruleset load/reload takes `write()`.
    pub(crate) ruleset: Option<Arc<RuleSet>>,
    /// Scope/link registry built from `ruleset` (config-driven scopes.cwt +
    /// links.cwt). Cached here because `build_scope_registry` is the expensive
    /// part of per-file validation setup and depends only on the loaded ruleset,
    /// which changes rarely. Rebuilt at the ruleset write site, so it always
    /// matches the ruleset it was derived from. `None` until the first load.
    pub(crate) scope_registry: Option<Arc<cwtools_game::scope_registry::ScopeRegistry>>,
    /// cached modifier-key set; rebuilt after ruleset load and after each full
    /// workspace scan when the type index is complete. `Arc` so the workspace
    /// scan snapshots it with a cheap refcount bump instead of deep-copying the
    /// whole set (#78).
    pub(crate) modifier_keys: Arc<HashSet<String>>,
    /// expanded modifier name → its category's `supported_scopes`, for
    /// scope-aware modifier ranking in completion. A pure function of
    /// ruleset + type index, rebuilt together with `modifier_keys` so the two
    /// can never disagree.
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

/// Server state.
///
/// LOCK ORDER: when holding more than one guard, acquire in this order —
/// `documents` -> `rules` -> `info_service` -> `loc_index`. `config` is a
/// settings snapshot: it is always read-clone-dropped and never held across
/// another lock or an await. Most sites snapshot-and-drop the others too; the
/// places that co-hold are the workspace scan and single-file validate
/// (`rules` -> `info_service` -> `loc_index`). Never acquire an earlier lock
/// while holding a later one.
pub(crate) struct DocumentState {
    /// Open documents plus exact retained-text accounting under the same lock.
    pub(crate) documents: Mutex<DocumentStore>,
    /// Settings set at init / didChangeConfiguration, read-clone-dropped
    /// elsewhere. See [`Config`].
    pub(crate) config: parking_lot::RwLock<Config>,
    pub(crate) workspace_roots_generation: AtomicU64,
    /// Ruleset + scope registry + modifier keys, rebuilt together on ruleset
    /// load. See [`RuleData`].
    pub(crate) rules: parking_lot::RwLock<RuleData>,
    /// shared string table
    pub(crate) string_table: StringTable,
    /// computed info service for type/references/definitions. `RwLock` so the
    /// full-workspace pass-2 validation can share a single read guard across
    /// rayon threads, and the many read-only consumers (hover, completion,
    /// document-symbol, export fingerprinting, validation) don't serialize.
    pub(crate) info_service: parking_lot::RwLock<cwtools_info::InfoService>,
    /// pre-generated base-game type instances (from a vanilla cache OR a live
    /// index of `config.vanilla_dir`), merged into the workspace index so the
    /// editor resolves base-game references. Each instance keeps its real source
    /// path (raw, the driver / cache form) so goto-definition into base-game
    /// content lands in the right file once the merge maps it to a `file://` URI.
    #[allow(clippy::type_complexity)]
    pub(crate) vanilla_index: Mutex<Option<HashMap<String, Vec<(Arc<str>, TypeInstance)>>>>,
    /// The distinct source URIs the current vanilla contribution was merged
    /// under. Tracked so a re-merge (`cacheVanilla` / `clearAllCaches`) drops
    /// exactly the previous base-game instances in one index pass, without a
    /// `"<vanilla-cache>"` sentinel.
    pub(crate) vanilla_merged_uris: Mutex<HashSet<Arc<str>>>,
    /// Vanilla loc keys per language (display name -> lowercased keys), from the
    /// vanilla cache or extracted when rebuilding it. They stand in for the
    /// install's loc when there is no dir to read it from; when there is one,
    /// the first loc rebuild takes them and `vanilla_loc` below supersedes them.
    #[allow(clippy::type_complexity)]
    pub(crate) vanilla_loc_keys: Mutex<Option<Vec<(String, Vec<String>)>>>,
    /// Base-game file paths (relative, on-disk case), from the vanilla cache or
    /// a live index of the install. Staged here until the merge folds them into
    /// `info_service.type_index.file_index` together with the workspace root's
    /// files: CW113 resolves a `filepath` against mod and base game as one set,
    /// so the two halves have to land in the same write (#283).
    pub(crate) vanilla_file_paths: Mutex<Option<Vec<String>>>,
    /// Staged vanilla vars — see `VarIndex::vanilla_names` for provenance semantics.
    pub(crate) vanilla_var_names: Mutex<Option<Vec<String>>>,
    /// Staged base-game scripted-localisation names, same provenance split as
    /// `vanilla_var_names`. See `ScriptedLocIndex`.
    pub(crate) vanilla_scripted_loc_names: Mutex<Option<Vec<String>>>,
    /// Staged base-game scripted-GUI callback names.
    pub(crate) vanilla_scripted_gui_names: Mutex<Option<Vec<String>>>,
    /// The base-game install's loc keys, hover text and definition sites, read
    /// from disk on the first loc rebuild and reused for the rest of the session
    /// (#89) — vanilla is ~2000 loc files that cannot change while the editor is
    /// running. Paired with the inputs it was built for so a change to any of
    /// them rebuilds. Dropped by `cacheVanilla` / `clearAllCaches`, which is how
    /// a user who updated the game mid-session picks up the new files.
    #[allow(clippy::type_complexity)]
    pub(crate) vanilla_loc:
        Mutex<Option<(crate::scan::VanillaLocKey, Arc<crate::scan::VanillaLoc>)>>,
    /// loc-key index (workspace + vanilla) for CW100/CW122 on config files and
    /// for scope-aware loc-command checks. Rebuilt on each full workspace scan.
    /// `Arc` so snapshot clone is a pointer bump, not a deep copy of the union
    /// (485f0b). `loc_key_index` already uses `Arc` for the same reason.
    pub(crate) loc_index: parking_lot::RwLock<Option<Arc<cwtools_localization::LocIndex>>>,
    /// Sorted, prefix-searchable mirror of `loc_index`'s key union, built beside
    /// it on each scan so loc completion can binary-search for the typed token
    /// instead of sweeping all ~400K keys per keystroke. `None` until the first
    /// scan; completion falls back to the sweep until then. Held behind its own
    /// lock and only ever cloned out (an `Arc` bump), so it nests with nothing.
    pub(crate) loc_key_index: parking_lot::RwLock<Option<Arc<crate::completion::LocKeyIndex>>>,
    /// Display text per loc key (lowercased) → list of (language, display text).
    /// Built from the LocService during workspace scan so hover can show
    /// localisation without re-reading loc files. Outer quotes are stripped
    /// from the desc for cleaner display. Patched on every loc edit; a key
    /// shared across files can lose the other file's translations until the
    /// next scan.
    #[allow(clippy::type_complexity)]
    pub(crate) loc_text: parking_lot::RwLock<LocTextMap>,
    /// Definition site per loc key (lowercased) → (file URI, 0-based line). Built
    /// from the LocService during workspace scan so goto-definition on a
    /// `localisation` reference jumps to the `.yml` entry. One representative
    /// (primary-language) location per key is enough for navigation.
    pub(crate) loc_locations: parking_lot::RwLock<LocLocationMap>,
    /// Live per-file loc keys (lowercased) for currently-open loc files, keyed by
    /// URI. Overlays the scanned `loc_index` so a key added to (or present in) an
    /// open `.yml` resolves immediately in `$ref$` checks without waiting for a
    /// full rescan (#36). Bounded by the number of open loc files, so it stays
    /// tiny next to the global index. A key only removed from disk still resolves
    /// against the baseline `loc_index` until the next scan — the overlay only
    /// adds keys, it can't subtract from the baseline union.
    pub(crate) loc_live_overlay: parking_lot::RwLock<HashMap<String, HashSet<String>>>,
    /// Per-file loc keys (lowercased) for NON-open loc files changed on disk
    /// (watched events), keyed by URI — the watched-files counterpart of
    /// `loc_live_overlay`, with the same per-file replace semantics, unioned at
    /// the same query sites. Deliberately NOT cleared by a scan: the scan's
    /// index install is built from disk reads that may predate a watched
    /// change, so surviving the install is the point. Entries can go stale
    /// (still correct, just redundant with the index) and are bounded by the
    /// distinct watched files, like `watched_signatures`. Pruned per URI on a
    /// watched DELETE, the scan's on-disk prune, and when the doc opens (the
    /// open overlay owns it from then on). Taken after `loc_live_overlay` when
    /// both are held.
    pub(crate) loc_watched_overlay: parking_lot::RwLock<HashMap<String, HashSet<String>>>,
    /// Monotonic counter bumped on every mutation of either loc overlay, so the
    /// key sets derived from them can be cached. `info_revision` cannot stand in:
    /// an open loc edit updates the overlay BEFORE it bumps that counter, so a
    /// set keyed on it alone would keep serving a union missing the key just
    /// typed and flag it as undefined. Bumped by [`LocOverlayWrite`]'s `Drop`,
    /// under the write lock, so a new writer can't be missed.
    pub(crate) loc_overlay_revision: AtomicU64,
    /// Cached [`Backend::loc_overlay_keys`] union, keyed on
    /// `loc_overlay_revision`. Was rebuilt (and every key cloned) per validated
    /// file, which on a watched batch meant once per file in the batch.
    pub(crate) loc_overlay_keys_cache: parking_lot::Mutex<Option<(u64, Arc<HashSet<String>>)>>,
    /// Cached [`Backend::loc_ref_names`] set, keyed on `info_revision`.
    /// Holds only modifier and type names; loc overlays are cached separately.
    pub(crate) loc_ref_names_cache: parking_lot::Mutex<Option<(u64, Arc<HashSet<String>>)>>,
    /// When `false` (the default), hover shows localisation for the primary
    /// language only (the first of `config.loc_languages`, else English) and the
    /// `loc_text` map only stores that language. Set via the
    /// `hoverShowAllLanguages` init option. Storing one language keeps the map
    /// small; the user opts into all translations explicitly.
    pub(crate) hover_show_all_languages: std::sync::atomic::AtomicBool,
    /// Developer hover toggle (`hoverDebug` init option). When `true`, hover
    /// includes the raw rule classification (field/type/scope) lines; off by
    /// default so users see only localisation, description, and required scopes.
    pub(crate) hover_debug: std::sync::atomic::AtomicBool,
    /// When `true` (the `hover.scopeDisplay = "resolved"` setting), hover adds a
    /// `Resolves to` line showing the scope the hovered link/keyword evaluates to
    /// (run through `change_scope`), alongside the ambient current scope. Off by
    /// default — the ambient scope is shown alone. (#37)
    pub(crate) hover_resolved_scope: std::sync::atomic::AtomicBool,
    /// Loc-title inlay hints (`cwtools.inlayHints.locTitles` init option, default
    /// ON). When `true`, `textDocument/inlayHint` annotates a leaf whose value is
    /// a known type-instance id with its localised title. Read on each request.
    pub(crate) inlay_hints_loc_titles: std::sync::atomic::AtomicBool,
    /// Resolved-scope inlay hints (`cwtools.inlayHints.scopes` init option,
    /// default OFF). When true, `textDocument/inlayHint` annotates visible
    /// scope-changing blocks with their rule-aware resolved scope. See
    /// `inlay.rs`.
    pub(crate) inlay_hints_scopes: std::sync::atomic::AtomicBool,
    /// Whether the client advertised `hierarchicalDocumentSymbolSupport` at
    /// initialize. When `true`, documentSymbol returns a nested `DocumentSymbol`
    /// tree; otherwise it falls back to the flat `SymbolInformation` list.
    pub(crate) hierarchical_symbols: std::sync::atomic::AtomicBool,
    /// Whether the client advertised `workspace.workspaceEdit.documentChanges`
    /// at initialize. When `true`, rename emits versioned `documentChanges`
    /// (stale-buffer safe); otherwise the legacy `changes` map.
    pub(crate) workspace_edit_document_changes: std::sync::atomic::AtomicBool,
    /// Whether the client advertised `completionItem.labelDetailsSupport` at
    /// initialize. When `true`, deferred type/enum/alias items carry their
    /// origin as `labelDetails.description` at build time.
    pub(crate) completion_label_details: std::sync::atomic::AtomicBool,
    /// Whether the client advertised `window.workDoneProgress` at initialize.
    /// A server-initiated `$/progress` has to register its token with
    /// `window/workDoneProgress/create` first, which a client that didn't
    /// advertise support isn't obliged to answer — so the whole stream is gated
    /// on this. The custom `loadingBar` notification is sent regardless.
    pub(crate) client_work_done_progress: std::sync::atomic::AtomicBool,
    /// Whether the client advertised `workspace.semanticTokens.refreshSupport`.
    /// A refresh is a server-to-client request, so a client that lacks this
    /// capability may never answer and would hold the workspace scan open.
    pub(crate) semantic_tokens_refresh_support: std::sync::atomic::AtomicBool,
    /// Whether the client advertised `workspace.codeLens.refreshSupport`.
    pub(crate) code_lens_refresh_support: std::sync::atomic::AtomicBool,
    /// Whether the scan's `$/progress` token is currently live, so the phase
    /// updates pair one `begin` with one `end` on a token that exists.
    pub(crate) scan_progress_active: std::sync::atomic::AtomicBool,
    /// Cancel latches for the in-flight `workspace/executeCommand` calls that
    /// carried a `workDoneToken`, keyed by `command_progress::token_key`.
    /// `window/workDoneProgress/cancel` sets one; the scan polls it. Entries
    /// live exactly as long as their command.
    pub(crate) command_cancels: parking_lot::Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// Whether a scan has the loading indicator open, over both channels. The
    /// close is sent defensively from several places (a cancelled or panicked
    /// scan's `ScanGuard`, `cacheVanilla` after an index that may have been a
    /// cache hit), so it is gated on this to stay one close per open.
    pub(crate) loading_bar_active: std::sync::atomic::AtomicBool,
    /// `false` until the first full workspace scan has finished building the
    /// index. While `false`, per-file validation still parses and indexes, but
    /// suppresses published diagnostics (clears instead) so the user never sees
    /// transient "not found" errors for cross-file references whose defining file
    /// isn't indexed yet. The scan publishes the real diagnostics once the index
    /// is complete. Set `true` with no workspace folder (nothing to index).
    pub(crate) index_ready: std::sync::atomic::AtomicBool,
    /// `false` until `initialized`. tower-lsp drops outgoing notifications until
    /// then, so anything published during `initialize` never reaches the client.
    pub(crate) handshake_complete: std::sync::atomic::AtomicBool,
    /// Broken-`.cwt` diagnostics, parked because the rules load runs inside
    /// `initialize` where the gate above would swallow them (#98).
    pub(crate) deferred_rule_diagnostics: parking_lot::Mutex<Vec<(String, Vec<Diagnostic>)>>,
    /// The rules load's user-visible messages (per-error log lines, the error
    /// toast), parked the same way as the diagnostics above (#98).
    pub(crate) deferred_rules_messages: parking_lot::Mutex<Vec<DeferredRulesMessage>>,
    /// Order-independent key (sorted error strings) of the last rules-error
    /// set toasted this session. The client fires `reloadrulesconfig` at boot
    /// right after `initialize` already loaded the rules, so an unchanged
    /// error set must not toast twice; a different set (a real reload) still
    /// does.
    pub(crate) last_rules_toast: parking_lot::Mutex<Option<String>>,
    /// URIs the last rules load published diagnostics for. A load only publishes
    /// files that still have errors, so a repaired one needs an explicit clear or
    /// its squiggle outlives the problem.
    pub(crate) published_rule_uris: parking_lot::Mutex<HashSet<String>>,
    /// URIs the last localisation rebuild published diagnostics for. A file
    /// excluded or removed by the next rebuild needs an explicit empty publish.
    pub(crate) published_loc_uris: parking_lot::Mutex<HashSet<String>>,
    /// Monotonic edit counter, bumped on every `did_change`. A debounced
    /// validation captures the value at spawn time; the cross-file dependent
    /// sweep bails the moment a newer edit lands, so concurrent sweeps collapse
    /// into the latest one instead of stacking up and double-validating.
    /// Ordering is Relaxed: the counter only gates a staleness `!=` compare on
    /// data already protected by locks, so no happens-before edge is needed.
    pub(crate) edit_generation: AtomicU64,
    /// Per open document, the interned `.lower` ids of the identifier-like
    /// tokens it mentions (keys + string values from its parsed AST). Used by
    /// the dependent sweep to revalidate only the open docs that actually
    /// reference a changed export, instead of every open doc. A SOUND
    /// OVER-APPROXIMATION: when a doc's token set is missing, it's always
    /// included. Updated on did_open / did_change, removed on did_close.
    pub(crate) doc_tokens: parking_lot::RwLock<HashMap<String, HashSet<StringId>>>,
    /// Names that changed during a preempted dependent sweep. When a sweep is
    /// aborted because a newer edit landed, the union of names it was processing
    /// is merged here so the next sweep (triggered by the newer edit) drains and
    /// includes them, preventing stale dependents after rapid successive edits.
    /// A use-set change (see `type_uses`) queues its names here too, so the
    /// sweep also covers unused-instance (CW239/CW231) transitions.
    pub(crate) pending_changed_names: Mutex<HashSet<String>>,
    /// Per file, the `<type>` references it makes to instances of a tracked
    /// (`should_be_used`) type — the LSP's counterpart of the batch driver's
    /// merged [`cwtools_validation::references::UsedInstances`]. Seeded for
    /// every file by the workspace scan and replaced per file on each
    /// validation, so the merged view stays answerable between scans. Empty
    /// for a config with nothing tracked (the recording itself is gated on
    /// `needs_use_tracking`).
    pub(crate) type_uses: parking_lot::RwLock<HashMap<String, references::UsedInstances>>,
    /// Cached merge of every `type_uses` entry, keyed on `type_uses_revision`.
    /// Rebuilt only when some file's use set actually changed; the common
    /// keystroke (uses unchanged) hits the cache.
    #[allow(clippy::type_complexity)]
    pub(crate) type_uses_merged: parking_lot::Mutex<Option<(u64, Arc<references::UsedInstances>)>>,
    /// Bumped whenever a `type_uses` entry changes, invalidating the merge.
    pub(crate) type_uses_revision: AtomicU64,
    /// Set to `true` once the vanilla index has been loaded and merged into
    /// `info_service.type_index`. After the merge the raw `vanilla_index` data
    /// is dropped to eliminate double residency; this flag prevents
    /// `ensure_vanilla_index` from re-running on subsequent workspace scans.
    pub(crate) vanilla_merged: std::sync::atomic::AtomicBool,
    /// Guards `validate_entire_workspace` against re-entrant scans. The
    /// startup scan, `clearAllCaches`, and (in a later phase) a periodic
    /// background rescan all funnel through it; without this, two overlapping
    /// scans would race serial `info_service` writes against each other.
    /// `compare_exchange`-guarded on entry; a losing caller logs and returns
    /// immediately instead of queueing behind the running scan.
    pub(crate) scan_in_progress: AtomicBool,
    /// Per-URI validation task. A unique id lets a completed predecessor remove
    /// itself without deleting the replacement a newer edit installed.
    pub(crate) debounce_handles: Mutex<HashMap<String, DebounceTask>>,
    pub(crate) next_debounce_id: AtomicU64,
    /// Detached per-document validation is outside tower-lsp's request limit.
    /// This semaphore bounds the parser/validator work a burst can start.
    pub(crate) validation_permits: tokio::sync::Semaphore,
    /// Monotonic counter bumped on every mutation of `info_service` or
    /// `rules` (the two state sources the fallback completion cache depends
    /// on). The completion handler reads this on each request; when
    /// the value matches a cached entry, it can return the cached list
    /// without walking `info.files` again. Hot in the half-typed case: the
    /// user is in a state where the AST is stale and every completion
    /// falls through to the fallback, but info/rules haven't moved since
    /// the last build, so the cache hit saves a full workspace walk.
    ///
    /// A single-file reindex bumps this only when that file's export
    /// fingerprint moved (`index_parsed_file`). Every consumer is built from
    /// the type-instance names, defined variables and saved event targets the
    /// fingerprint covers, so an edit inside a rule body leaves both caches
    /// valid. Ruleset loads, index prunes and deletions still bump
    /// unconditionally: they change inputs no per-file fingerprint describes.
    pub(crate) info_revision: AtomicU64,
    /// Cached fallback list (the flat type/enum/var dump reached when
    /// context-aware matching returns nothing).
    pub(crate) fallback_cache: parking_lot::Mutex<Option<CompletionCacheEntry>>,
    /// `(uri, version, ast)` of the last mid-edit re-parse `ast_snapshot_for`
    /// did when the document had no stored AST yet. Hover, goto, completion,
    /// semantic tokens and inlay hints all fire off one keystroke and each
    /// re-parsed the whole file independently in that window. One entry is
    /// enough: they are all for the focused document. Dropped on `did_close`.
    #[allow(clippy::type_complexity)]
    pub(crate) fresh_ast_cache: parking_lot::Mutex<Option<(String, i32, Arc<ParsedFile>)>>,
    /// Per-URI marker for in-flight completion requests. Each new
    /// `completion` request stores a unique id; the request checks the marker
    /// before doing any heavy work and bails if it was replaced or removed. Avoids
    /// stacking N parallel AST walks when the user types fast — only the
    /// latest one matters, the rest are wasted work.
    pub(crate) completion_generation: parking_lot::Mutex<HashMap<String, u64>>,
    pub(crate) next_completion_id: AtomicU64,
    /// Stat-only signature (path, size, mtime) over the loc files a scan last
    /// rebuilt, so the periodic background pass can skip
    /// `rebuild_and_publish_loc` (the biggest transient cost of a scan) when
    /// nothing loc-related has changed on disk. `None` until the first scan
    /// runs.
    pub(crate) last_loc_signature: parking_lot::Mutex<Option<u64>>,
    /// Cached shared-discovery result for the workspace root, so a
    /// code-action request (fired on cursor movement) doesn't re-walk the whole
    /// tree when nothing loc-related changed on disk. `(root, files, sig)` where
    /// `sig` is the scan's `last_loc_signature` value at population time: the
    /// cache is valid only while that still matches the scan's current value
    /// (a cheap read, no walk), which catches `.yaml`/`.csv` loc changes the
    /// client watcher misses (it only watches `*.yml`) and clients that send no
    /// watched events. Watched create/delete events invalidate immediately.
    /// `sig` stores the scan's value, NOT a freshly-computed signature, so a
    /// watched-event re-walk doesn't leave the cache permanently mismatched
    /// against the scan's stale value. `None` until the first code-action
    /// request populates it.
    #[allow(clippy::type_complexity)]
    pub(crate) loc_discovery_cache:
        parking_lot::Mutex<Option<(std::path::PathBuf, Vec<std::path::PathBuf>, Option<u64>)>>,
    /// `(stat_signature_for(walked files), settings_generation)` stored after
    /// the last successful full pass. A QUIET pass whose freshly-computed pair
    /// matches this short-circuits the whole reindex. `None` until the first
    /// pass; never stored for an empty walk (a transiently-unreadable root).
    pub(crate) last_scan_fingerprint: parking_lot::Mutex<Option<(u64, u64)>>,
    /// Pass-2 cancel tests park here. Production never installs one.
    #[cfg(test)]
    pub(crate) pass2_gate: parking_lot::Mutex<Option<Arc<Pass2Gate>>>,
    /// Bumped whenever a rules or config change could alter validation output,
    /// folded into `last_scan_fingerprint` so such a change forces the next
    /// quiet pass to run. `SeqCst`: rare writer, single reader, so ordering
    /// cost doesn't matter — chosen for clarity.
    pub(crate) settings_generation: AtomicU64,
    /// Server start time, the epoch `last_activity_ms` is measured against.
    pub(crate) start: std::time::Instant,
    /// Milliseconds since `start` at the last `did_change` / `completion`
    /// request — the idle clock the background reindex loop watches.
    /// `Relaxed`: the periodic loop is the only reader and tolerates a
    /// slightly-stale value.
    pub(crate) last_activity_ms: AtomicU64,
    /// URIs of non-open watched files (create/modify) waiting for the next
    /// coalescing window. A burst of `didChangeWatchedFiles` events (git
    /// checkout, generator, AV/OneDrive churn) collapses into one drain
    /// instead of validating 1:1 on the message future and starving the
    /// bounded request queue (#90).
    pub(crate) watched_pending: Mutex<HashSet<String>>,
    /// URIs of watched files DELETED since the last drain, coalesced into the
    /// same window as `watched_pending` instead of clearing inline per event.
    /// A URI that also arrived as a CHANGED/CREATED this window is treated as
    /// a change, not a delete.
    pub(crate) watched_deleted: Mutex<HashSet<String>>,
    /// The single in-flight watched-batch window, if one is armed. A live
    /// (not-yet-finished) handle means a window is already scheduled, so a
    /// continuous event stream can't keep pushing the trailing window back.
    pub(crate) watched_debounce: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Per-URI stat signature (file size, mtime-nanos) of the last watched
    /// validation — the per-file analogue of `last_loc_signature`. A CHANGED
    /// event whose bytes never moved (cloud sync, git, the running game
    /// rewriting identical content) matches and skips the revalidate. A DELETE
    /// drops the entry; a URI with no entry always validates.
    pub(crate) watched_signatures: Mutex<HashMap<String, (u64, u128)>>,
    /// Per-URI cached semantic tokens for `full/delta` support. `None` when never
    /// requested or after invalidation (file change, rename, encoding switch,
    /// rules reload). Bounded by distinct URIs that requested tokens.
    pub(crate) semantic_tokens_cache: Mutex<HashMap<String, SemanticCacheEntry>>,
    pub(crate) semantic_tokens_seq: AtomicU64,
    /// Per-URI span edits of the diagnostics currently published for that file
    /// (parser-convention ranges — the same `SpanEdit` shape a `SuggestedFix`
    /// carries), tagged with the diagnostic's code and the source version or
    /// content hash the edits were computed against. Replaced wholesale on
    /// every `publish_filtered` call (scan, keystroke, loc rebuild), so it
    /// always matches what the client's Problems panel shows; a URI with
    /// nothing fixable has no entry. Backs the `fixAllWorkspace` command: it
    /// snapshots this store instead of re-running validation, so "fix all in
    /// the workspace" fixes exactly the diagnostics currently visible and
    /// drops stale entries. CW100's create-key fix is excluded by construction
    /// (its payload carries no span edits, see `SuggestedFix::create_loc_key`) —
    /// `genlocall` covers mass stub generation instead.
    pub(crate) fixable_edits: Mutex<HashMap<String, FixableEdits>>,
    /// Summary captured from the most recent completed workspace scan:
    /// total files validated, files carrying an error, and counts by severity.
    /// Updated only after a successful pass so a `validateWorkspace` command
    /// can return a result without re-running the scan again.
    pub(crate) last_scan_summary: Mutex<Option<ScanSummary>>,
    /// Closed workspace files whose diagnostics were last published by the
    /// scan. Used to clear stale entries when a file is deleted, ignored, or
    /// pushed off the closed-file diagnostic budget on a later scan.
    pub(crate) published_workspace_uris: Mutex<HashSet<String>>,
}

/// Write access to a loc overlay that bumps `loc_overlay_revision` on drop,
/// while the write lock is still held — so a reader that takes the lock after
/// this writer always sees the new counter alongside the new contents.
///
/// Every overlay mutation must go through [`Backend::loc_live_overlay_mut`] or
/// [`Backend::loc_watched_overlay_mut`] rather than the `RwLock` directly: a
/// write that skipped the bump would leave `loc_overlay_keys` /
/// `loc_ref_names` serving a cached union without the key that just changed,
/// which reads to the user as a loc diagnostic on a key they can see in the
/// file.
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

/// One cached completion list. Stored behind a `Mutex<Option<_>>` so the
/// completion handler can swap a freshly built list in on cache miss without
/// holding any other lock.
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
    /// `Arc` so every reader that only needs to look at the text (completion,
    /// hover, the cross-file dependent sweep) clones a refcount bump instead
    /// of the whole document under the `documents` lock.
    pub(crate) text: Arc<str>,
    /// Shared so the cross-file dependent sweep can validate against it without
    /// re-parsing (an `Arc` clone instead of a full re-parse per open file).
    pub(crate) ast: Option<Arc<ParsedFile>>,
    /// Document version the cached AST was parsed from. `None` means there is
    /// no cached AST; a value different from `version` means completion/hover
    /// are looking at the last good parse while debounce validation catches up.
    pub(crate) ast_version: Option<i32>,
    /// Source size represented by the cached AST. A stale AST keeps this much
    /// of the aggregate document budget charged after a smaller broken edit.
    pub(crate) ast_source_bytes: usize,
    /// Versioned parsed localisation and its lowercased `$ref$` set.
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
/// lines go to the client's output channel, `Toast` to a popup.
pub(crate) enum DeferredRulesMessage {
    Log(String),
    Toast(String),
}

/// What kicked off a `parse_and_validate` call. Threaded through so the
/// `[validate]` log names its trigger, which makes a validate storm's source
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

/// What `did_close` found on disk when it re-read the file the buffer just
/// released. Three states, not two: only `Absent` means "gone", and only "gone"
/// may drop the file's index entry.
pub(crate) enum DiskState {
    /// `discarded_edits` is set when the disk text differs from the buffer that
    /// just closed, i.e. the user closed it with unsaved changes. Anything
    /// derived from that buffer describes content that never hit disk.
    Parsed {
        parsed: ParsedFile,
        discarded_edits: bool,
    },
    /// Nothing safe to index: deleted, unreadable, refused, or no longer parses.
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
