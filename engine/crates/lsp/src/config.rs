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

/// matcher work without bound (#169).
const MAX_IGNORE_ENTRIES: usize = 200;
const MAX_IGNORE_PATTERN_LEN: usize = 1024;
const MAX_IGNORED_ERROR_CODES: usize = 200;

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

fn folders_to_paths(uris: &[String]) -> Vec<std::path::PathBuf> {
    uris.iter()
        .filter_map(|uri| crate::access::file_uri_to_path(uri))
        .collect()
}

/// The primary after a folder change (#661): a surviving primary stays put,
/// otherwise an added folder wins, else the first surviving root is promoted.
fn replacement_primary(
    current: Option<&str>,
    removed: &[String],
    added: &[String],
    surviving_roots: &[std::path::PathBuf],
) -> Option<String> {
    let primary_survives = match current {
        Some(uri) => !removed.iter().any(|r| r == uri),
        None => false,
    };
    if primary_survives {
        return current.map(str::to_string);
    }
    added.first().cloned().or_else(|| {
        surviving_roots
            .first()
            .map(|p| crate::paths::path_to_uri(p))
    })
}

fn locale_tag(params: &InitializeParams) -> Option<&str> {
    params.locale.as_deref().or_else(|| {
        params
            .initialization_options
            .as_ref()
            .and_then(|opts| opts.get("locale"))
            .and_then(|v| v.as_str())
    })
}

