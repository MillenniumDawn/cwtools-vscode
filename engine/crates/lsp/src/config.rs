use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::Value;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use cwtools_info::check_path_dir;
use cwtools_rules::rules_types::RuleSet;
use cwtools_rules::ruleset_loader::load_ruleset_from_dir;
use cwtools_validation::build_scope_registry_arc;

use crate::Backend;
use crate::cache_purge::purge_caches;
use crate::command_progress::{CommandProgress, Phase, ScanOutcome};
use crate::paths::default_cache_dir;

/// Maximum entries accepted from a single ignore array in the init/didChange
/// payload, and maximum length of one glob, in chars. Longer input is cut at
/// the boundary so a hostile or accidental config can't grow the per-scan
/// matcher work without bound (#169).
const MAX_IGNORE_ENTRIES: usize = 200;
const MAX_IGNORE_PATTERN_LEN: usize = 1024;
const MAX_IGNORED_ERROR_CODES: usize = 200;

/// Pull `ignoreFilePatterns` and `ignoreDirectories` arrays out of a
/// `serde_json::Value` (the `initializationOptions` payload and the
/// `workspace/didChangeConfiguration` payload share the same shape).
/// Returns the two lists. Filters non-string and empty entries; truncates
/// past [`MAX_IGNORE_ENTRIES`] per list and drops globs longer than
/// [`MAX_IGNORE_PATTERN_LEN`], with a warning naming the key.
pub(crate) fn extract_ignore_patterns(opts: &Value) -> (Vec<String>, Vec<String>) {
    (
        extract_bounded_string_list(
            opts,
            "ignoreFilePatterns",
            MAX_IGNORE_ENTRIES,
            MAX_IGNORE_PATTERN_LEN,
        ),
        extract_bounded_string_list(
            opts,
            "ignoreDirectories",
            MAX_IGNORE_ENTRIES,
            MAX_IGNORE_PATTERN_LEN,
        ),
    )
}

/// Shared bounded extraction for the string-array settings. Non-string and
/// empty entries are filtered; a list longer than `max_entries` keeps its
/// first `max_entries`, and an entry longer than `max_entry_len` chars is
/// dropped. Both cuts log a warning naming the key.
fn extract_bounded_string_list(
    opts: &Value,
    key: &str,
    max_entries: usize,
    max_entry_len: usize,
) -> Vec<String> {
    let Some(arr) = opts.get(key).and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    if arr.len() > max_entries {
        tracing::warn!(
            key,
            count = arr.len(),
            max = max_entries,
            "truncating list to the first {} entries",
            max_entries
        );
    }
    let mut out = Vec::new();
    for v in arr.iter().take(max_entries) {
        let Some(s) = v.as_str() else { continue };
        if s.is_empty() {
            continue;
        }
        if s.chars().count() > max_entry_len {
            tracing::warn!(
                key,
                len = s.chars().count(),
                max = max_entry_len,
                "dropping entry over length limit"
            );
            continue;
        }
        out.push(s.to_string());
    }
    out
}

/// Pull `ignoredErrorCodes` (diagnostic codes the user suppressed via
/// `errors.ignore`) out of the shared init/didChange payload. Lowercased so the
/// publish-time filter compares case-insensitively; non-string and empty
/// entries are dropped, and the list is truncated past [`MAX_IGNORED_ERROR_CODES`].
pub(crate) fn extract_ignored_error_codes(opts: &Value) -> Vec<String> {
    extract_bounded_string_list(
        opts,
        "ignoredErrorCodes",
        MAX_IGNORED_ERROR_CODES,
        MAX_IGNORE_PATTERN_LEN,
    )
    .into_iter()
    .map(|s| s.to_ascii_lowercase())
    .collect()
}

/// Read an optional non-negative integer setting from the shared
/// init/didChange payload. Absent → `None` silently; present but not a u64
/// (string, float, negative) → `None` with a warning naming the key and the
/// received value, so a mistyped setting doesn't just vanish.
pub(crate) fn extract_u64_setting(opts: &Value, key: &str) -> Option<u64> {
    let v = opts.get(key)?;
    let parsed = v.as_u64();
    if parsed.is_none() {
        tracing::warn!(key, value = %v, "ignoring setting: expected a non-negative integer");
    }
    parsed
}

fn extract_localisation_languages(opts: &Value) -> Option<Option<Vec<cwtools_localization::Lang>>> {
    let value = opts.get("localisationLanguages")?;
    let Some(languages) = value.as_array() else {
        tracing::warn!(value = %value, "ignoring localisationLanguages: expected an array");
        return None;
    };
    let parsed = languages
        .iter()
        .filter_map(|value| value.as_str())
        .filter_map(cwtools_localization::Lang::from_name)
        .collect::<Vec<_>>();
    Some((!parsed.is_empty()).then_some(parsed))
}

fn extract_bool_setting(opts: &Value, key: &str) -> Option<bool> {
    let value = opts.get(key)?;
    let parsed = value.as_bool();
    if parsed.is_none() {
        tracing::warn!(key, value = %value, "ignoring setting: expected a boolean");
    }
    parsed
}

fn apply_formatting_settings(
    opts: &Value,
    mut current: cwtools_parser::format::FormatOptions,
) -> cwtools_parser::format::FormatOptions {
    if let Some(style) = opts.get("formattingIndentStyle").and_then(|v| v.as_str()) {
        match style {
            "tab" => current.indent_style = cwtools_parser::format::IndentStyle::Tab,
            "space" => current.indent_style = cwtools_parser::format::IndentStyle::Space,
            _ => tracing::warn!(
                value = style,
                "ignoring formattingIndentStyle: expected space or tab"
            ),
        }
    }
    if let Some(size) = extract_u64_setting(opts, "formattingIndentSize") {
        current.indent_size = (size as u32).clamp(1, 16);
    }
    if let Some(trim) = extract_bool_setting(opts, "formattingTrimTrailingWhitespace") {
        current.trim_trailing_whitespace = trim;
    }
    if let Some(newline) = extract_bool_setting(opts, "formattingInsertFinalNewline") {
        current.insert_final_newline = newline;
    }
    current
}

fn extract_hover_scope_display(opts: &Value) -> Option<bool> {
    let value = opts.get("hoverScopeDisplay")?;
    match value.as_str() {
        Some("context") => Some(false),
        Some("resolved") => Some(true),
        _ => {
            tracing::warn!(value = %value, "ignoring hoverScopeDisplay: expected context or resolved");
            None
        }
    }
}

/// Decode workspace-folder URIs to filesystem paths, for the access boundary's
/// root list. Strict on purpose: a folder URI that isn't a `file:` URI
/// contributes no root, where the lax converter would turn
/// `http://localhost/` into `/` and authorize the whole filesystem. One that
/// doesn't resolve on disk is dropped later, when `refresh_roots`
/// canonicalizes it.
fn folders_to_paths(uris: &[String]) -> Vec<std::path::PathBuf> {
    uris.iter()
        .filter_map(|uri| crate::access::file_uri_to_path(uri))
        .collect()
}

/// The display language the client asked for, as a BCP-47 tag.
///
/// LSP 3.16's `locale` is the one to use: `vscode-languageclient` fills it from
/// `vscode.env.language` with no help from the extension. A client that sends
/// neither can pass the same tag in `initializationOptions.locale` instead,
/// which is also the seam a test drives.
fn locale_tag(params: &InitializeParams) -> Option<&str> {
    params.locale.as_deref().or_else(|| {
        params
            .initialization_options
            .as_ref()
            .and_then(|opts| opts.get("locale"))
            .and_then(|v| v.as_str())
    })
}

/// The user-visible `reloadrulesconfig` status. The client toasts this string
/// verbatim, so the wording is the contract: each half (rules loaded or not,
/// revalidation ran / queued / still pending) must report honestly.
fn reload_status_message(loaded: bool, outcome: ScanOutcome, dir: &std::path::Path) -> String {
    let status = cwtools_i18n::t(match outcome {
        ScanOutcome::Ran => cwtools_i18n::Key::StatusRevalidated,
        // The rules themselves are live either way; only the re-validation
        // against them is outstanding, and the two wordings say whether
        // anything is still going to land.
        ScanOutcome::Cancelled => cwtools_i18n::Key::StatusRevalidationCancelled,
        ScanOutcome::Busy if loaded => cwtools_i18n::Key::StatusRevalidationQueued,
        ScanOutcome::Busy => cwtools_i18n::Key::StatusRevalidationPending,
    });
    if loaded {
        cwtools_i18n::format(cwtools_i18n::Key::CommandRulesReloaded, &[status])
    } else {
        cwtools_i18n::format(
            cwtools_i18n::Key::CommandNoRulesLoaded,
            &[&dir.display().to_string(), status],
        )
    }
}

/// Render one localisation stub file for `lang` covering every `missing` key,
/// as `{language, filename_suggestion, content}`. Standard Paradox loc shape:
/// an `l_<lang>:` header then ` KEY:0 "TODO"` entries. The file needs a UTF-8
/// BOM on save — the client prepends it — so the suggested name is the only
/// server-side hint the caller writes it as a `_l_<lang>.yml`.
fn render_loc_stub(lang: cwtools_localization::Lang, missing: &BTreeSet<String>) -> Value {
    let mut content = format!("l_{}:\n", lang);
    for key in missing {
        content.push_str(&format!(" {}:0 \"TODO\"\n", key));
    }
    serde_json::json!({
        "language": lang.to_string(),
        "filename_suggestion": format!("generated_l_{}.yml", lang),
        "content": content,
    })
}

impl Backend {
    /// Install a freshly-loaded ruleset and rebuild the cached scope registry to
    /// match it. The registry depends only on `(ruleset, game)`; building it here
    /// (once per load) keeps it out of the per-file validation hot path. The
    /// ruleset + registry live in one `rules` guard so they never disagree.
    pub(crate) fn set_ruleset(&self, ruleset: RuleSet) {
        let game = self.state.config.read().game();
        // Build the registry and the cached var-effects before taking any of the
        // ruleset-family locks, so the write section is short.
        let registry = build_scope_registry_arc(&ruleset, game);
        // Cache the variable-defining effects so per-file indexing can collect
        // value_set[variable] names (and values) for the CW246 / VariableGetField
        // checks and for hover/goto.
        let var_effects = cwtools_info::variable_defining_effects(&ruleset);
        // Lock order: rules -> info_service.
        let mut rules = self.state.rules.write();
        rules.ruleset = Some(Arc::new(ruleset));
        rules.scope_registry = registry;
        self.state
            .info_service
            .write()
            .update_ruleset_data(var_effects);
        drop(rules);
        self.bump_info_revision();
        // Bump the quiet-pass fingerprint generation: a new ruleset changes
        // validation output, even though reloadrulesconfig also rescans right away.
        self.state
            .settings_generation
            .fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) async fn initialize_impl(
        &self,
        params: InitializeParams,
    ) -> Result<InitializeResult> {
        // Distinctive banner so it's unmistakable in the Output panel WHICH server
        // is running. If you don't see this line, you're on an old/F# binary.
        self.client
            .log_message(
                MessageType::INFO,
                format!("★ CWTools Rust LSP server v{}", env!("CARGO_PKG_VERSION")),
            )
            .await;
        // Display language, for everything the server says back. Set here,
        // before the first scan, so nothing user-facing is built in the wrong
        // language.
        if let Some(tag) = locale_tag(&params) {
            let locale = cwtools_i18n::Locale::from_tag(tag);
            cwtools_i18n::set_locale(locale);
            self.client
                .log_message(
                    MessageType::INFO,
                    format!("locale: {} (from {})", locale.tag(), tag),
                )
                .await;
        }

        // Store language from init options
        if let Some(opts) = &params.initialization_options {
            if let Some(lang) = opts.get("language").and_then(|v| v.as_str()) {
                self.state.config.write().language = lang.to_string();
                self.client
                    .log_message(MessageType::INFO, format!("language: {}", lang))
                    .await;
            }

            // Optional list of loc languages to validate (e.g. ["english"]).
            // Unknown/empty entries are ignored; an empty resulting list leaves
            // scoping off (validate all languages). See `loc_languages`.
            if let Some(arr) = opts.get("localisationLanguages").and_then(|v| v.as_array()) {
                let langs: Vec<cwtools_localization::Lang> = arr
                    .iter()
                    .filter_map(|v| v.as_str())
                    .filter_map(cwtools_localization::Lang::from_name)
                    .collect();
                if !langs.is_empty() {
                    self.client
                        .log_message(
                            MessageType::INFO,
                            format!("localisation languages scoped to: {:?}", langs),
                        )
                        .await;
                    self.state.config.write().loc_languages = Some(langs);
                }
            }

            // Whether hover shows all loc languages or just the primary one.
            if let Some(all) = opts.get("hoverShowAllLanguages").and_then(|v| v.as_bool()) {
                self.state
                    .hover_show_all_languages
                    .store(all, std::sync::atomic::Ordering::Relaxed);
            }

            // Developer hover: when on, include the raw rule classification
            // (field / type / scope) lines. Off by default — most users only
            // want the localisation, description, and required scopes.
            if let Some(dbg) = opts.get("hoverDebug").and_then(|v| v.as_bool()) {
                self.state
                    .hover_debug
                    .store(dbg, std::sync::atomic::Ordering::Relaxed);
            }

            // Scope display: "resolved" adds a `Resolves to` line (the scope the
            // hovered link/keyword evaluates to); "context" (default) shows only
            // the ambient current scope. (#37)
            if let Some(mode) = opts.get("hoverScopeDisplay").and_then(|v| v.as_str()) {
                self.state
                    .hover_resolved_scope
                    .store(mode == "resolved", std::sync::atomic::Ordering::Relaxed);
            }

            // Inlay hints. Loc-title hints (`cwtools.inlayHints.locTitles`) default
            // ON; resolved-scope hints (`cwtools.inlayHints.scopes`) default OFF.
            // Absent leaves the constructor defaults untouched. Read once at init,
            // matching the hover toggles above.
            if let Some(on) = opts.get("inlayHintsLocTitles").and_then(|v| v.as_bool()) {
                self.state
                    .inlay_hints_loc_titles
                    .store(on, std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(on) = opts.get("inlayHintsScopes").and_then(|v| v.as_bool()) {
                self.state
                    .inlay_hints_scopes
                    .store(on, std::sync::atomic::Ordering::Relaxed);
            }
            {
                let mut cfg = self.state.config.write();
                cfg.formatting = apply_formatting_settings(opts, cfg.formatting);
            }

            // Persistent cache directory for the base-game index (so it isn't
            // re-parsed every startup). The client should pass its global
            // storage path; we fall back to an OS cache dir otherwise.
            if let Some(cd) = opts.get("cacheDir").and_then(|v| v.as_str()) {
                self.state.config.write().cache_dir = Some(std::path::PathBuf::from(cd));
            }

            // Minutes between quiet background re-index passes (0 disables).
            // A live change comes through `did_change_configuration_impl`.
            if let Some(mins) = extract_u64_setting(opts, "backgroundReindexIntervalMinutes") {
                self.state
                    .config
                    .write()
                    .background_reindex_interval_minutes = mins;
            }

            // Seconds of user inactivity a background pass waits for (default
            // 15). A live change comes through `did_change_configuration_impl`
            // and applies on the next reindex cycle.
            if let Some(secs) = extract_u64_setting(opts, "backgroundReindexIdleSeconds") {
                self.state.config.write().background_reindex_idle_seconds = secs;
            }

            // Whether to publish diagnostics for closed workspace files. The
            // default keeps the Problems panel up to date across the whole mod;
            // turning it off scopes diagnostics to open documents only.
            if let Some(wide) = extract_bool_setting(opts, "workspaceWideDiagnostics") {
                self.state.config.write().workspace_wide_diagnostics = wide;
            }
            self.client
                .log_message(MessageType::INFO, format!("init options: {:?}", opts))
                .await;

            // Load a pre-generated vanilla cache if provided, so the editor
            // resolves base-game references (sprites, operation_tokens, …)
            // without re-parsing the install. Merged into the index in
            // validate_entire_workspace.
            if let Some(vc) = opts.get("vanillaCache").and_then(|v| v.as_str()) {
                match cwtools_info::vanilla_cache::load(std::path::Path::new(vc)) {
                    Ok((game, _fingerprint, data)) => {
                        let total = self.stage_vanilla_payload(data);
                        self.client
                            .log_message(
                                MessageType::INFO,
                                format!(
                                    "Loaded {} base-game instances from vanilla cache {} (game {})",
                                    total, vc, game
                                ),
                            )
                            .await;
                    }
                    Err(e) => {
                        self.client
                            .log_message(
                                MessageType::WARNING,
                                format!("Could not load vanilla cache {}: {}", vc, e),
                            )
                            .await;
                    }
                }
            }

            // A raw base-game install dir (like the CLI's `--vanilla`). Stored
            // here and indexed lazily on the first full-workspace scan, so the
            // editor resolves base-game references without a pre-built cache.
            if let Some(vd) = opts.get("vanilla").and_then(|v| v.as_str()) {
                let p = std::path::PathBuf::from(vd);
                if p.is_dir() {
                    {
                        let mut cfg = self.state.config.write();
                        cfg.vanilla_dir = Some(p);
                        cfg.refresh_roots();
                    }
                    self.client
                        .log_message(MessageType::INFO, format!("Base-game dir set: {}", vd))
                        .await;
                } else {
                    self.client
                        .log_message(
                            MessageType::WARNING,
                            format!("`vanilla` dir does not exist: {}", vd),
                        )
                        .await;
                }
            }

            // Load .cwt rules from rulesCache if provided. Retain the dir so the
            // `reloadrulesconfig` command can re-read it later without a restart.
            if let Some(cache) = opts.get("rulesCache").and_then(|v| v.as_str()) {
                let cache_path = std::path::PathBuf::from(cache);
                {
                    let mut cfg = self.state.config.write();
                    cfg.rules_dir = Some(cache_path.clone());
                    cfg.refresh_roots();
                }
                self.load_rules_config(&cache_path).await;
            }
        }

        // Store workspace URI: prefer workspace_folders (multi-root aware), fall
        // back to the legacy root_uri field for clients that only send that.
        // Canonicalised like the document URIs (#319): `workspace_prefix` is
        // derived from the primary folder and stripped off canonical document
        // URIs by a plain compare, so a client spelling that differs from
        // `path_to_uri` — VS Code's `file:///d%3A/mod` against the round trip's
        // `file:///D:/mod` — would leave every logical path unstripped.
        let folders: Vec<String> = match &params.workspace_folders {
            Some(folders) if !folders.is_empty() => folders
                .iter()
                .map(|f| crate::paths::canonical_uri(f.uri.as_str()))
                .collect(),
            _ => params
                .root_uri
                .iter()
                .map(|u| crate::paths::canonical_uri(u.as_str()))
                .collect(),
        };
        if let Some(root) = folders.first() {
            let mut cfg = self.state.config.write();
            cfg.workspace_prefix = Some(crate::paths::workspace_prefix_of(root));
            cfg.workspace_uri = Some(root.as_str().into());
            // The rest of the server only knows the primary folder; the whole
            // list exists so the access boundary doesn't refuse files in a
            // multi-root window's other folders.
            cfg.workspace_roots = folders_to_paths(&folders);
            cfg.refresh_roots();
        }

        // Per-workspace ignore globs from the extension. The extension
        // forwards `cwtools.ignore.filePatterns` and `cwtools.ignore.directories`
        // into initializationOptions on first launch; runtime updates come
        // through `workspace/didChangeConfiguration` and re-apply the same
        // helper. We layer these on top of the engine's hard-coded baseline
        // (Changelog.txt, README.*, LICENSE.*, *.md) — user patterns extend,
        // they don't replace.
        if let Some(opts) = &params.initialization_options {
            let (files, dirs) = extract_ignore_patterns(opts);
            let codes = extract_ignored_error_codes(opts);
            if !files.is_empty() || !dirs.is_empty() || !codes.is_empty() {
                let (n_files, n_dirs, n_codes) = (files.len(), dirs.len(), codes.len());
                {
                    let mut cfg = self.state.config.write();
                    cfg.ignore_file_patterns = files;
                    cfg.ignore_dir_patterns = dirs;
                    cfg.ignored_error_codes = codes;
                }
                self.client
                    .log_message(
                        MessageType::INFO,
                        format!(
                            "ignore patterns: {} files, {} dirs, {} suppressed codes (engine defaults still apply)",
                            n_files, n_dirs, n_codes,
                        ),
                    )
                    .await;
            }
        }

        // Negotiate position encoding. The parser counts Unicode scalar values
        // (chars), which equal UTF-32 code units, so advertise utf-32 when the
        // client lists it — that client then gets exact columns on non-BMP
        // lines for free. Clients that don't advertise utf-32 (VS Code) stay on
        // the LSP default (utf-16), so their behavior is unchanged.
        let position_encoding = params
            .capabilities
            .general
            .as_ref()
            .and_then(|g| g.position_encodings.as_ref())
            .filter(|encs| encs.contains(&PositionEncodingKind::UTF32))
            .map(|_| PositionEncodingKind::UTF32);
        self.state.config.write().position_encoding = position_encoding
            .clone()
            .unwrap_or(PositionEncodingKind::UTF16);

        // documentSymbol: return a nested tree only when the client advertises
        // support; otherwise the flat SymbolInformation list is served.
        let hierarchical = params
            .capabilities
            .text_document
            .as_ref()
            .and_then(|td| td.document_symbol.as_ref())
            .and_then(|ds| ds.hierarchical_document_symbol_support)
            .unwrap_or(false);
        self.state
            .hierarchical_symbols
            .store(hierarchical, Ordering::Relaxed);

        // completion: origin labels next to deferred type/enum/alias items,
        // only when the client can render them.
        let label_details = params
            .capabilities
            .text_document
            .as_ref()
            .and_then(|td| td.completion.as_ref())
            .and_then(|c| c.completion_item.as_ref())
            .and_then(|ci| ci.label_details_support)
            .unwrap_or(false);
        self.state
            .completion_label_details
            .store(label_details, Ordering::Relaxed);

        // rename: versioned documentChanges only when the client advertises
        // support; otherwise the legacy `changes` map is served.
        let document_changes = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|w| w.workspace_edit.as_ref())
            .and_then(|we| we.document_changes)
            .unwrap_or(false);
        self.state
            .workspace_edit_document_changes
            .store(document_changes, Ordering::Relaxed);

        // `$/progress`: only usable when the client says it will answer
        // `window/workDoneProgress/create`. See `scan::send_work_done_progress`.
        let work_done_progress = params
            .capabilities
            .window
            .as_ref()
            .and_then(|w| w.work_done_progress)
            .unwrap_or(false);
        self.state
            .client_work_done_progress
            .store(work_done_progress, Ordering::Relaxed);

        let semantic_tokens_refresh = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|w| w.semantic_tokens.as_ref())
            .and_then(|s| s.refresh_support)
            .unwrap_or(false);
        self.state
            .semantic_tokens_refresh_support
            .store(semantic_tokens_refresh, Ordering::Relaxed);

        let code_lens_refresh = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|w| w.code_lens.as_ref())
            .and_then(|c| c.refresh_support)
            .unwrap_or(false);
        self.state
            .code_lens_refresh_support
            .store(code_lens_refresh, Ordering::Relaxed);

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                position_encoding,
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        will_save: None,
                        will_save_wait_until: None,
                        save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                            include_text: Some(false),
                        })),
                    },
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    // `completionItem/resolve` fills in `documentation`/`detail`
                    // for the one item the client focuses, deferred out of the
                    // initial list to shrink every response (perf/completion-
                    // responsiveness) — see `completion::resolve`.
                    resolve_provider: Some(true),
                    trigger_characters: Some(vec![
                        "=".to_string(),
                        "<".to_string(),
                        "[".to_string(),
                        "$".to_string(),
                        "#".to_string(),
                    ]),
                    work_done_progress_options: Default::default(),
                    all_commit_characters: None,
                    completion_item: None,
                }),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: Some(true),
                }),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![
                        "getFileTypes".to_string(),
                        "exportProfilingLog".to_string(),
                        "cacheVanilla".to_string(),
                        "clearAllCaches".to_string(),
                        "reloadrulesconfig".to_string(),
                        "genlocall".to_string(),
                        "fixAllWorkspace".to_string(),
                        "formatWorkspace".to_string(),
                        "reindexWorkspace".to_string(),
                        "validateWorkspace".to_string(),
                        // The extension greys out its graph commands unless it
                        // finds this name here (`graphAvailability.ts`).
                        "getGraphData".to_string(),
                    ],
                    // Tells the client it may pass a `workDoneToken` with
                    // `workspace/executeCommand` and get phase + percentage
                    // reports against it, plus a Cancel button that actually
                    // stops the work (`window/workDoneProgress/cancel`). The
                    // extension feature-detects on this before threading a
                    // token, so against an older server it keeps its own
                    // indeterminate indicator instead.
                    work_done_progress_options: WorkDoneProgressOptions {
                        work_done_progress: Some(true),
                    },
                }),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                document_highlight_provider: Some(OneOf::Left(true)),
                selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
                // Filepath/icon leaves as clickable links; targets are built
                // up-front in the handler, so no resolve step.
                document_link_provider: Some(DocumentLinkOptions {
                    resolve_provider: Some(false),
                    work_done_progress_options: Default::default(),
                }),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
                // Quick-fixes from diagnostics that carry a `SuggestedFix`
                // payload (CW253/CW282/CW280/CW121/CW281/CW268). No resolve
                // step: the WorkspaceEdit is built up-front in the handler.
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        code_action_kinds: Some(vec![
                            CodeActionKind::QUICKFIX,
                            // `source.fixAll` is what `editor.codeActionsOnSave`
                            // binds to; without it in this list no client ever
                            // asks for it.
                            CodeActionKind::SOURCE_FIX_ALL,
                        ]),
                        resolve_provider: Some(false),
                        work_done_progress_options: Default::default(),
                    },
                )),
                // Inlay hints: declared statically (loc-title hints default on).
                // The handler gates each kind on its setting and returns nothing
                // when both are off, so a client always-on capability is harmless.
                inlay_hint_provider: Some(OneOf::Left(true)),
                // Semantic tokens: `full` (with delta) and `range`. `range` skips
                // entities outside the viewport; `delta` sends only the changed
                // integer slice after a large edit, with a per-URI result cache
                // invalidated on file change, rename, rules reload, and full
                // reindex (#184). A `workspace/semanticTokens/refresh` is sent
                // after bulk index changes so the client re-requests visible files.
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: crate::semantic::legend(),
                            full: Some(SemanticTokensFullOptions::Delta { delta: Some(true) }),
                            range: Some(true),
                            work_done_progress_options: Default::default(),
                        },
                    ),
                ),
                // Document colours: `color = { … }` leaves get an inline swatch
                // and the native picker. `colorPresentation` re-reads the source
                // span so the picker writes back the convention it found.
                color_provider: Some(ColorProviderCapability::Simple(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                document_range_formatting_provider: Some(OneOf::Left(true)),
                // Multi-root: the server tracks one primary folder (the first),
                // so a folder change re-points it and re-scans.
                workspace: Some(WorkspaceServerCapabilities {
                    workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                        supported: Some(true),
                        change_notifications: Some(OneOf::Left(true)),
                    }),
                    file_operations: Some(WorkspaceFileOperationsServerCapabilities {
                        did_create: Some(FileOperationRegistrationOptions {
                            filters: vec![FileOperationFilter {
                                scheme: Some("file".to_string()),
                                pattern: FileOperationPattern {
                                    glob: "**/*.{txt,gui,gfx,asset,yml,cwt}".to_string(),
                                    matches: None,
                                    options: None,
                                },
                            }],
                        }),
                        will_create: None,
                        did_rename: Some(FileOperationRegistrationOptions {
                            filters: vec![FileOperationFilter {
                                scheme: Some("file".to_string()),
                                pattern: FileOperationPattern {
                                    glob: "**/*.{txt,gui,gfx,asset,yml,cwt}".to_string(),
                                    matches: None,
                                    options: None,
                                },
                            }],
                        }),
                        will_rename: None,
                        did_delete: Some(FileOperationRegistrationOptions {
                            filters: vec![FileOperationFilter {
                                scheme: Some("file".to_string()),
                                pattern: FileOperationPattern {
                                    glob: "**/*.{txt,gui,gfx,asset,yml,cwt}".to_string(),
                                    matches: None,
                                    options: None,
                                },
                            }],
                        }),
                        will_delete: None,
                    }),
                }),
                // `position_encoding` (above): utf-32 when the client supports
                // it, else the LSP default (utf-16). The parser counts chars,
                // so on utf-16 clients column offsets are off by the number of
                // astral code points on a line; utf-32 clients get exact
                // columns since UTF-32 code units equal Unicode scalar values.
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "cwtools-server".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    /// Load the `.cwt` rules from `cache_path`, publish any parse errors as
    /// per-file diagnostics plus a popup, and (on success) install the ruleset
    /// and rebuild the modifier-key set. Shared by `initialize` and the
    /// `reloadrulesconfig` command so a live reload behaves exactly like startup.
    /// Returns whether a non-empty ruleset was loaded.
    pub(crate) async fn load_rules_config(&self, cache_path: &std::path::Path) -> bool {
        // Surface a missing rules dir explicitly. The client may hand us a
        // path that doesn't resolve here (e.g. a Windows `rules_folder`
        // that didn't normalise), which otherwise degrades silently to a
        // generic "no rules loaded" with an empty error list.
        if !cache_path.is_dir() {
            self.client
                .log_message(
                    MessageType::WARNING,
                    format!("`rulesCache` dir does not exist: {}", cache_path.display()),
                )
                .await;
        }
        let dir = cache_path.to_path_buf();
        let table = self.state.string_table.clone();
        let loaded = tokio::task::spawn_blocking(move || {
            load_ruleset_from_dir(
                &dir,
                &table,
                cwtools_file_manager::file_manager::ScanBudget::default(),
            )
        })
        .await;
        let (combined_ruleset, parse_errors) = match loaded {
            Ok(result) => result,
            Err(error) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!("Rules config load failed: {error}"),
                    )
                    .await;
                return false;
            }
        };

        // Broken .cwt rules silently degrade every downstream check, so they are
        // reported three ways: the log, a popup, and a diagnostic per file. All
        // three are user-visible and all can run inside `initialize`, where
        // tower-lsp drops outgoing notifications — so each defers through the
        // handshake gate below (#98). Snapshotting once is sound only while the
        // park sites run await-free from here: an `.await` between a stale
        // `false` and a park would let the `initialized` flush slip past and
        // strand the parked message forever.
        let handshake_complete = self
            .state
            .handshake_complete
            .load(std::sync::atomic::Ordering::Relaxed);
        if handshake_complete {
            for err in &parse_errors {
                self.client
                    .log_message(MessageType::ERROR, err.to_string())
                    .await;
            }
        } else {
            self.state.deferred_rules_messages.lock().extend(
                parse_errors
                    .iter()
                    .map(|err| crate::DeferredRulesMessage::Log(err.to_string())),
            );
        }
        let mut diags_by_file: std::collections::HashMap<String, Vec<Diagnostic>> =
            std::collections::HashMap::new();
        for err in &parse_errors {
            // Shared with the live per-file CWT lint (#43). No file text
            // here to widen the squiggle, so pass no line info.
            diags_by_file
                .entry(crate::paths::path_to_uri(&err.file))
                .or_default()
                .push(crate::validate::rule_parse_error_to_diagnostic(
                    err,
                    &crate::validate::DocLines::none(),
                ));
        }
        let mut to_publish: Vec<(String, Vec<Diagnostic>)> = diags_by_file.into_iter().collect();
        // A load only reports files that still have errors, so anything reported
        // last time and absent now has been repaired and needs an explicit clear.
        {
            let current: std::collections::HashSet<String> =
                to_publish.iter().map(|(uri, _)| uri.clone()).collect();
            let mut previous = self.state.published_rule_uris.lock();
            let open = self.state.documents.lock();
            to_publish.extend(
                previous
                    .difference(&current)
                    // An open editor buffer owns its diagnostics: the live `.cwt`
                    // lint republishes it, and clearing here would blank a dirty
                    // buffer's squiggles until the next keystroke.
                    .filter(|uri| !open.contains_key(*uri))
                    .map(|uri| (uri.clone(), Vec::new())),
            );
            *previous = current;
        }

        // Dropped on the floor before `initialized`, so park them for the
        // handshake to flush (#98).
        if handshake_complete {
            for (uri, diags) in to_publish {
                if let Ok(url) = uri.parse() {
                    self.client.publish_diagnostics(url, diags, None).await;
                }
            }
        } else {
            self.state
                .deferred_rule_diagnostics
                .lock()
                .extend(to_publish);
        }
        if let Some(first) = parse_errors.first() {
            // Inline the first error: the client never auto-reveals its output
            // channel (RevealOutputChannelOn.Never), so the popup is the only
            // part a user is guaranteed to see.
            let summary = format!(
                "CWTools: {} rules-config error(s), first: {first}",
                parse_errors.len()
            );
            // Dedupe on the full error set, order-independent: `first` follows
            // read_dir traversal order, so the same set could summarize
            // differently across the boot double-load, and two different sets
            // can share a count and a first error.
            let dedupe_key = {
                let mut errs: Vec<String> = parse_errors.iter().map(|e| e.to_string()).collect();
                errs.sort_unstable();
                errs.join("\n")
            };
            let is_new = {
                let mut last = self.state.last_rules_toast.lock();
                if last.as_deref() == Some(dedupe_key.as_str()) {
                    false
                } else {
                    *last = Some(dedupe_key);
                    true
                }
            };
            if is_new {
                // Re-read the gate: a toast parked after the flush ran would sit
                // forever while the dedupe key above already claimed it.
                if self
                    .state
                    .handshake_complete
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    self.client.show_message(MessageType::ERROR, summary).await;
                } else {
                    self.state
                        .deferred_rules_messages
                        .lock()
                        .push(crate::DeferredRulesMessage::Toast(summary));
                }
            }
        } else {
            // A clean load forgets the last toast, so the same errors coming
            // back later in the session toast again.
            *self.state.last_rules_toast.lock() = None;
        }

        let loaded = !combined_ruleset.types.is_empty()
            || !combined_ruleset.enums.is_empty()
            || !combined_ruleset.aliases.is_empty()
            || !combined_ruleset.root_rules.is_empty();

        if loaded {
            self.client
                .log_message(
                    MessageType::INFO,
                    format!(
                        "Loaded rules from {} ({} types, {} enums, {} aliases, {} errors)",
                        cache_path.display(),
                        combined_ruleset.types.len(),
                        combined_ruleset.enums.len(),
                        combined_ruleset.aliases.len(),
                        parse_errors.len(),
                    ),
                )
                .await;
            self.set_ruleset(combined_ruleset);
            // Rebuild modifier_keys now that the ruleset is loaded.
            // The type index is empty at this point; it will be rebuilt
            // again after validate_entire_workspace with the full index.
            self.rebuild_modifier_keys();
            // Rule-driven semantic tokens change globally; invalidate the delta
            // cache so edits do not patch stale data and tell the client to
            // re-request tokens for visible files (#184).
            self.invalidate_all_semantic_tokens();
            self.request_semantic_refresh().await;
        } else {
            self.client
                .log_message(
                    MessageType::WARNING,
                    format!(
                        "No rules loaded from {}. Errors: {:?}",
                        cache_path.display(),
                        parse_errors
                    ),
                )
                .await;
        }
        loaded
    }

    /// React to a workspace folder being added or removed.
    ///
    /// The server tracks ONE primary folder (`config.workspace_uri`, the first
    /// one at initialize) and derives the logical paths, the file scan root and
    /// the type index from it. So the contained behaviour is: re-point that
    /// folder at the first survivor and re-index. A workspace whose FIRST folder
    /// is unchanged therefore only re-indexes; it does not gain the other
    /// folders' content. Indexing several roots at once needs the scan and the
    /// logical-path derivation to become multi-root, which is a bigger change
    /// than this handler.
    pub(crate) async fn did_change_workspace_folders_impl(
        &self,
        params: DidChangeWorkspaceFoldersParams,
    ) {
        // Both lists in the canonical spelling `initialize_impl` stored, so the
        // removal compare below still recognises the primary folder and a new
        // primary keys its `workspace_prefix` the way documents are keyed (#319).
        let removed: Vec<String> = params
            .event
            .removed
            .iter()
            .map(|f| crate::paths::canonical_uri(f.uri.as_str()))
            .collect();
        let added: Vec<String> = params
            .event
            .added
            .iter()
            .map(|f| crate::paths::canonical_uri(f.uri.as_str()))
            .collect();
        let current = self.state.config.read().workspace_uri.clone();
        let current = current.as_deref().map(str::to_string);

        // Re-point only when the primary folder itself went away; otherwise the
        // root the index was built from is still valid.
        let next = match &current {
            Some(uri) if removed.iter().any(|r| r == uri) => added.first().cloned(),
            None => added.first().cloned(),
            _ => current.clone(),
        };
        if next != current {
            match &next {
                Some(uri) => {
                    let mut cfg = self.state.config.write();
                    cfg.workspace_prefix = Some(crate::paths::workspace_prefix_of(uri));
                    cfg.workspace_uri = Some(uri.as_str().into());
                }
                None => {
                    let mut cfg = self.state.config.write();
                    cfg.workspace_prefix = None;
                    cfg.workspace_uri = None;
                }
            }
        }
        // The access boundary tracks every folder, not just the primary one, so
        // it follows add/remove even when the primary is untouched.
        {
            let removed_paths = folders_to_paths(&removed);
            let mut cfg = self.state.config.write();
            cfg.workspace_roots.retain(|r| !removed_paths.contains(r));
            for path in folders_to_paths(&added) {
                if !cfg.workspace_roots.contains(&path) {
                    cfg.workspace_roots.push(path);
                }
            }
            cfg.refresh_roots();
            self.state
                .workspace_roots_generation
                .fetch_add(1, Ordering::Release);
        }
        let open_uris: Vec<String> = {
            let documents = self.state.documents.lock();
            documents.keys().cloned().collect()
        };
        for uri in open_uris {
            if self.is_workspace_document(&uri) {
                continue;
            }
            if let Ok(uri) = Url::parse(&uri) {
                tower_lsp::LanguageServer::did_close(
                    self,
                    DidCloseTextDocumentParams {
                        text_document: TextDocumentIdentifier { uri },
                    },
                )
                .await;
            }
        }
        self.client
            .log_message(
                MessageType::INFO,
                format!(
                    "workspace folders changed (+{} / -{}); primary folder: {}",
                    added.len(),
                    removed.len(),
                    next.as_deref().unwrap_or("<none>"),
                ),
            )
            .await;
        if next.is_none() {
            return;
        }
        // A full rescan, the same path `reindexWorkspace` takes. The generation
        // bump makes the quiet-pass fingerprint stale so the scan can't
        // short-circuit on an unchanged file set from the OLD root.
        self.state
            .settings_generation
            .fetch_add(1, Ordering::SeqCst);
        // `validate_entire_workspace`'s CAS guard returns false when a scan is
        // already running — including the startup scan this notification often
        // races. That scan indexed the old root, so retry until we win the CAS,
        // bounded so a perpetually-busy server reports instead of spinning.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut rescanned = self.validate_entire_workspace(false).await;
        while !rescanned && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            rescanned = self.validate_entire_workspace(false).await;
        }
        if !rescanned {
            self.client
                .log_message(
                    MessageType::WARNING,
                    "workspace folders changed but a scan stayed in progress; index still points at the previous folder",
                )
                .await;
        }
    }

    /// Re-read ignore globs and the background-reindex interval/idle window
    /// when the extension's `cwtools.*` settings change. The shape mirrors
    /// what we accept in `initializationOptions`: the payload is the
    /// `cwtools` namespace object, with optional `ignoreFilePatterns`,
    /// `ignoreDirectories`, `backgroundReindexIntervalMinutes`, and
    /// `backgroundReindexIdleSeconds` — each absent-means-keep, so a partial
    /// payload only touches the keys it carries. The next full-workspace scan
    /// (or reindex cycle) picks up the new values; an in-flight scan finishes
    /// with the snapshot it took.
    pub(crate) async fn did_change_configuration_impl(&self, params: DidChangeConfigurationParams) {
        // The client may send either the whole `cwtools` section (when the
        // section is registered via `configurationSection`) or just the
        // changed slice. `extract_ignore_patterns` looks for the same two
        // keys at the top level — works in both cases. Every key here is
        // absent-means-keep (unlike initialize, where absent means empty):
        // a partial payload carrying only the reindex keys must not wipe
        // the ignore lists. The shipped VS Code client always sends its
        // full section, so this only matters for other clients.
        let (files, dirs) = extract_ignore_patterns(&params.settings);
        let files = params.settings.get("ignoreFilePatterns").map(|_| files);
        let dirs = params.settings.get("ignoreDirectories").map(|_| dirs);
        let codes = params
            .settings
            .get("ignoredErrorCodes")
            .map(|_| extract_ignored_error_codes(&params.settings));
        let counts = (
            files.as_ref().map(Vec::len),
            dirs.as_ref().map(Vec::len),
            codes.as_ref().map(Vec::len),
        );
        let reindex_minutes =
            extract_u64_setting(&params.settings, "backgroundReindexIntervalMinutes");
        let reindex_idle_secs =
            extract_u64_setting(&params.settings, "backgroundReindexIdleSeconds");
        let localisation_languages = extract_localisation_languages(&params.settings);
        let hover_all_languages = extract_bool_setting(&params.settings, "hoverShowAllLanguages");
        let hover_debug = extract_bool_setting(&params.settings, "hoverDebug");
        let hover_resolved_scope = extract_hover_scope_display(&params.settings);
        let workspace_wide_diagnostics =
            extract_bool_setting(&params.settings, "workspaceWideDiagnostics");
        {
            let mut cfg = self.state.config.write();
            cfg.formatting = apply_formatting_settings(&params.settings, cfg.formatting);
        }

        let (
            current_loc_languages,
            current_files,
            current_dirs,
            current_codes,
            current_reindex_minutes,
            current_reindex_idle_secs,
            current_workspace_wide,
        ) = {
            let cfg = self.state.config.read();
            (
                cfg.loc_languages.clone(),
                cfg.ignore_file_patterns.clone(),
                cfg.ignore_dir_patterns.clone(),
                cfg.ignored_error_codes.clone(),
                cfg.background_reindex_interval_minutes,
                cfg.background_reindex_idle_seconds,
                cfg.workspace_wide_diagnostics,
            )
        };
        let (current_hover_all, current_hover_debug, current_hover_resolved_scope) = (
            self.state.hover_show_all_languages.load(Ordering::Relaxed),
            self.state.hover_debug.load(Ordering::Relaxed),
            self.state.hover_resolved_scope.load(Ordering::Relaxed),
        );
        let unchanged = files.as_ref().is_none_or(|files| files == &current_files)
            && dirs.as_ref().is_none_or(|dirs| dirs == &current_dirs)
            && codes.as_ref().is_none_or(|codes| codes == &current_codes)
            && reindex_minutes.is_none_or(|minutes| minutes == current_reindex_minutes)
            && reindex_idle_secs.is_none_or(|seconds| seconds == current_reindex_idle_secs)
            && localisation_languages
                .as_ref()
                .is_none_or(|languages| languages == &current_loc_languages)
            && hover_all_languages.is_none_or(|all| all == current_hover_all)
            && hover_debug.is_none_or(|debug| debug == current_hover_debug)
            && hover_resolved_scope.is_none_or(|resolved| resolved == current_hover_resolved_scope)
            && workspace_wide_diagnostics.is_none_or(|wide| wide == current_workspace_wide);
        if unchanged {
            tracing::debug!("didChangeConfiguration: no relevant change; skipping revalidate");
            return;
        }

        let ignore_changed = files.as_ref().is_some_and(|files| files != &current_files)
            || dirs.as_ref().is_some_and(|dirs| dirs != &current_dirs);
        let localisation_changed = localisation_languages
            .as_ref()
            .is_some_and(|languages| languages != &current_loc_languages);
        let hover_all_changed = hover_all_languages.is_some_and(|all| all != current_hover_all);
        let workspace_wide_changed =
            workspace_wide_diagnostics.is_some_and(|wide| wide != current_workspace_wide);
        {
            // Any field written here must join the comparison above, or an
            // identical re-send of a changed field will slip past the guard.
            let mut cfg = self.state.config.write();
            if let Some(files) = files {
                cfg.ignore_file_patterns = files;
            }
            if let Some(dirs) = dirs {
                cfg.ignore_dir_patterns = dirs;
            }
            if let Some(codes) = codes {
                cfg.ignored_error_codes = codes;
            }
            if let Some(mins) = reindex_minutes {
                cfg.background_reindex_interval_minutes = mins;
            }
            if let Some(secs) = reindex_idle_secs {
                cfg.background_reindex_idle_seconds = secs;
            }
            if let Some(wide) = workspace_wide_diagnostics {
                cfg.workspace_wide_diagnostics = wide;
            }
            if let Some(languages) = localisation_languages {
                cfg.loc_languages = languages;
            }
        }
        if let Some(all) = hover_all_languages {
            self.state
                .hover_show_all_languages
                .store(all, Ordering::Relaxed);
        }
        if let Some(debug) = hover_debug {
            self.state.hover_debug.store(debug, Ordering::Relaxed);
        }
        if let Some(resolved) = hover_resolved_scope {
            self.state
                .hover_resolved_scope
                .store(resolved, Ordering::Relaxed);
        }
        if ignore_changed {
            *self.state.loc_discovery_cache.lock() = None;
        }
        // Bump the quiet-pass fingerprint generation: ignore globs or suppressed
        // codes may have changed, so the next background pass must re-run.
        self.state
            .settings_generation
            .fetch_add(1, Ordering::SeqCst);
        let (n_files, n_dirs, n_codes) = counts;
        tracing::info!(
            file_globs = ?n_files,
            dir_globs = ?n_dirs,
            ignored_codes = ?n_codes,
            reindex_minutes = ?reindex_minutes,
            reindex_idle_secs = ?reindex_idle_secs,
            "config updated via didChangeConfiguration"
        );
        // Localisation settings shape the project-wide loc index and hover
        // text, so they need the same serialized full scan as startup. A scan
        // already in progress may have passed its loc phase; queue one behind
        // it instead of racing a second rebuild.
        if ignore_changed || localisation_changed || hover_all_changed || workspace_wide_changed {
            if !self.validate_entire_workspace(false).await {
                self.spawn_deferred_revalidation("didChangeConfiguration");
            }
        } else if self.state.index_ready.load(Ordering::Relaxed) {
            self.revalidate_all_open_docs(crate::ValidateTrigger::ConfigChange)
                .await;
        }
    }

    pub(crate) async fn execute_command_impl(
        &self,
        params: ExecuteCommandParams,
    ) -> Result<Option<Value>> {
        // A client that wants a progress bar and a Cancel button passes a
        // `workDoneToken`; every long command below opens its stream against
        // that token. `None` (an older extension, or an editor that doesn't
        // bother) falls through to the server's own `loadingBar` indicator and
        // uncancellable behaviour, exactly as before.
        let token = params.work_done_progress_params.work_done_token.clone();
        match params.command.as_str() {
            "getFileTypes" => {
                if let Some(uri_val) = params.arguments.first() {
                    let uri = uri_val.as_str().unwrap_or("");
                    let types = self.determine_file_types(uri).await;
                    let arr: Vec<Value> = types.into_iter().map(Value::String).collect();
                    return Ok(Some(Value::Array(arr)));
                }
                Ok(Some(Value::Array(vec![])))
            }
            "exportProfilingLog" => Ok(Some(Value::String(
                cwtools_profiling::export_profiling_log(),
            ))),
            // Re-index the base-game install and re-write the vanilla cache,
            // even when a fresh-looking cache exists.
            "cacheVanilla" => self.cache_vanilla_command(token).await,
            // Purge every on-disk cache (parse cache + vanilla caches), drop the
            // in-memory vanilla state, and re-scan the workspace from scratch.
            "clearAllCaches" => self.clear_all_caches_command(token).await,
            // Re-read the rules-config dir from disk, rebuild the ruleset, and
            // re-validate the whole workspace against it — no server restart.
            "reloadrulesconfig" => self.reload_rules_config_command(token).await,
            // Generate localisation stubs for every missing `## required` loc key
            // and hand them back to the client to open for review (no files are
            // written server-side).
            // Not cancellable: this is one synchronous sweep of indexes already
            // in memory, with no seam to stop at and nothing long enough to
            // want one.
            "genlocall" => {
                let progress =
                    CommandProgress::begin(self, token, "CWTools: Generate missing loc", false)
                        .await;
                let stubs = self.generate_missing_loc();
                progress.finish(None).await;
                Ok(Some(Value::Array(stubs)))
            }
            // Apply every currently-fixable diagnostic across the workspace in
            // one `workspace/applyEdit`, mirroring `cwtools fix --apply`. See
            // `code_action::fix_all_workspace_impl`.
            "fixAllWorkspace" => Ok(Some(Value::String(self.fix_all_workspace_impl().await))),
            "formatWorkspace" => Ok(Some(Value::String(self.format_workspace_impl(token).await))),
            // User-triggered re-index (no cache purge, unlike clearAllCaches).
            // validate_entire_workspace's CAS guard returns false when a scan
            // (the startup scan's tail, another reindex, the periodic background
            // pass) is already running. The same race the startup scan's closing
            // `loadingBar(false)` notification creates for `reloadrulesconfig`
            // hits this command too: the bar-off goes out before the guard drops
            // (`ScanGuard::finish`), so a reindex sent right after the bar-off
            // can land in the gap, lose the CAS, and answer immediately without
            // ever sending `loadingBar(true)`. Retry until we win the CAS so the
            // user's reindex actually runs, bounded so a perpetually-busy
            // server reports honestly instead of spinning.
            // User-triggered re-index (no cache purge, unlike clearAllCaches).
            "reindexWorkspace" => self.reindex_workspace_command(token).await,
            // Run a full workspace validation and return a summary:
            // total files, files with errors, and counts by severity. The scan
            // respects the current `workspaceWideDiagnostics` setting, so the
            // summary is always complete even when the Problems panel is
            // capped.
            "validateWorkspace" => self.validate_workspace_command(token).await,
            // `getGraphData(entityType, depth)` — the entity graph the webview
            // renders. See `graph.rs` for the wire format and the bounds.
            "getGraphData" => self.get_graph_data(&params.arguments).await,
            // An error, not a silent `Ok(None)`: the VS Code client renders a
            // null result as success, masking client/engine version drift.
            other => Err(tower_lsp::jsonrpc::Error::invalid_params(format!(
                "unknown command: {other}"
            ))),
        }
    }

    /// Clear all in-memory base-game state (staged vanilla + live var_index).
    fn clear_vanilla_state(&self) {
        self.state.vanilla_merged.store(false, Ordering::SeqCst);
        *self.state.vanilla_index.lock() = None;
        *self.state.vanilla_loc_keys.lock() = None;
        *self.state.vanilla_file_paths.lock() = None;
        *self.state.vanilla_var_names.lock() = None;
        *self.state.vanilla_scripted_loc_names.lock() = None;
        *self.state.vanilla_scripted_gui_names.lock() = None;
        *self.state.vanilla_loc.lock() = None;
        {
            let mut info = self.state.info_service.write();
            let type_index = Arc::make_mut(&mut info.type_index);
            type_index.var_index.clear_vanilla_names();
            type_index.scripted_loc_index.set_vanilla_names(Vec::new());
            type_index.scripted_gui_index.set_vanilla_names(Vec::new());
        }
        self.bump_info_revision();
    }

    /// Reset per-scan state that ties diagnostics to the current workspace
    /// snapshot. Called by operations that invalidate the existing index so
    /// the next scan does not try to clear URIs from a previous workspace.
    fn reset_scan_publication_state(&self) {
        *self.state.last_scan_summary.lock() = None;
        self.state.published_workspace_uris.lock().clear();
    }

    /// `cacheVanilla`: re-index the base-game install and re-write the vanilla
    /// cache, even when a fresh-looking cache exists.
    ///
    /// The in-memory base-game state is dropped only once the rebuild is
    /// actually about to start, so a cancel that lands before then leaves the
    /// server exactly as it found it.
    ///
    /// The bar is not cancellable: the rebuild is a single engine call over the
    /// whole base game with no per-file seam to poll at, so once it is under
    /// way there is nothing a Cancel button could do.
    async fn cache_vanilla_command(&self, token: Option<ProgressToken>) -> Result<Option<Value>> {
        let progress =
            CommandProgress::begin(self, token, "CWTools: Rebuild base-game cache", false).await;
        if progress.is_cancelled() {
            let msg = "Cancelled before the rebuild started.".to_string();
            progress.finish(Some(msg.clone())).await;
            return Ok(Some(Value::String(msg)));
        }
        progress.report_phase(Phase::Vanilla).await;
        self.clear_vanilla_state();
        // ensure_vanilla_index turns the loading bar on but, unlike a full
        // workspace scan, this command never reaches the code that turns
        // it off. The guard covers both exits: the normal one below, and
        // the client cancelling the command mid-index (#204).
        let guard = crate::scan::ScanGuard::for_command(self);
        self.ensure_vanilla_index(Some(&progress), true, false)
            .await;
        tokio::task::block_in_place(|| self.merge_pending_vanilla_index());
        self.rebuild_modifier_keys();
        guard.finish().await;
        // The base-game index is one opaque engine call with no per-file seam,
        // so a cancel raised during it is only observable now — and by now the
        // rebuild it would have stopped has already finished. Report what
        // actually happened rather than the cancel the user asked for.
        let msg = "Vanilla cache rebuilt.".to_string();
        progress.finish(Some(msg.clone())).await;
        Ok(Some(Value::String(msg)))
    }

    /// `clearAllCaches`: purge every on-disk cache, drop the in-memory
    /// base-game state, and re-scan the workspace from scratch.
    async fn clear_all_caches_command(
        &self,
        token: Option<ProgressToken>,
    ) -> Result<Option<Value>> {
        let progress =
            CommandProgress::begin(self, token, "CWTools: Clear all caches and reindex", true)
                .await;
        if progress.is_cancelled() {
            let msg = "Cancelled; no caches were cleared.".to_string();
            progress.finish(Some(msg.clone())).await;
            return Ok(Some(Value::String(msg)));
        }
        progress.report_phase(Phase::Discover).await;
        let dir = self
            .state
            .config
            .read()
            .cache_dir
            .clone()
            .or_else(default_cache_dir);
        let (removed, failures) = match dir {
            Some(dir) => tokio::task::block_in_place(|| purge_caches(&dir)),
            None => (0, Vec::new()),
        };
        // Dropped here rather than before the purge: from this line until the
        // re-index rebuilds it the server resolves no base-game reference, so
        // the window a cancel could strand it in is as narrow as it can be.
        self.clear_vanilla_state();
        self.reset_scan_publication_state();
        // A `Busy` scan (e.g. the periodic background pass) started before this
        // purge and may already be past its vanilla-index phase, so it can't be
        // trusted to rebuild what we just dropped — retry until we win the CAS
        // and actually re-index, bounded so a perpetually-busy server reports
        // honestly instead of hanging forever.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        let mut outcome = self
            .validate_entire_workspace_tracked(false, Some(&progress))
            .await;
        while outcome == ScanOutcome::Busy && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            // Cancel has to break the retry too: this loop can spin for three
            // minutes, and a user watching a progress bar that long is exactly
            // the one reaching for the button.
            if progress.is_cancelled() {
                outcome = ScanOutcome::Cancelled;
                break;
            }
            outcome = self
                .validate_entire_workspace_tracked(false, Some(&progress))
                .await;
        }
        let status = cwtools_i18n::t(match outcome {
            ScanOutcome::Ran => cwtools_i18n::Key::StatusReindexed,
            ScanOutcome::Busy => cwtools_i18n::Key::StatusReindexPending,
            ScanOutcome::Cancelled => {
                // The purge already happened and the in-memory base-game index
                // is gone with it, so stopping here would serve "not found" for
                // every vanilla reference until the next background pass. Hand
                // the rebuild to the same bounded background retry
                // `reloadrulesconfig` uses: cancelling should cost the user
                // their wait, not their diagnostics.
                self.spawn_deferred_revalidation("clearAllCaches");
                cwtools_i18n::Key::StatusReindexCancelledRebuilding
            }
        });
        let msg = if failures.is_empty() {
            cwtools_i18n::format(
                cwtools_i18n::Key::CommandCachesCleared,
                &[&removed.to_string(), status],
            )
        } else {
            cwtools_i18n::format(
                cwtools_i18n::Key::CommandCachesClearedWithErrors,
                &[
                    &removed.to_string(),
                    &failures.len().to_string(),
                    status,
                    &failures.join("; "),
                ],
            )
        };
        progress.finish(Some(msg.clone())).await;
        Ok(Some(Value::String(msg)))
    }

    /// `reloadrulesconfig`: re-read the rules-config dir from disk, rebuild the
    /// ruleset, and re-validate the whole workspace against it.
    async fn reload_rules_config_command(
        &self,
        token: Option<ProgressToken>,
    ) -> Result<Option<Value>> {
        let dir = self.state.config.read().rules_dir.clone();
        let Some(dir) = dir else {
            return Ok(Some(Value::String(
                cwtools_i18n::t(cwtools_i18n::Key::CommandNoRulesDirectory).to_string(),
            )));
        };
        let progress =
            CommandProgress::begin(self, token, "CWTools: Reload config rules", true).await;
        let loaded = self.load_rules_config(&dir).await;
        // The client fires this command right after the startup scan's loading
        // bar ends, but the bar-off notification is sent before the guard
        // drops, so the reload races the tail of that scan — whose diagnostics
        // were produced with no rules loaded. Retry until we win the CAS,
        // bounded so a perpetually-busy server reports honestly rather than
        // spinning.
        // `CWTOOLS_RETRY_DEADLINE_MS` test override (like `CWTOOLS_SCAN_HOLD_MS`):
        // shorten the bound so a test can prove the give-up path without
        // waiting out 60s.
        let deadline = std::time::Instant::now()
            + std::env::var("CWTOOLS_RETRY_DEADLINE_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .map_or(
                    std::time::Duration::from_secs(60),
                    std::time::Duration::from_millis,
                );
        let mut outcome = self
            .validate_entire_workspace_tracked(false, Some(&progress))
            .await;
        while outcome == ScanOutcome::Busy && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if progress.is_cancelled() {
                outcome = ScanOutcome::Cancelled;
                break;
            }
            outcome = self
                .validate_entire_workspace_tracked(false, Some(&progress))
                .await;
        }
        // The competing scan outlived the response bound. Rules are already
        // live, so hand the revalidation to a bounded background retry that
        // lands it once the scan releases, instead of leaving the stale
        // no-rules diagnostics until the next edit. A failed rules load changes
        // nothing, so there is nothing to defer then — and a cancel is the user
        // saying stop, which a background retry would ignore.
        if outcome == ScanOutcome::Busy && loaded {
            self.spawn_deferred_revalidation("reloadrulesconfig");
        }
        let msg = reload_status_message(loaded, outcome, &dir);
        progress.finish(Some(msg.clone())).await;
        Ok(Some(Value::String(msg)))
    }

    /// `reindexWorkspace`: user-triggered re-index, no cache purge.
    async fn reindex_workspace_command(
        &self,
        token: Option<ProgressToken>,
    ) -> Result<Option<Value>> {
        let progress =
            CommandProgress::begin(self, token, "CWTools: Re-index workspace", true).await;
        // `Busy` is surfaced unless we win the CAS within the give-up window:
        // unlike `clearAllCaches`, this command changes no state that must stay
        // coherent, so once the current scan runs the user can retry.
        let deadline = std::time::Instant::now()
            + std::env::var("CWTOOLS_RETRY_DEADLINE_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .map_or(
                    std::time::Duration::from_secs(60),
                    std::time::Duration::from_millis,
                );
        let mut outcome = self
            .validate_entire_workspace_tracked(false, Some(&progress))
            .await;
        while outcome == ScanOutcome::Busy && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if progress.is_cancelled() {
                outcome = ScanOutcome::Cancelled;
                break;
            }
            outcome = self
                .validate_entire_workspace_tracked(false, Some(&progress))
                .await;
        }
        let msg = cwtools_i18n::t(match outcome {
            ScanOutcome::Ran => cwtools_i18n::Key::CommandWorkspaceReindexed,
            ScanOutcome::Busy => cwtools_i18n::Key::CommandReindexInProgress,
            ScanOutcome::Cancelled => cwtools_i18n::Key::CommandReindexCancelled,
        })
        .to_string();
        progress.finish(Some(msg.clone())).await;
        Ok(Some(Value::String(msg)))
    }

    /// `validateWorkspace`: run a full workspace validation under a cancellable
    /// progress token and return a JSON summary. Retries the same way
    /// `reindexWorkspace` does if another scan holds the guard.
    async fn validate_workspace_command(
        &self,
        token: Option<ProgressToken>,
    ) -> Result<Option<Value>> {
        let progress = CommandProgress::begin(
            self,
            token,
            cwtools_i18n::t(cwtools_i18n::Key::CommandValidateWorkspace),
            true,
        )
        .await;
        let deadline = std::time::Instant::now()
            + std::env::var("CWTOOLS_RETRY_DEADLINE_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .map_or(
                    std::time::Duration::from_secs(60),
                    std::time::Duration::from_millis,
                );
        let mut outcome = self
            .validate_entire_workspace_tracked(false, Some(&progress))
            .await;
        while outcome == ScanOutcome::Busy && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if progress.is_cancelled() {
                outcome = ScanOutcome::Cancelled;
                break;
            }
            outcome = self
                .validate_entire_workspace_tracked(false, Some(&progress))
                .await;
        }

        let value = match outcome {
            ScanOutcome::Cancelled => serde_json::json!({ "cancelled": true }),
            ScanOutcome::Busy => serde_json::json!({ "busy": true }),
            ScanOutcome::Ran => {
                let summary = self.state.last_scan_summary.lock().clone();
                match summary {
                    Some(s) => serde_json::json!({
                        "totalFiles": s.total_files,
                        "validatedFiles": s.validated_files,
                        "filesWithErrors": s.files_with_errors,
                        "totalErrors": s.total_errors,
                        "totalWarnings": s.total_warnings,
                        "totalInfos": s.total_infos,
                        "totalHints": s.total_hints,
                    }),
                    None => serde_json::json!({
                        "message": "workspace validation did not complete",
                    }),
                }
            }
        };
        progress.finish(None).await;
        Ok(Some(value))
    }

    /// Aggregate every `## required` localisation key that no loc file provides
    /// (the same keys the CW100 check flags), grouped into one stub file per
    /// target language. Returned to the client as `[{language,
    /// filename_suggestion, content}]`; the client opens each as an untitled
    /// document for the user to review and save. Nothing is written here.
    pub(crate) fn generate_missing_loc(&self) -> Vec<Value> {
        // Snapshot the target languages first (config is read-clone-dropped, so
        // its guard is never held across the ruleset/info/loc locks below).
        let langs: Vec<cwtools_localization::Lang> = self
            .state
            .config
            .read()
            .loc_languages
            .clone()
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| vec![cwtools_localization::Lang::English]);
        // Live overlay of open `.yml` keys, so a key just typed isn't re-stubbed.
        let overlay = self.loc_overlay_keys();
        // Lock order: rules -> info_service -> loc_index.
        let rules = self.state.rules.read();
        let Some(ruleset) = rules.ruleset.as_ref() else {
            return Vec::new();
        };
        let info = self.state.info_service.read();
        let loc_guard = self.state.loc_index.read();
        // Before the loc index is built every key looks missing; bail so the
        // command never dumps the entire mod's key set as "missing".
        let Some(loc) = loc_guard.as_deref().filter(|l| !l.union().is_empty()) else {
            return Vec::new();
        };
        let exists = |key: &str| loc.exists_any(key) || overlay.contains(key);

        let mut missing: BTreeSet<String> = BTreeSet::new();
        for td in &ruleset.types {
            if td.localisation.is_empty() {
                continue;
            }
            for (_uri, inst) in info.type_index.instances(&td.name) {
                for locdef in &td.localisation {
                    // Mirrors check_missing_localisation.
                    if !locdef.is_required_name_derived() {
                        continue;
                    }
                    let expected = locdef.derived_key(&inst.name);
                    if !exists(&expected.to_ascii_lowercase()) {
                        missing.insert(expected);
                    }
                }
            }
        }
        if missing.is_empty() {
            return Vec::new();
        }
        langs
            .into_iter()
            .map(|lang| render_loc_stub(lang, &missing))
            .collect()
    }

    pub(crate) async fn determine_file_types(&self, uri: &str) -> Vec<String> {
        let ws_prefix = self.state.config.read().workspace_prefix.clone();
        let rules = self.state.rules.read();

        // Derive from the loaded ruleset when available: any TypeDefinition whose
        // path matches the logical path contributes its name to the result.
        if let Some(rs) = rules.ruleset.as_ref() {
            let logical_path = crate::paths::logical_path_from_uri(uri, &ws_prefix);
            let types: Vec<String> = rs
                .types
                .iter()
                .filter(|td| check_path_dir(&td.path_options, &logical_path))
                .map(|td| td.name.clone())
                .collect();
            if !types.is_empty() {
                return types;
            }
        }
        drop(rules);

        // Fallback when no ruleset is loaded.
        let path = uri.to_lowercase();
        let mut types = Vec::new();

        if path.contains("/events/") {
            types.push("event".to_string());
        }
        if path.contains("/common/") {
            types.push("script".to_string());
        }
        if path.contains("/common/scripted_effects") {
            types.push("scripted_effect".to_string());
        }
        if path.contains("/common/scripted_triggers") {
            types.push("scripted_trigger".to_string());
        }
        if path.ends_with(".txt") {
            types.push("txt".to_string());
        }

        types
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_localization::Lang;
    use serde_json::json;

    #[test]
    fn locale_tag_prefers_the_protocol_field_over_the_init_option() {
        let with_option = InitializeParams {
            initialization_options: Some(json!({ "locale": "fr" })),
            ..Default::default()
        };
        assert_eq!(locale_tag(&with_option), Some("fr"));

        let with_both = InitializeParams {
            locale: Some("de".to_string()),
            initialization_options: Some(json!({ "locale": "fr" })),
            ..Default::default()
        };
        assert_eq!(locale_tag(&with_both), Some("de"));

        // A client that sends neither leaves the server in English.
        assert_eq!(locale_tag(&InitializeParams::default()), None);
        let wrong_type = InitializeParams {
            initialization_options: Some(json!({ "locale": 5 })),
            ..Default::default()
        };
        assert_eq!(locale_tag(&wrong_type), None);
    }

    #[test]
    fn extract_ignored_error_codes_lowercases_and_drops_empties() {
        let opts = json!({ "ignoredErrorCodes": ["CW100", "cw246", "", 5] });
        let codes = extract_ignored_error_codes(&opts);
        assert_eq!(codes, vec!["cw100".to_string(), "cw246".to_string()]);
    }

    #[test]
    fn extract_ignore_patterns_bounds_count_per_list() {
        // #169: a hostile config must not be able to grow the per-scan matcher
        // work without bound. Only the first MAX_IGNORE_ENTRIES survive.
        let files: Vec<String> = (0..MAX_IGNORE_ENTRIES + 50)
            .map(|i| format!("file{i}.txt"))
            .collect();
        let dirs: Vec<String> = (0..MAX_IGNORE_ENTRIES + 50)
            .map(|i| format!("dir{i}"))
            .collect();
        let opts = json!({ "ignoreFilePatterns": files, "ignoreDirectories": dirs });
        let (files, dirs) = extract_ignore_patterns(&opts);
        assert_eq!(files.len(), MAX_IGNORE_ENTRIES);
        assert_eq!(dirs.len(), MAX_IGNORE_ENTRIES);
        assert_eq!(files[0], "file0.txt");
        assert_eq!(
            files[MAX_IGNORE_ENTRIES - 1],
            format!("file{}.txt", MAX_IGNORE_ENTRIES - 1)
        );
        assert_eq!(dirs[0], "dir0");
        assert_eq!(
            dirs[MAX_IGNORE_ENTRIES - 1],
            format!("dir{}", MAX_IGNORE_ENTRIES - 1)
        );
    }

    #[test]
    fn extract_ignore_patterns_drops_overlength_globs() {
        // A 1 MB '?'-heavy glob used to force ~255M DP iterations per filename
        // (#169). Over-limit entries are dropped; valid ones pass through.
        let opts = json!({
            "ignoreFilePatterns": ["*.tmp", "x".repeat(MAX_IGNORE_PATTERN_LEN + 1), "**/skip.txt"],
        });
        let (files, _) = extract_ignore_patterns(&opts);
        assert_eq!(files, vec!["*.tmp".to_string(), "**/skip.txt".to_string()]);
        // At the cap exactly: kept.
        let at_cap = "?".repeat(MAX_IGNORE_PATTERN_LEN);
        let (files, _) = extract_ignore_patterns(&json!({ "ignoreFilePatterns": [at_cap] }));
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn extract_ignored_error_codes_bounds_count() {
        let codes: Vec<String> = (0..MAX_IGNORED_ERROR_CODES + 50)
            .map(|i| format!("CW{i}"))
            .collect();
        let opts = json!({ "ignoredErrorCodes": codes });
        let codes = extract_ignored_error_codes(&opts);
        assert_eq!(codes.len(), MAX_IGNORED_ERROR_CODES);
        assert_eq!(codes[0], "cw0");
        assert_eq!(
            codes[MAX_IGNORED_ERROR_CODES - 1],
            format!("cw{}", MAX_IGNORED_ERROR_CODES - 1)
        );
    }

    #[test]
    fn extract_ignored_error_codes_absent_is_empty() {
        assert!(extract_ignored_error_codes(&json!({})).is_empty());
    }

    #[test]
    fn extract_u64_setting_reads_valid_values() {
        let opts = json!({ "backgroundReindexIdleSeconds": 30 });
        assert_eq!(
            extract_u64_setting(&opts, "backgroundReindexIdleSeconds"),
            Some(30)
        );
        assert_eq!(
            extract_u64_setting(&json!({ "k": 0 }), "k"),
            Some(0),
            "0 is a valid value (disables), not an error"
        );
    }

    #[test]
    fn extract_u64_setting_absent_is_silently_none() {
        assert_eq!(extract_u64_setting(&json!({}), "k"), None);
    }

    #[test]
    fn extract_u64_setting_invalid_types_are_none() {
        // Present-but-wrong-type (string, float, negative, null) is ignored;
        // the warn side effect isn't asserted here, just the ignoring.
        for v in [json!("30"), json!(1.5), json!(-5), json!(null), json!([30])] {
            assert_eq!(extract_u64_setting(&json!({ "k": v }), "k"), None);
        }
    }

    #[test]
    fn reload_status_message_reports_every_state_combination() {
        // The client displays this string verbatim, so the exact wording is
        // the contract and each combination must stay honest.
        let dir = std::path::Path::new("my/rules/dir");
        assert_eq!(
            reload_status_message(true, ScanOutcome::Ran, dir),
            "Rules config reloaded; workspace re-validated."
        );
        assert_eq!(
            reload_status_message(true, ScanOutcome::Busy, dir),
            "Rules config reloaded; re-validation queued behind the running scan."
        );
        assert_eq!(
            reload_status_message(false, ScanOutcome::Ran, dir),
            "No rules loaded from my/rules/dir; workspace re-validated."
        );
        assert_eq!(
            reload_status_message(false, ScanOutcome::Busy, dir),
            "No rules loaded from my/rules/dir; re-validation still pending (a scan is running)."
        );
        // Cancelled reads the same either way: the rules load already happened
        // or already failed, and neither leaves a re-validation coming.
        assert_eq!(
            reload_status_message(true, ScanOutcome::Cancelled, dir),
            "Rules config reloaded; re-validation cancelled."
        );
        assert_eq!(
            reload_status_message(false, ScanOutcome::Cancelled, dir),
            "No rules loaded from my/rules/dir; re-validation cancelled."
        );
    }

    #[test]
    fn render_loc_stub_uses_paradox_shape() {
        let mut missing = BTreeSet::new();
        missing.insert("my_focus".to_string());
        missing.insert("my_focus_desc".to_string());
        let stub = render_loc_stub(Lang::English, &missing);
        assert_eq!(stub["language"], "english");
        assert_eq!(stub["filename_suggestion"], "generated_l_english.yml");
        // Header line then one ` KEY:0 "TODO"` entry per key, keys sorted (BTreeSet).
        assert_eq!(
            stub["content"].as_str().unwrap(),
            "l_english:\n my_focus:0 \"TODO\"\n my_focus_desc:0 \"TODO\"\n"
        );
    }

    #[test]
    fn extract_localisation_languages_handles_absent_and_empty() {
        assert_eq!(extract_localisation_languages(&json!({})), None);
        // Non-array is a silent None (warns, doesn't scope).
        assert_eq!(
            extract_localisation_languages(&json!({ "localisationLanguages": "english" })),
            None
        );
        // Empty array or all-unknown -> Some(None) meaning "validate all" (no scoping).
        assert_eq!(
            extract_localisation_languages(&json!({ "localisationLanguages": [] })),
            Some(None)
        );
        assert_eq!(
            extract_localisation_languages(&json!({ "localisationLanguages": ["klingon"] })),
            Some(None)
        );
        // Mixed known/unknown keeps only known.
        assert_eq!(
            extract_localisation_languages(
                &json!({ "localisationLanguages": ["english", "klingon", "french"] })
            ),
            Some(Some(vec![Lang::English, Lang::French]))
        );
    }

    #[test]
    fn extract_bool_setting_distinguishes_absent_from_wrong_type() {
        assert_eq!(extract_bool_setting(&json!({}), "k"), None);
        assert_eq!(extract_bool_setting(&json!({ "k": true }), "k"), Some(true));
        assert_eq!(
            extract_bool_setting(&json!({ "k": false }), "k"),
            Some(false)
        );
        for v in [json!("true"), json!(1), json!(null)] {
            assert_eq!(extract_bool_setting(&json!({ "k": v }), "k"), None);
        }
    }

    #[test]
    fn extract_hover_scope_display_accepts_only_two_strings() {
        assert_eq!(extract_hover_scope_display(&json!({})), None);
        assert_eq!(
            extract_hover_scope_display(&json!({ "hoverScopeDisplay": "context" })),
            Some(false)
        );
        assert_eq!(
            extract_hover_scope_display(&json!({ "hoverScopeDisplay": "resolved" })),
            Some(true)
        );
        for v in [
            json!("Resolved"),
            json!("CONTEXT"),
            json!(true),
            json!(null),
        ] {
            assert_eq!(
                extract_hover_scope_display(&json!({ "hoverScopeDisplay": v })),
                None
            );
        }
    }

    #[test]
    fn folders_to_paths_drops_non_file_uris_and_empty() {
        assert!(folders_to_paths(&[]).is_empty());
        assert!(folders_to_paths(&["http://localhost/".to_string()]).is_empty());
        assert!(folders_to_paths(&["not a uri".to_string()]).is_empty());
        // A file URI with a drive letter parses on all platforms via `Url`.
        let file_uri = if cfg!(windows) {
            "file:///C:/repo"
        } else {
            "file:///repo"
        };
        let paths = folders_to_paths(&[file_uri.to_string()]);
        assert_eq!(paths.len(), 1);
    }

    #[test]
    fn extract_ignore_patterns_filters_non_string_and_empty() {
        let opts = json!({
            "ignoreFilePatterns": ["*.tmp", "", 42, null, "keep.txt"],
            "ignoreDirectories": ["build", ""]
        });
        let (files, dirs) = extract_ignore_patterns(&opts);
        assert_eq!(files, vec!["*.tmp".to_string(), "keep.txt".to_string()]);
        assert_eq!(dirs, vec!["build".to_string()]);
    }

    #[test]
    fn apply_formatting_settings_overlays_present_keys() {
        let base = cwtools_parser::format::FormatOptions::default();
        let out = apply_formatting_settings(
            &json!({
                "formattingIndentStyle": "tab",
                "formattingIndentSize": 2,
                "formattingTrimTrailingWhitespace": false,
                "formattingInsertFinalNewline": false,
            }),
            base,
        );
        assert_eq!(out.indent_style, cwtools_parser::format::IndentStyle::Tab);
        assert_eq!(out.indent_size, 2);
        assert!(!out.trim_trailing_whitespace);
        assert!(!out.insert_final_newline);
        assert_eq!(apply_formatting_settings(&json!({}), base), base);
    }
}