/// verbatim, so the wording is the contract: each half (rules loaded or not,
fn reload_status_message(loaded: bool, outcome: ScanOutcome, dir: &std::path::Path) -> String {
    let status = cwtools_i18n::t(match outcome {
        ScanOutcome::Ran => cwtools_i18n::Key::StatusRevalidated,
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

/// an `l_<lang>:` header then ` KEY:0 "TODO"` entries. The file needs a UTF-8
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
    pub(crate) fn set_ruleset(&self, ruleset: RuleSet) {
        let game = self.state.config.read().game();
        let registry = build_scope_registry_arc(&ruleset, game);
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
        self.state
            .settings_generation
            .fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) async fn initialize_impl(
        &self,
        params: InitializeParams,
    ) -> Result<InitializeResult> {
        self.client
            .log_message(
                MessageType::INFO,
                format!("★ CWTools Rust LSP server v{}", env!("CARGO_PKG_VERSION")),
            )
            .await;
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

        if let Some(opts) = &params.initialization_options {
            if let Some(lang) = opts.get("language").and_then(|v| v.as_str()) {
                self.state.config.write().language = lang.to_string();
                self.client
                    .log_message(MessageType::INFO, format!("language: {}", lang))
                    .await;
            }

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

            if let Some(all) = opts.get("hoverShowAllLanguages").and_then(|v| v.as_bool()) {
                self.state
                    .hover_show_all_languages
                    .store(all, std::sync::atomic::Ordering::Relaxed);
            }

            if let Some(dbg) = opts.get("hoverDebug").and_then(|v| v.as_bool()) {
                self.state
                    .hover_debug
                    .store(dbg, std::sync::atomic::Ordering::Relaxed);
            }

            // the ambient current scope. (#37)
            if let Some(mode) = opts.get("hoverScopeDisplay").and_then(|v| v.as_str()) {
                self.state
                    .hover_resolved_scope
                    .store(mode == "resolved", std::sync::atomic::Ordering::Relaxed);
            }

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

            if let Some(cd) = opts.get("cacheDir").and_then(|v| v.as_str()) {
                self.state.config.write().cache_dir = Some(std::path::PathBuf::from(cd));
            }

            if let Some(mins) = extract_u64_setting(opts, "backgroundReindexIntervalMinutes") {
                self.state
                    .config
                    .write()
                    .background_reindex_interval_minutes = mins;
            }

            if let Some(secs) = extract_u64_setting(opts, "backgroundReindexIdleSeconds") {
                self.state.config.write().background_reindex_idle_seconds = secs;
            }

            if let Some(wide) = extract_bool_setting(opts, "workspaceWideDiagnostics") {
                self.state.config.write().workspace_wide_diagnostics = wide;
            }
            self.client
                .log_message(MessageType::INFO, format!("init options: {:?}", opts))
                .await;

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

        // Canonicalised like the document URIs (#319): `workspace_prefix` is
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
            cfg.workspace_roots = folders_to_paths(&folders);
            cfg.refresh_roots();
        }

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
        // lines for free. Clients that don't advertise utf-32 (VS Code) stay on
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
                        "getGraphData".to_string(),
                    ],
                    work_done_progress_options: WorkDoneProgressOptions {
                        work_done_progress: Some(true),
                    },
                }),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                document_highlight_provider: Some(OneOf::Left(true)),
                selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
                document_link_provider: Some(DocumentLinkOptions {
                    resolve_provider: Some(false),
                    work_done_progress_options: Default::default(),
                }),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        code_action_kinds: Some(vec![
                            CodeActionKind::QUICKFIX,
                            CodeActionKind::SOURCE_FIX_ALL,
                        ]),
                        resolve_provider: Some(false),
                        work_done_progress_options: Default::default(),
                    },
                )),
                inlay_hint_provider: Some(OneOf::Left(true)),
                // reindex (#184). A `workspace/semanticTokens/refresh` is sent
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
                color_provider: Some(ColorProviderCapability::Simple(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                document_range_formatting_provider: Some(OneOf::Left(true)),
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
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "cwtools-server".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    pub(crate) async fn load_rules_config(&self, cache_path: &std::path::Path) -> bool {
        // path that doesn't resolve here (e.g. a Windows `rules_folder`
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

        // handshake gate below (#98). Snapshotting once is sound only while the
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
            diags_by_file
                .entry(crate::paths::path_to_uri(&err.file))
                .or_default()
                .push(crate::validate::rule_parse_error_to_diagnostic(
                    err,
                    &crate::lines::DocLines::none(),
                ));
        }
        for diags in diags_by_file.values_mut() {
            crate::validate::truncate_diagnostics(diags, &crate::lines::DocLines::none());
        }
        let mut to_publish: Vec<(String, Vec<Diagnostic>)> = diags_by_file.into_iter().collect();
        {
            let current: std::collections::HashSet<String> =
                to_publish.iter().map(|(uri, _)| uri.clone()).collect();
            let mut previous = self.state.published_rule_uris.lock();
            let open = self.state.documents.lock();
            to_publish.extend(
                previous
                    .difference(&current)
                    .filter(|uri| !open.contains_key(*uri))
                    .map(|uri| (uri.clone(), Vec::new())),
            );
            *previous = current;
        }

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
            let summary = format!(
                "CWTools: {} rules-config error(s), first: {first}",
                parse_errors.len()
            );
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
            self.rebuild_modifier_keys();
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

    pub(crate) async fn did_change_workspace_folders_impl(
        &self,
        params: DidChangeWorkspaceFoldersParams,
    ) {
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
        let removed_paths = folders_to_paths(&removed);
        let added_paths = folders_to_paths(&added);

        // Derive roots and primary under one write lock so concurrent
        // unordered notifications each apply as one transaction (#661).
        let next = {
            let mut cfg = self.state.config.write();
            let current = cfg.workspace_uri.as_deref().map(str::to_string);
            let surviving_roots = {
                let mut roots: Vec<std::path::PathBuf> = cfg
                    .workspace_roots
                    .iter()
                    .filter(|r| !removed_paths.contains(r))
                    .cloned()
                    .collect();
                for path in &added_paths {
                    if !roots.contains(path) {
                        roots.push(path.clone());
                    }
                }
                roots
            };
            let next = replacement_primary(current.as_deref(), &removed, &added, &surviving_roots);
            if next != current {
                match &next {
                    Some(uri) => {
                        cfg.workspace_prefix = Some(crate::paths::workspace_prefix_of(uri));
                        cfg.workspace_uri = Some(uri.as_str().into());
                    }
                    None => {
                        cfg.workspace_prefix = None;
                        cfg.workspace_uri = None;
                    }
                }
            }
            cfg.workspace_roots = surviving_roots;
            cfg.refresh_roots();
            self.state
                .workspace_roots_generation
                .fetch_add(1, Ordering::Release);
            next
        };
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
        self.state
            .settings_generation
            .fetch_add(1, Ordering::SeqCst);
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

    pub(crate) async fn did_change_configuration_impl(&self, params: DidChangeConfigurationParams) {
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
            "cacheVanilla" => self.cache_vanilla_command(token).await,
            "clearAllCaches" => self.clear_all_caches_command(token).await,
            "reloadrulesconfig" => self.reload_rules_config_command(token).await,
            "genlocall" => {
                let progress =
                    CommandProgress::begin(self, token, "CWTools: Generate missing loc", false)
                        .await;
                let stubs = self.generate_missing_loc();
                progress.finish(None).await;
                Ok(Some(Value::Array(stubs)))
            }
            "fixAllWorkspace" => Ok(Some(Value::String(self.fix_all_workspace_impl().await))),
            "formatWorkspace" => Ok(Some(Value::String(self.format_workspace_impl(token).await))),
            "reindexWorkspace" => self.reindex_workspace_command(token).await,
            "validateWorkspace" => self.validate_workspace_command(token).await,
            "getGraphData" => self.get_graph_data(&params.arguments).await,
            other => Err(tower_lsp::jsonrpc::Error::invalid_params(format!(
                "unknown command: {other}"
            ))),
        }
    }

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

    fn reset_scan_publication_state(&self) {
        *self.state.last_scan_summary.lock() = None;
        self.state.published_workspace_uris.lock().clear();
    }

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
        // the client cancelling the command mid-index (#204).
        let guard = crate::scan::ScanGuard::for_command(self);
        self.ensure_vanilla_index(Some(&progress), true, false)
            .await;
        tokio::task::block_in_place(|| self.merge_pending_vanilla_index());
        self.rebuild_modifier_keys();
        guard.finish().await;
        let msg = "Vanilla cache rebuilt.".to_string();
        progress.finish(Some(msg.clone())).await;
        Ok(Some(Value::String(msg)))
    }

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
        self.clear_vanilla_state();
        self.reset_scan_publication_state();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
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
        let status = cwtools_i18n::t(match outcome {
            ScanOutcome::Ran => cwtools_i18n::Key::StatusReindexed,
            ScanOutcome::Busy => cwtools_i18n::Key::StatusReindexPending,
            ScanOutcome::Cancelled => {
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
        if outcome == ScanOutcome::Busy && loaded {
            self.spawn_deferred_revalidation("reloadrulesconfig");
        }
        let msg = reload_status_message(loaded, outcome, &dir);
        progress.finish(Some(msg.clone())).await;
        Ok(Some(Value::String(msg)))
    }

    async fn reindex_workspace_command(
        &self,
        token: Option<ProgressToken>,
    ) -> Result<Option<Value>> {
        let progress =
            CommandProgress::begin(self, token, "CWTools: Re-index workspace", true).await;
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

    pub(crate) fn generate_missing_loc(&self) -> Vec<Value> {
        let langs: Vec<cwtools_localization::Lang> = self
            .state
            .config
            .read()
            .loc_languages
            .clone()
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| vec![cwtools_localization::Lang::English]);
        let overlay = self.loc_overlay_keys();
        // Lock order: rules -> info_service -> loc_index.
        let rules = self.state.rules.read();
        let Some(ruleset) = rules.ruleset.as_ref() else {
            return Vec::new();
        };
        let info = self.state.info_service.read();
        let loc_guard = self.state.loc_index.read();
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
        // (#169). Over-limit entries are dropped; valid ones pass through.
        let opts = json!({
            "ignoreFilePatterns": ["*.tmp", "x".repeat(MAX_IGNORE_PATTERN_LEN + 1), "**/skip.txt"],
        });
        let (files, _) = extract_ignore_patterns(&opts);
        assert_eq!(files, vec!["*.tmp".to_string(), "**/skip.txt".to_string()]);
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
        for v in [json!("30"), json!(1.5), json!(-5), json!(null), json!([30])] {
            assert_eq!(extract_u64_setting(&json!({ "k": v }), "k"), None);
        }
    }

    #[test]
    fn reload_status_message_reports_every_state_combination() {
        // The client displays this string verbatim, so the exact wording is
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
        assert_eq!(
            stub["content"].as_str().unwrap(),
            "l_english:\n my_focus:0 \"TODO\"\n my_focus_desc:0 \"TODO\"\n"
        );
    }

    #[test]
    fn extract_localisation_languages_handles_absent_and_empty() {
        assert_eq!(extract_localisation_languages(&json!({})), None);
        assert_eq!(
            extract_localisation_languages(&json!({ "localisationLanguages": "english" })),
            None
        );
        assert_eq!(
            extract_localisation_languages(&json!({ "localisationLanguages": [] })),
            Some(None)
        );
        assert_eq!(
            extract_localisation_languages(&json!({ "localisationLanguages": ["klingon"] })),
            Some(None)
        );
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
    fn replacement_primary_promotes_a_surviving_root_when_the_primary_is_removed() {
        // Fixtures use the same URI→path→URI round trip as production.
        let uri = |name: &str| {
            if cfg!(windows) {
                format!("file:///C:/{name}")
            } else {
                format!("file:///{name}")
            }
        };
        let (a, b, c) = (uri("ws-a"), uri("ws-b"), uri("ws-c"));
        let roots = folders_to_paths(&[a.clone(), b.clone()]);
        assert_eq!(roots.len(), 2);
        let only_b = roots[1..].to_vec();
        let only_a = roots[..1].to_vec();

        // Removal-only: the surviving root becomes the primary (#661).
        assert_eq!(
            replacement_primary(Some(&a), std::slice::from_ref(&a), &[], &only_b),
            Some(b.clone())
        );
        // An added folder in the same event still wins over the survivors.
        assert_eq!(
            replacement_primary(
                Some(&a),
                std::slice::from_ref(&a),
                std::slice::from_ref(&c),
                &only_b
            ),
            Some(c.clone())
        );
        // Removing a folder that is not the primary leaves the primary alone.
        assert_eq!(
            replacement_primary(Some(&a), std::slice::from_ref(&b), &[], &only_a),
            Some(a.clone())
        );
        // Removing every root leaves no primary, so the rescan is skipped.
        assert_eq!(
            replacement_primary(Some(&a), std::slice::from_ref(&a), &[], &[]),
            None
        );
        // No primary to begin with, nothing arrives, nothing survives.
        assert_eq!(replacement_primary(None, &[], &[], &[]), None);
        // No primary to begin with, a folder arrives: it becomes the primary.
        assert_eq!(
            replacement_primary(None, &[], std::slice::from_ref(&c), &[]),
            Some(c)
        );
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
