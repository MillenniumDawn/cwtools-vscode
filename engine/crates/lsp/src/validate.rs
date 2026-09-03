use std::collections::HashSet;
use std::sync::Arc;

use tower_lsp::lsp_types::*;

use cwtools_parser::ast::{ParseError, ParsedFile};
use cwtools_parser::parser::{parse_string, parse_string_without_comments};
use cwtools_rules::rules_types::RuleSet;
use cwtools_string_table::string_table::StringId;
use cwtools_validation::references::{UsedInstances, check_unused_instances, needs_use_tracking};
use cwtools_validation::{
    InlineScripts, Prepared, ValidationError, validate_prepared, validate_prepared_tracking_uses,
};

use crate::lines::DocLines;
use crate::paths::{logical_path_from_uri, uri_to_path_str};
use crate::state::LocDocumentCache;
use crate::{Backend, LocTextMap};

#[cfg(test)]
mod loc_sweep_test_hook {
    use super::Backend;
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::{Arc, OnceLock};

    type Hook = Box<dyn FnOnce(&Backend, &str) + Send>;
    static HOOKS: OnceLock<Mutex<HashMap<usize, Hook>>> = OnceLock::new();

    fn key(backend: &Backend) -> usize {
        Arc::as_ptr(&backend.state) as usize
    }

    pub(super) fn set(backend: &Backend, hook: Hook) {
        HOOKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .insert(key(backend), hook);
    }

    pub(super) fn run(backend: &Backend, uri: &str) {
        let hook = HOOKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .remove(&key(backend));
        if let Some(hook) = hook {
            hook(backend, uri);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn make_prepared<'a>(
    ruleset: &'a cwtools_rules::rules_types::RuleSet,
    table: &'a cwtools_string_table::string_table::StringTable,
    game: Option<cwtools_game::constants::Game>,
    type_index: &'a cwtools_info::TypeIndex,
    modifier_keys: &'a std::collections::HashSet<String>,
    loc_index: Option<&'a cwtools_localization::LocIndex>,
    extra_loc_keys: Option<&'a std::collections::HashSet<String>>,
    inline_scripts: Option<&'a cwtools_validation::InlineScripts>,
    registry: Option<&'a std::sync::Arc<cwtools_game::scope_registry::ScopeRegistry>>,
    scope_checks: bool,
    var_checks: bool,
) -> Prepared<'a> {
    Prepared {
        ruleset,
        table,
        game,
        type_index: Some(type_index),
        modifier_keys: Some(modifier_keys),
        loc_index,
        extra_loc_keys,
        inline_scripts,
        registry,
        scope_checks,
        var_checks,
    }
}

pub(crate) const MAX_FILE_ERRORS: usize = 100;

pub(crate) fn loc_diag_to_validation_error(
    d: &cwtools_localization::LocDiagnostic,
) -> ValidationError {
    ValidationError {
        message: d.message.clone(),
        severity: d.severity,
        line: d.line as u32,
        col: d.col.saturating_sub(1) as u16,
        file: d.file.as_str().into(),
        code: Some(d.code),
        fix: d.fix.clone(),
        end: None,
        related: Vec::new(),
    }
}

fn try_parse_loc_buffer(
    text: &str,
    path: &str,
) -> Result<Vec<cwtools_localization::LocFile>, cwtools_localization::LocFileParseError> {
    cwtools_localization::parse_loc_files(path, text, None)
}

fn parse_loc_buffer(text: &str, path: &str) -> Vec<cwtools_localization::LocFile> {
    try_parse_loc_buffer(text, path).unwrap_or_default()
}

/// parse used to keep the live overlay current on edit (#36).
fn loc_keys_of(text: &str, path: &str) -> HashSet<String> {
    loc_keys_from(&parse_loc_buffer(text, path))
}

fn loc_keys_from(files: &[cwtools_localization::LocFile]) -> HashSet<String> {
    let mut keys = HashSet::new();
    for file in files {
        for entry in &file.entries {
            keys.insert(entry.key.to_lowercase());
        }
    }
    keys
}

fn loc_cache(
    version: i32,
    source_bytes: usize,
    files: Vec<cwtools_localization::LocFile>,
) -> Arc<LocDocumentCache> {
    let references: HashSet<String> = files
        .iter()
        .flat_map(|file| &file.entries)
        .flat_map(|entry| &entry.refs)
        .map(|reference| reference.to_lowercase())
        .collect();
    let reference_bytes = references.iter().fold(
        references
            .capacity()
            .saturating_mul(std::mem::size_of::<String>()),
        |bytes, reference| bytes.saturating_add(reference.capacity()),
    );
    Arc::new(LocDocumentCache {
        version,
        retained_bytes: source_bytes.saturating_add(reference_bytes),
        files,
        references,
    })
}

fn loc_cache_needs_revalidation(
    cache: Option<&LocDocumentCache>,
    version: i32,
    changed_keys: &HashSet<String>,
) -> bool {
    cache
        .is_none_or(|cache| cache.version != version || !cache.references.is_disjoint(changed_keys))
}

struct OpenLocTarget {
    uri: String,
    version: i32,
    text: Arc<str>,
    cache: Option<Arc<LocDocumentCache>>,
}

fn loc_change_candidate_names(
    ruleset: Option<&RuleSet>,
    changed_keys: &HashSet<String>,
) -> HashSet<String> {
    let mut names = changed_keys.clone();
    let Some(rs) = ruleset else {
        return names;
    };
    let mut affixes: HashSet<(String, String)> = HashSet::new();
    for td in &rs.types {
        let subtype_locs = td.subtypes.iter().flat_map(|st| st.localisation.iter());
        for loc in td.localisation.iter().chain(subtype_locs) {
            if !loc.is_required_name_derived() {
                continue;
            }
            if loc.prefix.is_empty() && loc.suffix.is_empty() {
                continue;
            }
            affixes.insert((loc.prefix.to_lowercase(), loc.suffix.to_lowercase()));
        }
    }
    for (prefix, suffix) in &affixes {
        for key in changed_keys {
            if let Some(mid) = key
                .strip_prefix(prefix.as_str())
                .and_then(|m| m.strip_suffix(suffix.as_str()))
                && !mid.is_empty()
            {
                names.insert(mid.to_string());
            }
        }
    }
    names
}

pub(crate) fn truncate_validation_errors(
    errs: &mut Vec<cwtools_validation::ValidationError>,
    uri: &str,
) -> usize {
    let total = errs.len();
    if total <= MAX_FILE_ERRORS {
        return total;
    }
    let limit = errs
        .iter()
        .position(|e| e.code == Some(cwtools_error_codes::CW277_ALIAS_BRANCH_LIMIT.id))
        .map(|i| errs.remove(i));
    if errs.len() > MAX_FILE_ERRORS {
        let dropped = errs.len() - MAX_FILE_ERRORS;
        errs.truncate(MAX_FILE_ERRORS);
        errs.push(cwtools_validation::ValidationError {
            message: format!("... {dropped} additional errors truncated"),
            severity: cwtools_validation::ErrorSeverity::Information,
            line: 0,
            col: 0,
            file: uri.into(),
            code: None,
            fix: None,
            end: None,
            related: Vec::new(),
        });
    }
    errs.extend(limit);
    total
}

pub(crate) fn loc_extra_valid_refs(
    modifier_keys: &HashSet<String>,
    type_index: &cwtools_info::TypeIndex,
) -> HashSet<String> {
    let mut extra = modifier_keys.clone();
    extra.extend(type_index.loc_bindable_names());
    extra
}

pub(crate) fn append_missing_loc_errors(
    uri: &str,
    prepared: &Prepared,
    errs: &mut Vec<cwtools_validation::ValidationError>,
) {
    if let (Some(loc), Some(type_index)) = (prepared.loc_index, prepared.type_index)
        && !loc.union().is_empty()
    {
        let overlay = prepared.extra_loc_keys;
        let instances = type_index.instances_in_file(uri);
        errs.extend(cwtools_validation::missing_loc::check_missing_localisation(
            &instances,
            uri,
            &uri.into(),
            prepared.ruleset,
            |k| loc.exists_any(k) || overlay.is_some_and(|o| o.contains(k)),
        ));
    }
}

pub(crate) fn validate_parsed_with_indexes(
    uri: &str,
    parsed: &ParsedFile,
    prepared: &Prepared,
    lines: &DocLines,
    track_uses: bool,
) -> (Vec<Diagnostic>, Option<UsedInstances>) {
    let mut diagnostics = parse_errors_to_diagnostics(&parsed.errors, lines);
    let (mut errs, used) = if track_uses {
        let (errs, used) = validate_prepared_tracking_uses(parsed, uri, prepared);
        (errs, Some(used))
    } else {
        (validate_prepared(parsed, uri, prepared), None)
    };
    append_missing_loc_errors(uri, prepared, &mut errs);
    truncate_validation_errors(&mut errs, uri);
    for err in &errs {
        diagnostics.push(validation_error_to_diagnostic(err, lines));
    }
    (diagnostics, used)
}

const CODE_TAGS: &[(&str, DiagnosticTag)] = &[
    ("CW121", DiagnosticTag::UNNECESSARY),
    ("CW231", DiagnosticTag::UNNECESSARY),
    ("CW236", DiagnosticTag::DEPRECATED),
    ("CW239", DiagnosticTag::UNNECESSARY),
];

fn code_tags(code: &NumberOrString) -> Option<Vec<DiagnosticTag>> {
    let NumberOrString::String(id) = code else {
        return None;
    };
    CODE_TAGS
        .iter()
        .find(|(c, _)| c.eq_ignore_ascii_case(id))
        .map(|(_, tag)| vec![tag.clone()])
}

fn code_description(code: &NumberOrString) -> Option<CodeDescription> {
    let NumberOrString::String(id) = code else {
        return None;
    };
    let href = Url::parse(&cwtools_error_codes::doc_url(id)).ok()?;
    Some(CodeDescription { href })
}

fn diagnostic_at(
    line: u32,
    col: u32,
    lines: &DocLines,
    severity: DiagnosticSeverity,
    source: &str,
    code: Option<NumberOrString>,
    message: String,
) -> Diagnostic {
    let start = lines.position(line, col);
    let code_description = code.as_ref().and_then(code_description);
    let tags = code.as_ref().and_then(code_tags);
    Diagnostic {
        range: Range {
            end: lines.end_position(line, start.character),
            start,
        },
        severity: Some(severity),
        code,
        code_description,
        source: Some(source.to_string()),
        message,
        related_information: None,
        tags,
        data: None,
    }
}

fn severity_to_lsp(severity: cwtools_validation::ErrorSeverity) -> DiagnosticSeverity {
    match severity {
        cwtools_validation::ErrorSeverity::Error => DiagnosticSeverity::ERROR,
        cwtools_validation::ErrorSeverity::Warning => DiagnosticSeverity::WARNING,
        cwtools_validation::ErrorSeverity::Information => DiagnosticSeverity::INFORMATION,
        cwtools_validation::ErrorSeverity::Hint => DiagnosticSeverity::HINT,
    }
}

pub(crate) fn parse_error_to_diagnostic(e: &ParseError, lines: &DocLines) -> Diagnostic {
    let ParseError::Pos(line, col, msg) = e;
    diagnostic_at(
        line.saturating_sub(1),
        *col as u32,
        lines,
        DiagnosticSeverity::ERROR,
        "cwtools",
        None,
        msg.clone(),
    )
}

pub(crate) fn parse_errors_to_diagnostics(
    errors: &[ParseError],
    lines: &DocLines,
) -> Vec<Diagnostic> {
    let total = errors.len();
    let mut diagnostics: Vec<Diagnostic> = errors
        .iter()
        .take(MAX_FILE_ERRORS)
        .map(|e| parse_error_to_diagnostic(e, lines))
        .collect();
    if total > MAX_FILE_ERRORS {
        let dropped = total - MAX_FILE_ERRORS;
        diagnostics.push(diagnostic_at(
            0,
            0,
            lines,
            DiagnosticSeverity::INFORMATION,
            "cwtools",
            None,
            format!("... {dropped} additional errors truncated"),
        ));
    }
    diagnostics
}

pub(crate) fn rule_parse_error_to_diagnostic(
    err: &cwtools_rules::ruleset_loader::RuleParseError,
    lines: &DocLines,
) -> Diagnostic {
    let line = err.line.saturating_sub(1);
    let col = err.col as u32;
    diagnostic_at(
        line,
        col,
        lines,
        severity_to_lsp(err.severity),
        "cwtools-rules",
        Some(NumberOrString::String(err.code.to_string())),
        err.message.clone(),
    )
}

pub(crate) fn code_is_suppressed(code: Option<&NumberOrString>, ignored: &[String]) -> bool {
    match code {
        Some(NumberOrString::String(c)) => ignored.contains(&c.to_ascii_lowercase()),
        _ => false,
    }
}

pub(crate) fn drop_inline_suppressed(
    diagnostics: &mut Vec<Diagnostic>,
    map: &cwtools_validation::inline_ignore::InlineIgnoreMap,
) {
    if map.is_empty() {
        return;
    }
    diagnostics.retain(|d| {
        let Some(NumberOrString::String(code)) = d.code.as_ref() else {
            return true;
        };
        !cwtools_validation::inline_ignore::inline_suppressed(
            map,
            d.range.start.line.saturating_add(1),
            code,
        )
    });
}

pub(crate) fn validation_error_to_diagnostic(
    err: &ValidationError,
    lines: &DocLines,
) -> Diagnostic {
    let line = err.line.saturating_sub(1);
    let col = err.col as u32;
    let severity = severity_to_lsp(err.severity);
    let mut diag = diagnostic_at(
        line,
        col,
        lines,
        severity,
        "cwtools",
        err.code.map(|c| NumberOrString::String(c.to_string())),
        err.message.clone(),
    );
    if let Some((end_line, end_col)) = err.end
        && lines.has_text()
    {
        diag.range.end = lines.clamped_end_position(
            end_line.saturating_sub(1),
            end_col as u32,
            diag.range.start,
        );
    }
    if !err.related.is_empty()
        && let Ok(uri) = Url::parse(&err.file)
    {
        diag.related_information = Some(
            err.related
                .iter()
                .map(|r| DiagnosticRelatedInformation {
                    location: Location {
                        uri: uri.clone(),
                        range: lines.related_range(r.line, r.col, r.end),
                    },
                    message: r.message.clone(),
                })
                .collect(),
        );
    }
    if let Some(fix) = &err.fix {
        diag.data = Some(crate::code_action::fix_to_data(fix));
    }
    diag
}

pub(crate) fn collect_doc_tokens(ast: &ParsedFile) -> HashSet<StringId> {
    use cwtools_parser::ast::Value;
    let arena = &ast.arena;
    let mut tokens = HashSet::new();
    for leaf in &arena.leaves {
        tokens.insert(leaf.key.lower);
        if let Value::String(t) | Value::QString(t) = &leaf.value {
            tokens.insert(t.lower);
        }
    }
    for lv in &arena.leaf_values {
        if let Value::String(t) | Value::QString(t) = &lv.value {
            tokens.insert(t.lower);
        }
    }
    tokens.remove(&StringId(0));
    tokens
}

impl Backend {
    pub(crate) fn update_doc_tokens(&self, uri: &str, ast: Option<&Arc<ParsedFile>>) {
        match ast {
            Some(ast) => {
                let toks = collect_doc_tokens(ast);
                self.state.doc_tokens.write().insert(uri.to_string(), toks);
            }
            None => {
                self.state.doc_tokens.write().remove(uri);
            }
        }
    }

    pub(crate) fn is_ignored_uri(&self, uri: &str) -> bool {
        let cfg = self.state.config.read();
        let logical = logical_path_from_uri(uri, &cfg.workspace_prefix);
        cwtools_file_manager::file_manager::is_ignored_path(
            &logical,
            &cfg.ignore_file_patterns,
            &cfg.ignore_dir_patterns,
        )
    }

    pub(crate) fn clear_ignored_file_state(&self, uri: &str) {
        let rel = self.workspace_rel_for_file_index(uri);
        let bump_info = {
            let mut info = self.state.info_service.write();
            let before = info.export_fingerprint(uri);
            info.clear_file(uri);
            if let Some(rel) = rel
                && !info.type_index.file_index.is_empty()
            {
                Arc::make_mut(&mut info.type_index).file_index.remove(&rel);
            }
            info.export_fingerprint(uri) != before
        };
        self.state.doc_tokens.write().remove(uri);
        let dropped: std::collections::HashSet<String> = {
            let mut store = self.state.type_uses.write();
            store
                .remove(uri)
                .map(|uses| {
                    uses.changed_names(&Default::default())
                        .into_iter()
                        .collect()
                })
                .unwrap_or_default()
        };
        if !dropped.is_empty() {
            self.state
                .type_uses_revision
                .fetch_add(1, std::sync::atomic::Ordering::Release);
            self.state.pending_changed_names.lock().extend(dropped);
        }
        if self.state.loc_live_overlay.read().contains_key(uri) {
            self.loc_live_overlay_mut().remove(uri);
        }
        if self.state.loc_watched_overlay.read().contains_key(uri) {
            self.loc_watched_overlay_mut().remove(uri);
        }
        self.state.watched_signatures.lock().remove(uri);
        if bump_info {
            self.bump_info_revision();
        }
    }

    #[tracing::instrument(skip_all, fields(uri = %uri))]
    pub(crate) fn index_parsed_file(
        &self,
        uri: &str,
        parsed: &ParsedFile,
        parsed_version: Option<i32>,
    ) {
        if self.is_ignored_uri(uri) {
            self.clear_ignored_file_state(uri);
            return;
        }
        let ws_prefix = self.state.config.read().workspace_prefix.clone();
        let logical_path = logical_path_from_uri(uri, &ws_prefix);
        let ruleset = self.state.rules.read().ruleset.clone();
        let collected = ruleset.as_ref().map(|ruleset| {
            cwtools_info::collect_type_instances_with_subtypes(
                ruleset,
                parsed,
                &logical_path,
                &self.state.string_table,
                cwtools_validation::subtype_membership_for_instance,
            )
        });
        if let Some(version) = parsed_version
            && self
                .state
                .documents
                .lock()
                .get(uri)
                .is_some_and(|doc| doc.version != version)
        {
            return;
        }
        let mut info = self.state.info_service.write();
        let exports_before = info.export_fingerprint(uri);
        info.clear_file(uri);
        if let (Some(ruleset), Some(collected)) = (ruleset.as_ref(), collected) {
            info.index_file_with_precomputed_instances(
                uri,
                parsed,
                &self.state.string_table,
                ruleset,
                &logical_path,
                collected.instances,
                collected.subtype_instances,
            );
        }
        let exports_changed = info.export_fingerprint(uri) != exports_before;
        drop(info);
        if exports_changed {
            self.bump_info_revision();
        }
    }

    /// workspace scan (#259). Cheap to call unconditionally: the path check
    pub(crate) fn refresh_inline_script(&self, uri: &str, text: &str) -> Option<String> {
        if self.is_ignored_uri(uri) {
            return None;
        }
        let ws_prefix = self.state.config.read().workspace_prefix.clone();
        let logical_path = logical_path_from_uri(uri, &ws_prefix);
        if !InlineScripts::is_script_path(&logical_path) {
            return None;
        }
        let parsed = parse_string_without_comments(text, &self.state.string_table);
        self.state
            .inline_scripts
            .write()
            .insert(&logical_path, parsed)
    }

    pub(crate) fn remove_inline_script(&self, uri: &str) -> Option<String> {
        let ws_prefix = self.state.config.read().workspace_prefix.clone();
        let logical_path = logical_path_from_uri(uri, &ws_prefix);
        self.state.inline_scripts.write().remove(&logical_path)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn validate_parsed_prebuilt(
        &self,
        uri: &str,
        parsed: &ParsedFile,
        modifier_keys: &std::collections::HashSet<String>,
        ruleset: &RuleSet,
        game: Option<cwtools_game::constants::Game>,
        registry: Option<&std::sync::Arc<cwtools_game::scope_registry::ScopeRegistry>>,
        lines: &DocLines,
    ) -> Vec<Diagnostic> {
        if self.is_ignored_uri(uri) {
            return Vec::new();
        }
        let overlay = self.loc_overlay_keys();
        let info_guard = self.state.info_service.read();
        let loc_guard = self.state.loc_index.read();
        let inline_guard = self.state.inline_scripts.read();
        let (scope_checks, var_checks) = {
            let cfg = self.state.config.read();
            (cfg.scope_checks, cfg.var_checks)
        };
        let prepared = make_prepared(
            ruleset,
            &self.state.string_table,
            game,
            &info_guard.type_index,
            modifier_keys,
            loc_guard.as_deref(),
            Some(&overlay),
            Some(&inline_guard),
            registry,
            scope_checks,
            var_checks,
        );
        let track = needs_use_tracking(ruleset, game);
        let (mut diagnostics, used) =
            validate_parsed_with_indexes(uri, parsed, &prepared, lines, track);
        if let Some(used) = used {
            for err in
                &self.unused_instance_errors(uri, used, ruleset, game, &info_guard.type_index)
            {
                diagnostics.push(validation_error_to_diagnostic(err, lines));
            }
        }
        diagnostics
    }

    fn refresh_type_uses(&self, uri: &str, uses: UsedInstances) -> HashSet<String> {
        let mut store = self.state.type_uses.write();
        let changed: HashSet<String> = match store.get(uri) {
            Some(prev) => prev.changed_names(&uses).into_iter().collect(),
            None => uses
                .changed_names(&UsedInstances::default())
                .into_iter()
                .collect(),
        };
        if !changed.is_empty() || !store.contains_key(uri) {
            store.insert(uri.to_string(), uses);
        }
        drop(store);
        if !changed.is_empty() {
            self.state
                .type_uses_revision
                .fetch_add(1, std::sync::atomic::Ordering::Release);
        }
        changed
    }

    /// keep describing text that never reached disk (#133).
    pub(crate) fn refresh_type_uses_from_parsed(&self, uri: &str, parsed: &ParsedFile) {
        let game = self.state.config.read().game();
        let rules_guard = self.state.rules.read();
        let Some(ruleset) = rules_guard.ruleset.as_ref() else {
            return;
        };
        if !needs_use_tracking(ruleset, game) {
            return;
        }
        let info_guard = self.state.info_service.read();
        let loc_guard = self.state.loc_index.read();
        let inline_guard = self.state.inline_scripts.read();
        let (scope_checks, var_checks) = {
            let cfg = self.state.config.read();
            (cfg.scope_checks, cfg.var_checks)
        };
        let prepared = make_prepared(
            ruleset,
            &self.state.string_table,
            game,
            &info_guard.type_index,
            &rules_guard.modifier_keys,
            loc_guard.as_deref(),
            None,
            Some(&inline_guard),
            rules_guard.scope_registry.as_ref(),
            scope_checks,
            var_checks,
        );
        let (_, used) = validate_prepared_tracking_uses(parsed, uri, &prepared);
        drop(inline_guard);
        drop(loc_guard);
        drop(info_guard);
        drop(rules_guard);
        let changed = self.refresh_type_uses(uri, used);
        if !changed.is_empty() {
            self.state.pending_changed_names.lock().extend(changed);
        }
    }

    pub(crate) fn merged_type_uses(&self) -> Arc<UsedInstances> {
        let revision = self
            .state
            .type_uses_revision
            .load(std::sync::atomic::Ordering::Acquire);
        let cached = {
            let guard = self.state.type_uses_merged.lock();
            guard
                .as_ref()
                .filter(|(cached_revision, _)| *cached_revision == revision)
                .map(|(_, merged)| Arc::clone(merged))
        };
        if let Some(merged) = cached {
            return merged;
        }
        let mut merged = UsedInstances::default();
        for uses in self.state.type_uses.read().values() {
            merged.merge_from(uses);
        }
        let merged = Arc::new(merged);
        *self.state.type_uses_merged.lock() = Some((revision, Arc::clone(&merged)));
        merged
    }

    pub(crate) fn unused_instance_errors(
        &self,
        uri: &str,
        uses: UsedInstances,
        ruleset: &RuleSet,
        game: Option<cwtools_game::constants::Game>,
        type_index: &cwtools_info::TypeIndex,
    ) -> Vec<ValidationError> {
        let changed = self.refresh_type_uses(uri, uses);
        if !changed.is_empty() {
            self.state.pending_changed_names.lock().extend(changed);
        }
        if !self
            .state
            .index_ready
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Vec::new();
        }
        let merged = self.merged_type_uses();
        let instances = type_index.instances_in_file(uri);
        check_unused_instances(ruleset, game, &instances, &merged, &uri.into())
    }

    pub(crate) async fn publish_filtered(
        &self,
        uri: tower_lsp::lsp_types::Url,
        mut diagnostics: Vec<Diagnostic>,
        version: Option<i32>,
        source_hash: Option<u64>,
    ) {
        {
            let cfg = self.state.config.read();
            if !cfg.ignored_error_codes.is_empty() {
                diagnostics
                    .retain(|d| !code_is_suppressed(d.code.as_ref(), &cfg.ignored_error_codes));
            }
        }
        let entries: Vec<(String, cwtools_parser::fix::SpanEdit)> = diagnostics
            .iter()
            .flat_map(crate::code_action::fixable_span_edits)
            .collect();
        let content_hash = if entries.is_empty() || version.is_some() {
            None
        } else {
            source_hash
        };
        {
            let mut store = self.state.fixable_edits.lock();
            if entries.is_empty() || (version.is_none() && content_hash.is_none()) {
                store.remove(uri.as_str());
            } else {
                store.insert(
                    uri.as_str().to_string(),
                    crate::FixableEdits {
                        entries,
                        version,
                        content_hash,
                    },
                );
            }
        }
        self.client
            .publish_diagnostics(uri, diagnostics, version)
            .await;
    }

    pub(crate) async fn publish_gated(
        &self,
        uri: tower_lsp::lsp_types::Url,
        diagnostics: Vec<Diagnostic>,
        version: Option<i32>,
        source_hash: Option<u64>,
    ) {
        let ready = self
            .state
            .index_ready
            .load(std::sync::atomic::Ordering::Relaxed);
        let diags = if ready { diagnostics } else { Vec::new() };
        self.publish_filtered(uri, diags, version, source_hash)
            .await;
    }

    #[tracing::instrument(skip_all, fields(uri = %uri, version = expected_version))]
    pub(crate) async fn debounced_validate(
        &self,
        uri: String,
        expected_version: i32,
        generation: u64,
        trigger: crate::ValidateTrigger,
    ) {
        let Ok(_permit) = self.state.validation_permits.acquire().await else {
            return;
        };
        let text = {
            let docs = self.state.documents.lock();
            match docs.get(&uri) {
                Some(d) if d.version == expected_version => d.text.clone(),
                _ => return,
            }
        };

        let (exports_before, names_before) = {
            let info = self.state.info_service.read();
            (info.export_fingerprint(&uri), info.export_names(&uri))
        };

        let (diagnostics, parsed) = self
            .parse_and_validate(&uri, &text, trigger, Some(expected_version))
            .await;
        let code_lenses_changed = parsed.is_some();
        {
            let ast = parsed.map(Arc::new);
            // acquired before documents everywhere to avoid ABBA deadlock with
            self.update_doc_tokens(&uri, ast.as_ref());
            let mut docs = self.state.documents.lock();
            let still_current = docs
                .get(&uri)
                .is_some_and(|document| document.version == expected_version);
            if !still_current {
                return;
            }
            // is still published. (#41) Loc/.cwt files always parse to None
            if let Some(ast) = ast {
                docs.set_ast(&uri, expected_version, ast);
            }
        }
        let still_open = self.state.documents.lock().contains_key(&uri);
        if still_open && let Ok(uri_obj) = Url::parse(&uri) {
            self.publish_gated(
                uri_obj,
                diagnostics,
                Some(expected_version),
                Some(cwtools_cache::workspace::content_hash(&text)),
            )
            .await;
        }

        let (exports_after, names_after) = {
            let info = self.state.info_service.read();
            (info.export_fingerprint(&uri), info.export_names(&uri))
        };
        let exports_changed = exports_before != exports_after;
        let mut changed_names: HashSet<String> = names_before
            .symmetric_difference(&names_after)
            .cloned()
            .collect();
        {
            let mut pending = self.state.pending_changed_names.lock();
            changed_names.extend(pending.drain());
        }
        if exports_changed || !changed_names.is_empty() {
            let scope = if changed_names.is_empty() {
                None
            } else {
                Some(&changed_names)
            };
            self.revalidate_open_dependents(&uri, generation, scope)
                .await;
        } else {
            tracing::debug!(uri = %uri, "exports unchanged; skipping dependent sweep");
        }
        if code_lenses_changed {
            self.request_code_lens_refresh().await;
        }
    }

    pub(crate) async fn revalidate_open_dependents(
        &self,
        changed_uri: &str,
        generation: u64,
        changed_names: Option<&HashSet<String>>,
    ) {
        use std::sync::atomic::Ordering;

        let changed_ids: Option<Vec<StringId>> = changed_names.map(|names| {
            names
                .iter()
                .map(|n| self.state.string_table.intern(n).lower)
                .collect()
        });
        let mut others: Vec<(String, i32, Arc<ParsedFile>, Arc<str>)> = {
            let tokens = self.state.doc_tokens.read();
            let docs = self.state.documents.lock();
            docs.iter()
                .filter(|(u, _)| u.as_str() != changed_uri)
                .filter(|(u, _)| match &changed_ids {
                    None => true,
                    Some(ids) => match tokens.get(u.as_str()) {
                        None => true,
                        Some(doc_set) => ids.iter().any(|id| doc_set.contains(id)),
                    },
                })
                .filter_map(|(u, d)| {
                    d.ast
                        .clone()
                        .map(|ast| (u.clone(), d.version, ast, d.text.clone()))
                })
                .collect()
        };
        others.retain(|(uri, _, _, _)| !self.is_ignored_uri(uri));
        if others.is_empty() {
            return;
        }
        tracing::debug!(
            count = others.len(),
            generation,
            "revalidate_open_dependents"
        );
        let (game, encoding) = {
            let cfg = self.state.config.read();
            (cfg.game(), cfg.position_encoding.clone())
        };
        let validated: Vec<(String, i32, Vec<Diagnostic>, u64)> = {
            let rules_guard = self.state.rules.read();
            let mut out = Vec::with_capacity(others.len());
            for (uri, snapshot_version, ast, text) in others {
                if self.state.edit_generation.load(Ordering::Relaxed) != generation {
                    tracing::debug!(generation, "revalidate_open_dependents superseded");
                    if let Some(names) = changed_names {
                        let mut pending = self.state.pending_changed_names.lock();
                        pending.extend(names.iter().cloned());
                    }
                    break;
                }
                let lines = DocLines::new(&text, encoding.clone());
                let diagnostics = match rules_guard.ruleset.as_ref() {
                    Some(ruleset) => self.validate_parsed_prebuilt(
                        &uri,
                        &ast,
                        &rules_guard.modifier_keys,
                        ruleset,
                        game,
                        rules_guard.scope_registry.as_ref(),
                        &lines,
                    ),
                    None => parse_errors_to_diagnostics(&ast.errors, &lines),
                };
                out.push((
                    uri,
                    snapshot_version,
                    diagnostics,
                    cwtools_cache::workspace::content_hash(&text),
                ));
            }
            out
        };
        let to_publish: Vec<(String, i32, Vec<Diagnostic>, u64)> = validated
            .into_iter()
            .filter(|(uri, snapshot_version, _, _)| {
                let docs = self.state.documents.lock();
                docs.get(uri.as_str())
                    .map(|d| d.version == *snapshot_version)
                    .unwrap_or(false)
            })
            .collect();
        for (uri, snapshot_version, diagnostics, source_hash) in to_publish {
            if let Ok(uri_obj) = Url::parse(&uri) {
                self.publish_filtered(
                    uri_obj,
                    diagnostics,
                    Some(snapshot_version),
                    Some(source_hash),
                )
                .await;
            }
        }
    }

    /// to a watched one — resolves without a full rescan (#36). Bounded by the
    /// in the batch even though nothing between them changed (#87). The revision
    pub(crate) fn loc_overlay_keys(&self) -> Arc<HashSet<String>> {
        let revision = self
            .state
            .loc_overlay_revision
            .load(std::sync::atomic::Ordering::Acquire);
        let cached = {
            let guard = self.state.loc_overlay_keys_cache.lock();
            guard
                .as_ref()
                .filter(|(cached_revision, _)| *cached_revision == revision)
                .map(|(_, keys)| Arc::clone(keys))
        };
        if let Some(keys) = cached {
            return keys;
        }
        let mut keys = HashSet::new();
        for set in self.state.loc_live_overlay.read().values() {
            keys.extend(set.iter().cloned());
        }
        for set in self.state.loc_watched_overlay.read().values() {
            keys.extend(set.iter().cloned());
        }
        let keys = Arc::new(keys);
        *self.state.loc_overlay_keys_cache.lock() = Some((revision, Arc::clone(&keys)));
        keys
    }

    /// workspace rescan (#53). Takes the shared parse of the edited buffer.
    fn update_loc_text_for_file(&self, files: &[cwtools_localization::LocFile]) {
        let hover_all = self
            .state
            .hover_show_all_languages
            .load(std::sync::atomic::Ordering::Relaxed);
        let loc_languages = self.state.config.read().loc_languages.clone();
        let primary_lang = loc_languages
            .as_deref()
            .and_then(|l| l.first().copied())
            .unwrap_or(cwtools_localization::Lang::English);

        let new_entries = {
            let loc_index = self.state.loc_index.read();
            let mut new_entries = LocTextMap::default();
            for file in files {
                let lang = file.lang.unwrap_or(cwtools_localization::Lang::English);
                let lang_included = hover_all || lang == primary_lang;
                if !lang_included {
                    continue;
                }
                for entry in &file.entries {
                    let display = crate::paths::loc_display_text(&entry.desc);
                    if !display.is_empty() {
                        let key = loc_index
                            .as_deref()
                            .and_then(|index| index.key(&entry.key))
                            .unwrap_or_else(|| Arc::from(entry.key.to_lowercase()));
                        new_entries
                            .entry(key)
                            .or_default()
                            .push((lang, display.to_string()));
                    }
                }
            }
            new_entries
        };

        let mut loc_text = self.state.loc_text.write();
        for key in new_entries.keys() {
            loc_text.remove(key);
        }
        for (key, translations) in new_entries {
            loc_text.entry(key).or_default().extend(translations);
        }
    }

    fn loc_ref_names(&self) -> Arc<HashSet<String>> {
        let revision = self
            .state
            .info_revision
            .load(std::sync::atomic::Ordering::Acquire);
        let cached = {
            let guard = self.state.loc_ref_names_cache.lock();
            guard
                .as_ref()
                .filter(|(cached_revision, _)| *cached_revision == revision)
                .map(|(_, names)| Arc::clone(names))
        };
        if let Some(names) = cached {
            return names;
        }
        let extra = {
            let modifier_keys = self.state.rules.read().modifier_keys.clone();
            let info = self.state.info_service.read();
            Arc::new(loc_extra_valid_refs(&modifier_keys, &info.type_index))
        };
        *self.state.loc_ref_names_cache.lock() = Some((revision, Arc::clone(&extra)));
        extra
    }

    fn validate_loc_parsed(
        &self,
        path: &str,
        files: &[cwtools_localization::LocFile],
        lines: &DocLines,
        additional_loc_keys: &HashSet<String>,
        extra: &HashSet<String>,
    ) -> Vec<Diagnostic> {
        let loc_guard = self.state.loc_index.read();
        let empty_union = cwtools_localization::LocKeySet::default();
        let union: &cwtools_localization::LocKeySet = loc_guard
            .as_deref()
            .map(|idx| idx.union())
            .unwrap_or(&empty_union);
        cwtools_localization::pipeline::validate_parsed_loc_files_with_additional_keys(
            files,
            path,
            union,
            additional_loc_keys,
            extra,
        )
        .iter()
        .map(|d| validation_error_to_diagnostic(&loc_diag_to_validation_error(d), lines))
        .collect()
    }

    async fn revalidate_other_open_loc_files(
        &self,
        except_uri: &str,
        changed_keys: &HashSet<String>,
        additional_loc_keys: &HashSet<String>,
        extra: &HashSet<String>,
    ) {
        let mut targets: Vec<OpenLocTarget> = {
            let docs = self.state.documents.lock();
            docs.iter()
                .filter(|(uri, _)| uri.as_str() != except_uri && crate::paths::is_loc_file(uri))
                .filter(|(_, document)| {
                    loc_cache_needs_revalidation(
                        document.loc_cache.as_deref(),
                        document.version,
                        changed_keys,
                    )
                })
                .map(|(uri, document)| OpenLocTarget {
                    uri: uri.clone(),
                    version: document.version,
                    text: Arc::clone(&document.text),
                    cache: document
                        .loc_cache
                        .clone()
                        .filter(|cache| cache.version == document.version),
                })
                .collect()
        };
        targets.retain(|target| !self.is_ignored_uri(&target.uri));
        let encoding = self.state.config.read().position_encoding.clone();
        for target in targets {
            let path = uri_to_path_str(&target.uri);
            let cache = match target.cache {
                Some(cache) => cache,
                None => {
                    let (cache, cacheable) = match try_parse_loc_buffer(&target.text, &path) {
                        Ok(files) => (loc_cache(target.version, target.text.len(), files), true),
                        Err(_) => (
                            loc_cache(target.version, target.text.len(), Vec::new()),
                            false,
                        ),
                    };
                    if cacheable {
                        match self.state.documents.lock().set_loc_cache(
                            &target.uri,
                            target.version,
                            Arc::clone(&cache),
                        ) {
                            Ok(true) => {}
                            Ok(false) => continue,
                            Err(rejection) => {
                                tracing::debug!(
                                    uri = %target.uri,
                                    reason = rejection.reason(),
                                    "localisation cache not retained"
                                );
                            }
                        }
                    }
                    cache
                }
            };
            let lines = DocLines::new(&target.text, encoding.clone());
            let inline_ignored =
                cwtools_validation::inline_ignore::extract_inline_ignored_codes(&target.text);
            let mut diagnostics =
                self.validate_loc_parsed(&path, &cache.files, &lines, additional_loc_keys, extra);
            drop_inline_suppressed(&mut diagnostics, &inline_ignored);
            #[cfg(test)]
            loc_sweep_test_hook::run(self, &target.uri);
            let still_current = self
                .state
                .documents
                .lock()
                .get(&target.uri)
                .is_some_and(|document| document.version == target.version);
            if still_current && let Ok(obj) = Url::parse(&target.uri) {
                self.publish_gated(obj, diagnostics, Some(target.version), None)
                    .await;
            }
        }
    }

    pub(crate) fn record_watched_loc_keys(
        &self,
        uri: &str,
        path: &str,
        text: &str,
    ) -> HashSet<String> {
        let new_keys = loc_keys_of(text, path);
        if self.loc_overlay_entry_matches(&self.state.loc_watched_overlay, uri, &new_keys) {
            return HashSet::new();
        }
        let mut overlay = self.loc_watched_overlay_mut();
        let changed = match overlay.get(uri) {
            Some(prev) => prev.symmetric_difference(&new_keys).cloned().collect(),
            None => new_keys.clone(),
        };
        overlay.insert(uri.to_string(), new_keys);
        changed
    }

    fn loc_overlay_entry_matches(
        &self,
        overlay: &parking_lot::RwLock<std::collections::HashMap<String, HashSet<String>>>,
        uri: &str,
        new_keys: &HashSet<String>,
    ) -> bool {
        overlay.read().get(uri).is_some_and(|prev| prev == new_keys)
    }

    /// whole watched batch whose loc key sets changed (#90). `changed_keys` is
    pub(crate) async fn refresh_after_watched_loc_changes(&self, changed_keys: &HashSet<String>) {
        let additional_loc_keys = self.loc_overlay_keys();
        let extra = self.loc_ref_names();
        self.revalidate_other_open_loc_files("", changed_keys, &additional_loc_keys, &extra)
            .await;
        let generation = self
            .state
            .edit_generation
            .load(std::sync::atomic::Ordering::Relaxed);
        let scope = {
            let rules_guard = self.state.rules.read();
            loc_change_candidate_names(rules_guard.ruleset.as_deref(), changed_keys)
        };
        self.revalidate_open_dependents("", generation, Some(&scope))
            .await;
    }

    #[tracing::instrument(skip_all, fields(uri = %uri, bytes = text.len(), trigger = trigger.as_str()))]
    pub(crate) async fn parse_and_validate(
        &self,
        uri: &str,
        text: &str,
        trigger: crate::ValidateTrigger,
        parsed_version: Option<i32>,
    ) -> (Vec<Diagnostic>, Option<ParsedFile>) {
        if self.is_ignored_uri(uri) {
            self.clear_ignored_file_state(uri);
            self.update_doc_tokens(uri, None);
            return (Vec::new(), None);
        }
        let mut diagnostics = Vec::new();
        // Per-line text + negotiated encoding, so every squiggle spans the whole
        let lines = DocLines::new(text, self.state.config.read().position_encoding.clone());
        let inline_ignored = cwtools_validation::inline_ignore::extract_inline_ignored_codes(text);

        if crate::paths::is_loc_file(uri) {
            let path = uri_to_path_str(uri);
            // full rescan. Record which keys were added or removed. (#36)
            // first sight fire the whole-file cross-file sweep (#90). Watched
            // paths already fence their sync work. (#87)
            let (changed_keys, diagnostics, cache) = tokio::task::block_in_place(|| {
                // whole file itself, and two of them copied it first (#87).
                let parsed_loc = try_parse_loc_buffer(text, &path);
                let cache_version = parsed_version.filter(|_| parsed_loc.is_ok());
                let parsed_loc = parsed_loc.unwrap_or_default();
                let is_open = self.state.documents.lock().contains_key(uri);
                let changed_keys: HashSet<String> = if is_open {
                    let new_keys = loc_keys_from(&parsed_loc);
                    if self.loc_overlay_entry_matches(&self.state.loc_live_overlay, uri, &new_keys)
                    {
                        HashSet::new()
                    } else {
                        let mut overlay = self.loc_live_overlay_mut();
                        let diff = match overlay.get(uri) {
                            Some(prev) => prev.symmetric_difference(&new_keys).cloned().collect(),
                            None => new_keys.clone(),
                        };
                        overlay.insert(uri.to_string(), new_keys);
                        diff
                    }
                } else {
                    HashSet::new()
                };
                let additional_loc_keys = self.loc_overlay_keys();
                let extra = self.loc_ref_names();
                let diagnostics = self.validate_loc_parsed(
                    &path,
                    &parsed_loc,
                    &lines,
                    &additional_loc_keys,
                    &extra,
                );
                // edits without waiting for a full workspace rescan (#53).
                self.update_loc_text_for_file(&parsed_loc);
                let cache = cache_version.map(|version| loc_cache(version, text.len(), parsed_loc));
                (changed_keys, diagnostics, cache)
            });
            if let Some(cache) = cache
                && let Err(rejection) =
                    self.state
                        .documents
                        .lock()
                        .set_loc_cache(uri, cache.version, cache)
            {
                tracing::debug!(
                    %uri,
                    reason = rejection.reason(),
                    "localisation cache not retained"
                );
            }
            // key without a full rescan. (#36) The sweep is scoped to the docs
            if !changed_keys.is_empty() {
                let additional_loc_keys = self.loc_overlay_keys();
                let extra = self.loc_ref_names();
                self.revalidate_other_open_loc_files(
                    uri,
                    &changed_keys,
                    &additional_loc_keys,
                    &extra,
                )
                .await;
                let generation = self
                    .state
                    .edit_generation
                    .load(std::sync::atomic::Ordering::Relaxed);
                let scope = {
                    let rules_guard = self.state.rules.read();
                    loc_change_candidate_names(rules_guard.ruleset.as_deref(), &changed_keys)
                };
                self.revalidate_open_dependents(uri, generation, Some(&scope))
                    .await;
            }
            let mut diagnostics = diagnostics;
            drop_inline_suppressed(&mut diagnostics, &inline_ignored);
            return (diagnostics, None);
        }

        if crate::paths::has_loc_ext(uri) {
            return (diagnostics, None);
        }

        // flag every rule field as unknown). See #43.
        if crate::paths::is_cwt_file(uri) {
            let parsed = parse_string(text, &self.state.string_table);
            diagnostics.extend(parse_errors_to_diagnostics(&parsed.errors, &lines));
            let rules_guard = self.state.rules.read();
            if let Some(ruleset) = rules_guard.ruleset.as_ref() {
                let path = std::path::PathBuf::from(uri_to_path_str(uri));
                let files = [(path, parsed)];
                for err in cwtools_rules::config_validation::validate_ruleset_references(
                    &files,
                    ruleset,
                    &self.state.string_table,
                ) {
                    diagnostics.push(rule_parse_error_to_diagnostic(&err, &lines));
                }
            }
            drop_inline_suppressed(&mut diagnostics, &inline_ignored);
            return (diagnostics, None);
        }

        tracing::debug!(%uri, "[validate] parsing");

        // that do the same work already fence theirs. (#87)
        tokio::task::block_in_place(|| {
            let parsed = parse_string(text, &self.state.string_table);
            diagnostics.extend(parse_errors_to_diagnostics(&parsed.errors, &lines));

            self.index_parsed_file(uri, &parsed, parsed_version);

            // see it on THIS pass too (#259).
            if let Some(name) = self.refresh_inline_script(uri, text) {
                self.state.pending_changed_names.lock().insert(name);
            }

            // Validation. Lock order: rules -> info_service -> loc_index ->
            let (errors, log_msg) = {
                let game = self.state.config.read().game();
                // (CW100/CW122) without a full rescan (#36). Computed before
                let overlay = self.loc_overlay_keys();
                let rules_guard = self.state.rules.read();
                if let Some(ruleset) = rules_guard.ruleset.as_ref() {
                    let start = std::time::Instant::now();
                    let info_guard = self.state.info_service.read();
                    let type_index = &info_guard.type_index;
                    let loc_guard = self.state.loc_index.read();
                    let inline_guard = self.state.inline_scripts.read();
                    let (scope_checks, var_checks) = {
                        let cfg = self.state.config.read();
                        (cfg.scope_checks, cfg.var_checks)
                    };
                    let prepared = make_prepared(
                        ruleset,
                        &self.state.string_table,
                        game,
                        type_index,
                        &rules_guard.modifier_keys,
                        loc_guard.as_deref(),
                        Some(&overlay),
                        Some(&inline_guard),
                        rules_guard.scope_registry.as_ref(),
                        scope_checks,
                        var_checks,
                    );
                    let track = needs_use_tracking(ruleset, game);
                    let (mut errs, used) = if track {
                        let (errs, used) = validate_prepared_tracking_uses(&parsed, uri, &prepared);
                        (errs, Some(used))
                    } else {
                        (validate_prepared(&parsed, uri, &prepared), None)
                    };
                    if let Some(used) = used {
                        errs.extend(
                            self.unused_instance_errors(uri, used, ruleset, game, type_index),
                        );
                    }
                    append_missing_loc_errors(uri, &prepared, &mut errs);
                    drop(loc_guard);
                    drop(info_guard);
                    let elapsed = start.elapsed();
                    let total = truncate_validation_errors(&mut errs, uri);
                    let msg = format!(
                        "[validate] ({}) {} errors in {:?} ({} types, {} enums, {} aliases)",
                        trigger.as_str(),
                        total,
                        elapsed,
                        ruleset.types.len(),
                        ruleset.enums.len(),
                        ruleset.aliases.len()
                    );
                    (errs, Some(msg))
                } else {
                    (Vec::new(), None)
                }
            };

            if let Some(msg) = log_msg {
                tracing::info!(target: "cwtools::profile", "{}", msg);
            }

            for err in &errors {
                diagnostics.push(validation_error_to_diagnostic(err, &lines));
            }
            drop_inline_suppressed(&mut diagnostics, &inline_ignored);
            (diagnostics, Some(parsed))
        })
    }
}

#[cfg(test)]
mod perf_bench {
    use super::*;
    use std::collections::HashMap;

    fn bench<F: FnMut() -> usize>(label: &str, iters: usize, mut f: F) {
        for _ in 0..3 {
            f();
        }
        let mut times = Vec::with_capacity(iters);
        let mut n = 0;
        for _ in 0..iters {
            let t = std::time::Instant::now();
            n = f();
            times.push(t.elapsed());
        }
        times.sort();
        let mean = times.iter().sum::<std::time::Duration>() / iters as u32;
        eprintln!(
            "{:>28}: mean {:>10.1?}  min {:>10.1?}  max {:>10.1?}  (count {}, n={})",
            label,
            mean,
            times[0],
            times[iters - 1],
            n,
            iters
        );
    }

    #[test]
    #[ignore]
    fn perf_keystroke_validate() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../testfiles/performancetest2/events/cc_colony_events.txt"
        );
        let text = std::fs::read_to_string(fixture).expect("fixture").repeat(8);
        eprintln!("fixture: {} bytes", text.len());
        let table = cwtools_string_table::string_table::StringTable::new();
        let parsed = parse_string(&text, &table);

        bench("collect_doc_tokens", 30, || {
            collect_doc_tokens(&parsed).len()
        });

        let ws: Option<Arc<str>> = Some(crate::paths::workspace_prefix_of(
            "file:///mnt/mods/millennium_dawn",
        ));
        let uri = "file:///mnt/mods/millennium_dawn/events/some_event_file.txt";
        bench("logical_path_from_uri x1000", 30, || {
            let mut total = 0usize;
            for _ in 0..1000 {
                total += logical_path_from_uri(uri, &ws).len();
            }
            total
        });
    }

    /// The parse a loc keystroke pays, before and after #87. The edited buffer
    #[test]
    #[ignore]
    fn perf_loc_edit_parse() {
        const ENTRIES: usize = 4_000;
        let path = "localisation/bench_l_english.yml";
        let mut text = String::from("\u{FEFF}l_english:\n");
        for i in 0..ENTRIES {
            text.push_str(&format!(
                " key_{i:05}:0 \"Localised text for $other_key_{i:05}$ with [GetName] in it\"\n"
            ));
        }
        eprintln!("fixture: {} bytes, {ENTRIES} entries", text.len());

        bench("loc parse x1 (after)", 20, || {
            parse_loc_buffer(&text, path).len()
        });
        bench("loc parse x3 + 2 copies (before)", 20, || {
            let owned = text.to_string();
            let a = parse_loc_buffer(&owned, path);
            let b = parse_loc_buffer(&text, path);
            let owned = text.to_string();
            let c = parse_loc_buffer(&owned, path);
            a.len() + b.len() + c.len()
        });
    }

    #[test]
    #[ignore]
    fn perf_loc_ref_names() {
        const MODIFIER_KEYS: usize = 50_000;
        const TYPE_INSTANCES: usize = 190_000;

        let modifier_keys: HashSet<String> = (0..MODIFIER_KEYS)
            .map(|i| format!("modifier_key_{:06}", i))
            .collect();
        let mut type_index = cwtools_info::TypeIndex::new();
        let mut per_type: HashMap<String, Vec<cwtools_info::TypeInstance>> = HashMap::new();
        for i in 0..TYPE_INSTANCES {
            per_type
                .entry(format!("type_{:02}", i % 40))
                .or_default()
                .push(cwtools_info::TypeInstance {
                    name: format!("instance_name_{:06}", i),
                    location: cwtools_info::SourceLocation {
                        line: 1,
                        col: 0,
                        end: (1, 0),
                    },
                    primary_loc_key: None,
                    required_loc_keys: Vec::new(),
                });
        }
        type_index.merge("file:///bench/defs.txt", per_type);
        eprintln!(
            "fixture: {} modifier keys, {} type instances",
            modifier_keys.len(),
            TYPE_INSTANCES
        );

        bench("loc_extra_valid_refs", 20, || {
            loc_extra_valid_refs(&modifier_keys, &type_index).len()
        });

        let base = loc_extra_valid_refs(&modifier_keys, &type_index);
        let overlay: HashSet<String> = (0..4_000).map(|i| format!("overlay_key_{i:06}")).collect();
        bench("overlay merge (before)", 20, || {
            let mut combined = base.clone();
            combined.extend(overlay.iter().cloned());
            combined.len()
        });
        bench("overlay rebuild (after)", 20, || {
            let mut keys = HashSet::new();
            keys.extend(overlay.iter().cloned());
            keys.len()
        });
    }

    #[test]
    #[ignore]
    fn perf_index_parsed_file() {
        let expand = |p: &str| -> std::path::PathBuf {
            match p.strip_prefix("~/") {
                Some(rest) => std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(rest),
                None => std::path::PathBuf::from(p),
            }
        };
        let rules_dir = expand(
            &std::env::var("CWTOOLS_PERF_RULES")
                .unwrap_or_else(|_| "~/Documents/github-projects/cwtools-hoi4-config".to_string()),
        )
        .join("Config");
        let vanilla = expand(&std::env::var("CWTOOLS_PERF_VANILLA").unwrap_or_else(|_| {
            "~/.local/share/Steam/steamapps/common/Hearts of Iron IV".to_string()
        }));
        if !rules_dir.is_dir() || !vanilla.is_dir() {
            eprintln!(
                "perf_index_parsed_file: skipping, need {} and {}",
                rules_dir.display(),
                vanilla.display()
            );
            return;
        }

        let table = cwtools_string_table::string_table::StringTable::new();
        let (ruleset, _errors) = cwtools_rules::ruleset_loader::load_ruleset_from_dir(
            &rules_dir,
            &table,
            cwtools_file_manager::file_manager::ScanBudget::default(),
        );
        eprintln!(
            "ruleset: {} types / {} aliases from {}",
            ruleset.types.len(),
            ruleset.aliases.len(),
            rules_dir.display()
        );

        for logical_path in [
            "common/national_focus/germany.txt",
            "events/WUW_Germany.txt",
            "common/units/equipment/plane_airframes.txt",
        ] {
            let game_file = vanilla.join(logical_path);
            let Ok(text) = std::fs::read_to_string(&game_file) else {
                eprintln!("  skipping missing fixture {}", game_file.display());
                continue;
            };
            let parsed = parse_string(&text, &table);
            let uri = format!("file:///bench/{}", logical_path);
            eprintln!("fixture: {} ({} bytes)", logical_path, text.len());

            bench("collect_type_instances_with_subtypes", 20, || {
                let collected = cwtools_info::collect_type_instances_with_subtypes(
                    &ruleset,
                    &parsed,
                    logical_path,
                    &table,
                    cwtools_validation::subtype_membership_for_instance,
                );
                collected.instances.len() + collected.subtype_instances.len()
            });
            let mut info = cwtools_info::InfoService::new();
            bench("collect + clear_file + index_file", 20, || {
                let collected = cwtools_info::collect_type_instances_with_subtypes(
                    &ruleset,
                    &parsed,
                    logical_path,
                    &table,
                    cwtools_validation::subtype_membership_for_instance,
                );
                info.clear_file(&uri);
                info.index_file_with_precomputed_instances(
                    &uri,
                    &parsed,
                    &table,
                    &ruleset,
                    logical_path,
                    collected.instances,
                    collected.subtype_instances,
                );
                info.export_fingerprint(&uri) as usize
            });

            let mut type_index = cwtools_info::TypeIndex::new();
            type_index.merge(
                &uri,
                cwtools_info::collect_type_instances(&ruleset, &parsed, logical_path, &table),
            );
            bench("CW100 recollect + check", 20, || {
                let per_type =
                    cwtools_info::collect_type_instances(&ruleset, &parsed, logical_path, &table);
                let instances: Vec<_> = per_type
                    .iter()
                    .flat_map(|(type_name, values)| {
                        values
                            .iter()
                            .map(move |instance| (type_name.as_str(), instance))
                    })
                    .collect();
                cwtools_validation::missing_loc::check_missing_localisation(
                    &instances,
                    logical_path,
                    &logical_path.into(),
                    &ruleset,
                    |_| true,
                )
                .len()
            });
            bench("CW100 indexed check", 20, || {
                let instances = type_index.instances_in_file(&uri);
                cwtools_validation::missing_loc::check_missing_localisation(
                    &instances,
                    logical_path,
                    &logical_path.into(),
                    &ruleset,
                    |_| true,
                )
                .len()
            });
        }
    }
}

/// export fingerprint moved (#289). These pin both halves: an edit that changes
#[cfg(test)]
mod info_revision_tests {
    use super::*;
    use crate::{Backend, CompletionCacheEntry, DocumentState};
    use cwtools_rules::rules_types::{PathOptions, TypeDefinition};
    use std::sync::atomic::Ordering;

    /// Workspace URI for the fixtures. Drive-lettered on Windows so the derived
    const WORKSPACE_URI: &str = if cfg!(windows) {
        "file:///C:/ws"
    } else {
        "file:///ws"
    };

    fn ideas_uri() -> String {
        format!("{WORKSPACE_URI}/common/ideas/00_ideas.txt")
    }

    fn test_backend() -> Backend {
        let state = Arc::new(DocumentState::new());
        let captured = Arc::new(parking_lot::Mutex::new(None));
        let slot = captured.clone();
        let server_state = state.clone();
        let (_service, _socket) = tower_lsp::LspService::new(move |client| {
            *slot.lock() = Some(client.clone());
            Backend {
                client,
                state: server_state.clone(),
            }
        });
        let client = captured.lock().take().unwrap();
        state.config.write().workspace_prefix =
            Some(crate::paths::workspace_prefix_of(WORKSPACE_URI));
        let mut ruleset = RuleSet::new();
        ruleset.types.push(TypeDefinition {
            name: "idea".to_string(),
            name_field: None,
            path_options: PathOptions {
                paths: vec!["common/ideas".to_string()],
                ..Default::default()
            },
            subtypes: Vec::new(),
            type_key_filter: None,
            skip_root_key: Vec::new(),
            starts_with: None,
            type_per_file: false,
            key_prefix: None,
            warning_only: false,
            unique: false,
            should_be_referenced: false,
            localisation: Vec::new(),
            graph_related_types: Vec::new(),
            modifiers: Vec::new(),
        });
        ruleset.reindex();
        state.rules.write().ruleset = Some(Arc::new(ruleset));
        Backend { client, state }
    }

    fn index(backend: &Backend, uri: &str, text: &str) {
        let parsed = parse_string(text, &backend.state.string_table);
        backend.index_parsed_file(uri, &parsed, None);
    }

    fn revision(backend: &Backend) -> u64 {
        backend.state.info_revision.load(Ordering::Relaxed)
    }

    fn seed_fallback_cache(backend: &Backend) {
        *backend.state.fallback_cache.lock() = Some(CompletionCacheEntry {
            revision: revision(backend),
            items: vec![CompletionItem {
                label: "seeded".to_string(),
                ..Default::default()
            }],
        });
    }

    fn fallback_cache_hits(backend: &Backend) -> bool {
        let current = revision(backend);
        backend
            .state
            .fallback_cache
            .lock()
            .as_ref()
            .is_some_and(|entry| entry.revision == current && !entry.items.is_empty())
    }

    fn open_loc_doc(backend: &Backend, uri: &str, text: &str) {
        backend
            .state
            .documents
            .lock()
            .open(
                uri.to_string(),
                crate::state::ParsedDoc {
                    version: 1,
                    text: Arc::from(text),
                    ast: None,
                    ast_version: None,
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_loc_cache_miss_is_parsed_and_installed() {
        let backend = test_backend();
        let uri = format!("{WORKSPACE_URI}/localisation/target_l_english.yml");
        open_loc_doc(&backend, &uri, "l_english:\n KEY:0 \"$other_ref$\"\n");

        backend
            .revalidate_other_open_loc_files(
                "",
                &HashSet::from(["unrelated_key".to_string()]),
                &HashSet::new(),
                &HashSet::new(),
            )
            .await;

        let documents = backend.state.documents.lock();
        let cache = documents
            .get(&uri)
            .and_then(|document| document.loc_cache.as_ref())
            .expect("cache installed");
        assert_eq!(cache.version, 1);
        assert!(cache.references.contains("other_ref"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_fatal_loc_parse_stays_a_cache_miss() {
        let backend = test_backend();
        let uri = format!("{WORKSPACE_URI}/localisation/target_l_english.yml");
        open_loc_doc(&backend, &uri, "l_english");

        backend
            .revalidate_other_open_loc_files(
                "",
                &HashSet::from(["unrelated_key".to_string()]),
                &HashSet::new(),
                &HashSet::new(),
            )
            .await;

        assert!(
            backend
                .state
                .documents
                .lock()
                .get(&uri)
                .is_some_and(|document| document.loc_cache.is_none())
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_loc_key_change_only_invalidates_the_overlay_cache() {
        let backend = test_backend();
        let uri = format!("{WORKSPACE_URI}/localisation/test_l_english.yml");
        let text: Arc<str> = Arc::from("l_english:\n NEW_KEY:0 \"$new_key$\"\n");
        backend
            .state
            .documents
            .lock()
            .open(
                uri.clone(),
                crate::state::ParsedDoc {
                    version: 1,
                    text: Arc::clone(&text),
                    ast: None,
                    ast_version: None,
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .unwrap();
        let settled = revision(&backend);
        let names_before = backend.loc_ref_names();
        let overlay_before = backend.loc_overlay_keys();
        seed_fallback_cache(&backend);

        let (diagnostics, _) = backend
            .parse_and_validate(&uri, &text, crate::ValidateTrigger::DidChange, Some(1))
            .await;

        assert_eq!(revision(&backend), settled);
        assert!(Arc::ptr_eq(&names_before, &backend.loc_ref_names()));
        let overlay_after = backend.loc_overlay_keys();
        assert!(!Arc::ptr_eq(&overlay_before, &overlay_after));
        assert!(overlay_after.contains("new_key"));
        assert!(fallback_cache_hits(&backend));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == Some(NumberOrString::String("CW259".to_string()))
        }));
    }

    #[test]
    fn clearing_ignored_loc_state_invalidates_both_overlays() {
        let backend = test_backend();
        let uri = format!("{WORKSPACE_URI}/localisation/ignored_l_english.yml");
        backend
            .loc_live_overlay_mut()
            .insert(uri.clone(), HashSet::from(["live_key".to_string()]));
        backend
            .loc_watched_overlay_mut()
            .insert(uri.clone(), HashSet::from(["watched_key".to_string()]));
        let settled = revision(&backend);
        let before = backend.loc_overlay_keys();

        backend.clear_ignored_file_state(&uri);

        assert_eq!(revision(&backend), settled);
        let after = backend.loc_overlay_keys();
        assert!(!Arc::ptr_eq(&before, &after));
        assert!(after.is_empty());
        assert!(!backend.state.loc_live_overlay.read().contains_key(&uri));
        assert!(!backend.state.loc_watched_overlay.read().contains_key(&uri));
    }

    #[tokio::test]
    async fn first_index_of_a_new_file_bumps_the_revision() {
        let backend = test_backend();
        let before = revision(&backend);
        index(&backend, &ideas_uri(), "my_idea = {\n\tcost = 5\n}\n");
        assert_ne!(
            revision(&backend),
            before,
            "a file's first index publishes exports nothing had seen"
        );
        assert!(backend.loc_ref_names().contains("my_idea"));
    }

    #[tokio::test]
    async fn editing_a_rule_body_leaves_the_derived_caches_valid() {
        let backend = test_backend();
        let uri = ideas_uri();
        index(&backend, &uri, "my_idea = {\n\tcost = 5\n}\n");
        let settled = revision(&backend);
        let names_before = backend.loc_ref_names();
        seed_fallback_cache(&backend);

        index(&backend, &uri, "my_idea = {\n\tcost = 7\n}\n");

        assert_eq!(
            revision(&backend),
            settled,
            "an edit inside a rule body exports the same symbols"
        );
        let names_after = backend.loc_ref_names();
        assert!(
            Arc::ptr_eq(&names_before, &names_after),
            "the loc `$ref$` name set must be served from the cache, not rebuilt"
        );
        assert!(
            names_after.contains("my_idea"),
            "and it must still be right"
        );
        assert!(
            fallback_cache_hits(&backend),
            "the completion fallback list must still be a cache hit"
        );
    }

    #[tokio::test]
    async fn adding_an_export_bumps_and_invalidates() {
        let backend = test_backend();
        let uri = ideas_uri();
        index(&backend, &uri, "my_idea = {\n\tcost = 5\n}\n");
        let settled = revision(&backend);
        let names_before = backend.loc_ref_names();
        seed_fallback_cache(&backend);

        index(
            &backend,
            &uri,
            "my_idea = {\n\tcost = 5\n}\nanother_idea = {\n\tcost = 1\n}\n",
        );

        assert_ne!(revision(&backend), settled, "a new definition is an export");
        assert!(!fallback_cache_hits(&backend));
        let names_after = backend.loc_ref_names();
        assert!(!Arc::ptr_eq(&names_before, &names_after));
        assert!(names_after.contains("another_idea"));
    }

    #[tokio::test]
    async fn renaming_an_export_bumps_and_invalidates() {
        let backend = test_backend();
        let uri = ideas_uri();
        index(&backend, &uri, "my_idea = {\n\tcost = 5\n}\n");
        let settled = revision(&backend);
        seed_fallback_cache(&backend);

        index(&backend, &uri, "renamed_idea = {\n\tcost = 5\n}\n");

        assert_ne!(revision(&backend), settled, "a rename moves two names");
        assert!(!fallback_cache_hits(&backend));
        let names = backend.loc_ref_names();
        assert!(names.contains("renamed_idea"));
        assert!(!names.contains("my_idea"), "the old name must be gone");
    }

    #[tokio::test]
    async fn removing_every_export_bumps_and_invalidates() {
        let backend = test_backend();
        let uri = ideas_uri();
        index(&backend, &uri, "my_idea = {\n\tcost = 5\n}\n");
        let settled = revision(&backend);
        seed_fallback_cache(&backend);

        index(&backend, &uri, "\n");

        assert_ne!(revision(&backend), settled);
        assert!(!fallback_cache_hits(&backend));
        assert!(
            !backend.loc_ref_names().contains("my_idea"),
            "a removed definition must not keep resolving"
        );
    }

    #[tokio::test]
    async fn defined_variables_and_event_targets_bump_the_revision() {
        let backend = test_backend();
        let uri = ideas_uri();
        index(&backend, &uri, "my_idea = {\n\tcost = 5\n}\n");

        let settled = revision(&backend);
        seed_fallback_cache(&backend);
        index(&backend, &uri, "@my_var = 3\nmy_idea = {\n\tcost = 5\n}\n");
        assert_ne!(revision(&backend), settled, "an @-var is an export");
        assert!(!fallback_cache_hits(&backend));
        assert!(
            backend
                .state
                .info_service
                .read()
                .variable_counts
                .contains_key("@my_var")
        );

        let settled = revision(&backend);
        seed_fallback_cache(&backend);
        index(
            &backend,
            &uri,
            "@my_var = 3\nmy_idea = {\n\tsave_event_target_as = my_target\n}\n",
        );
        assert_ne!(revision(&backend), settled, "an event target is an export");
        assert!(!fallback_cache_hits(&backend));
        assert!(
            backend
                .state
                .info_service
                .read()
                .event_target_counts
                .contains_key("my_target")
        );
    }

    #[tokio::test]
    async fn a_file_with_no_exports_never_bumps() {
        let backend = test_backend();
        let uri = format!("{WORKSPACE_URI}/events/some_events.txt");
        let before = revision(&backend);
        index(&backend, &uri, "country_event = {\n\tid = test.1\n}\n");
        assert_eq!(revision(&backend), before);
        index(&backend, &uri, "country_event = {\n\tid = test.2\n}\n");
        assert_eq!(revision(&backend), before);
    }
}

#[cfg(test)]
mod truncation_tests {
    use super::*;
    use cwtools_validation::ErrorSeverity;

    fn error(code: Option<&'static str>) -> ValidationError {
        ValidationError {
            message: "x".into(),
            severity: ErrorSeverity::Error,
            line: 1,
            col: 0,
            file: "f".into(),
            code,
            fix: None,
            end: None,
            related: Vec::new(),
        }
    }

    fn tail_codes(errs: &[ValidationError]) -> Vec<Option<&'static str>> {
        errs.iter().rev().take(3).rev().map(|e| e.code).collect()
    }

    #[test]
    fn under_the_cap_nothing_is_touched() {
        let mut errs: Vec<_> = (0..MAX_FILE_ERRORS).map(|_| error(Some("CW240"))).collect();
        assert_eq!(truncate_validation_errors(&mut errs, "f"), MAX_FILE_ERRORS);
        assert_eq!(errs.len(), MAX_FILE_ERRORS);
        assert!(errs.iter().all(|e| e.code == Some("CW240")));
    }

    #[test]
    fn over_the_cap_truncates_and_counts_the_remainder() {
        let mut errs: Vec<_> = (0..MAX_FILE_ERRORS + 5)
            .map(|_| error(Some("CW240")))
            .collect();
        assert_eq!(
            truncate_validation_errors(&mut errs, "f"),
            MAX_FILE_ERRORS + 5
        );
        assert_eq!(errs.len(), MAX_FILE_ERRORS + 1);
        let marker = errs.last().expect("summary marker");
        assert_eq!(marker.code, None);
        assert!(
            marker.message.contains("5 additional"),
            "got: {}",
            marker.message
        );
    }

    #[test]
    fn the_branch_limit_survives_a_flood() {
        let mut errs: Vec<_> = (0..MAX_FILE_ERRORS + 5)
            .map(|_| error(Some("CW240")))
            .collect();
        errs.push(error(Some("CW277")));
        let total = truncate_validation_errors(&mut errs, "f");
        assert_eq!(total, MAX_FILE_ERRORS + 6);
        assert_eq!(
            errs.last().map(|e| e.code),
            Some(Some("CW277")),
            "the branch-limit diagnostic must still be published: {:?}",
            tail_codes(&errs)
        );
        let marker = &errs[errs.len() - 2];
        assert_eq!(marker.code, None);
        assert!(
            marker.message.contains("5 additional"),
            "the remainder count must exclude the held-back CW277, got: {}",
            marker.message
        );
    }

    #[test]
    fn the_branch_limit_alone_does_not_trip_the_cap() {
        let mut errs: Vec<_> = (0..MAX_FILE_ERRORS).map(|_| error(Some("CW240"))).collect();
        errs.push(error(Some("CW277")));
        truncate_validation_errors(&mut errs, "f");
        assert_eq!(errs.len(), MAX_FILE_ERRORS + 1);
        assert!(
            errs.iter().all(|e| e.code.is_some()),
            "no summary marker expected: {:?}",
            tail_codes(&errs)
        );
        assert_eq!(errs.last().map(|e| e.code), Some(Some("CW277")));
    }

    #[test]
    fn parse_errors_over_the_cap_are_truncated() {
        let errors: Vec<_> = (0..MAX_FILE_ERRORS + 5)
            .map(|i| ParseError::Pos(1, 0, format!("e{i}")))
            .collect();
        let diags = parse_errors_to_diagnostics(&errors, &DocLines::none());
        assert_eq!(diags.len(), MAX_FILE_ERRORS + 1);
        let marker = diags.last().expect("summary marker");
        assert_eq!(marker.severity, Some(DiagnosticSeverity::INFORMATION));
        assert_eq!(marker.code, None);
        assert!(
            marker.message.contains("5 additional"),
            "got: {}",
            marker.message
        );
        assert!(
            diags[..MAX_FILE_ERRORS]
                .iter()
                .all(|d| d.severity == Some(DiagnosticSeverity::ERROR))
        );
    }
}

#[cfg(test)]
mod whole_line_range_tests {
    use super::*;
    use cwtools_validation::ErrorSeverity;
    use std::collections::HashMap;

    #[test]
    fn loc_keys_of_extracts_lowercased_keys() {
        // Live-overlay key extraction for #36: keys are lowercased to match the
        let keys = loc_keys_of(
            "l_english:\n MY_Key: \"hi\"\n other_key: \"x\"\n",
            "a_l_english.yml",
        );
        assert!(keys.contains("my_key"), "got: {:?}", keys);
        assert!(keys.contains("other_key"), "got: {:?}", keys);
        assert!(!keys.contains("absent"));
    }

    #[test]
    fn loc_cache_scopes_mixed_case_refs_and_falls_back_safely() {
        let text = "l_english:\n malformed line\n KEY:0 \"$MiXeD_Key$\"\n";
        let files = try_parse_loc_buffer(text, "a_l_english.yml").unwrap();
        assert!(!files[0].parse_errors.is_empty());
        let cache = loc_cache(7, text.len(), files);
        assert!(cache.references.contains("mixed_key"));

        let matching = HashSet::from(["mixed_key".to_string()]);
        let unrelated = HashSet::from(["other_key".to_string()]);
        assert!(loc_cache_needs_revalidation(Some(&cache), 7, &matching));
        assert!(!loc_cache_needs_revalidation(Some(&cache), 7, &unrelated));
        assert!(loc_cache_needs_revalidation(Some(&cache), 8, &unrelated));
        assert!(loc_cache_needs_revalidation(None, 7, &unrelated));
        assert!(try_parse_loc_buffer("l_english", "a_l_english.yml").is_err());
    }

    #[test]
    fn loc_change_candidates_cover_derived_cw100_keys() {
        use cwtools_rules::rules_types::{TypeDefinition, TypeLocalisation};
        let mut rs = RuleSet::new();
        let loc = |prefix: &str, suffix: &str, required: bool, optional: bool| TypeLocalisation {
            name: "x".into(),
            prefix: prefix.into(),
            suffix: suffix.into(),
            required,
            optional,
            explicit_field: None,
            replace_scopes: None,
            primary: false,
        };
        rs.types.push(TypeDefinition {
            name: "thing".to_string(),
            name_field: None,
            path_options: Default::default(),
            subtypes: Vec::new(),
            type_key_filter: None,
            skip_root_key: Vec::new(),
            starts_with: None,
            type_per_file: false,
            key_prefix: None,
            warning_only: false,
            unique: false,
            should_be_referenced: false,
            localisation: vec![
                loc("", "_desc", true, false),
                loc("mod_", "", true, false),
                loc("", "_opt", true, true),
            ],
            graph_related_types: Vec::new(),
            modifiers: Vec::new(),
        });
        let changed: HashSet<String> = ["my_focus_desc".to_string(), "plainkey".to_string()]
            .into_iter()
            .collect();
        let names = loc_change_candidate_names(Some(&rs), &changed);
        assert!(names.contains("my_focus_desc"));
        assert!(names.contains("plainkey"));
        assert!(names.contains("my_focus"), "got: {:?}", names);
        assert!(
            !names.contains("my_focus_desc_opt") && !names.contains("my_focus_d"),
            "optional affixes must not expand: {:?}",
            names
        );
        assert_eq!(loc_change_candidate_names(None, &changed), changed);
    }

    #[test]
    fn loc_extra_valid_refs_covers_every_bindable_name() {
        use cwtools_info::{SourceLocation, TypeIndex, TypeInstance};
        let instance = |name: &str| TypeInstance {
            name: name.to_string(),
            location: SourceLocation {
                line: 1,
                col: 0,
                end: (1, 0),
            },
            primary_loc_key: None,
            required_loc_keys: Vec::new(),
        };
        let mut index = TypeIndex::new();
        index.merge(
            "file:///mod/common/ideas/x.txt",
            HashMap::from([
                ("idea".to_string(), vec![instance("my_idea")]),
                (
                    "dynamic_modifier".to_string(),
                    vec![instance("My_Dynamic_Modifier")],
                ),
            ]),
        );
        index.var_index.add_name("my_variable");
        let modifier_keys = HashSet::from(["stability_factor".to_string()]);

        let refs = loc_extra_valid_refs(&modifier_keys, &index);
        for name in [
            "stability_factor",
            "my_idea",
            "my_dynamic_modifier",
            "my_variable",
        ] {
            assert!(refs.contains(name), "{name} must resolve, got {refs:?}");
        }
    }

    /// Line view in the LSP default encoding, as VS Code negotiates it.
    fn utf16_lines(text: &str) -> DocLines<'_> {
        DocLines::new(text, PositionEncodingKind::UTF16)
    }

    #[test]
    fn diagnostic_end_stops_at_trimmed_line_content() {
        let text = "abc\n  hello world  \n";
        let lines = utf16_lines(text);
        assert_eq!(lines.end_position(0, 0).character, 3, "\"abc\" has 3 chars");
        assert_eq!(
            lines.end_position(1, 0).character,
            13,
            "\"  hello world\" (trailing ws trimmed) has 13 chars"
        );
    }

    #[test]
    fn diagnostic_range_uses_negotiated_encoding() {
        // 2 UTF-16 units, so the whole-line end moves with the encoding.
        let text = "😀 custom_cost_text = a\n";
        let err = ValidationError {
            message: "x".into(),
            severity: ErrorSeverity::Warning,
            line: 1,
            col: 2, // parser char column of `custom_cost_text`
            file: "f".into(),
            code: Some("CW242"),
            fix: None,
            end: None,
            related: Vec::new(),
        };
        let utf16 = validation_error_to_diagnostic(&err, &utf16_lines(text));
        assert_eq!(utf16.range.start.character, 3);
        assert_eq!(utf16.range.end.character, 23, "22 chars, 23 UTF-16 units");
        // A client that negotiated utf-32 gets the parser's columns unchanged.
        let utf32 =
            validation_error_to_diagnostic(&err, &DocLines::new(text, PositionEncodingKind::UTF32));
        assert_eq!(utf32.range.start.character, 2);
        assert_eq!(utf32.range.end.character, 22);
    }

    #[test]
    fn diagnostic_spans_from_field_to_end_of_line() {
        let text = "decision = {\n    custom_cost_text = a\n}\n";
        let ends = utf16_lines(text);
        let err = ValidationError {
            message: "x".into(),
            severity: ErrorSeverity::Warning,
            line: 2, // 1-based: the custom_cost_text line
            col: 4,  // start of the field, after the indentation
            file: "f".into(),
            code: Some("CW242"),
            fix: None,
            end: None,
            related: Vec::new(),
        };
        let diag = validation_error_to_diagnostic(&err, &ends);
        assert_eq!(diag.range.start.line, 1);
        assert_eq!(diag.range.start.character, 4);
        assert_eq!(diag.range.end.line, 1);
        assert_eq!(diag.range.end.character, 24);
    }

    #[test]
    fn diagnostic_uses_precise_range_when_end_present() {
        let text = "decision = {\n    custom_cost_text = a\n}\n";
        let ends = utf16_lines(text);
        let err = ValidationError {
            message: "x".into(),
            severity: ErrorSeverity::Warning,
            line: 2,
            col: 4,
            file: "f".into(),
            code: Some("CW240"),
            fix: None,
            end: Some((2, 24)),
            related: Vec::new(),
        };
        let diag = validation_error_to_diagnostic(&err, &ends);
        assert_eq!(diag.range.start.line, 1);
        assert_eq!(diag.range.start.character, 4);
        assert_eq!(diag.range.end.line, 1);
        assert_eq!(diag.range.end.character, 24);
    }

    #[test]
    fn diagnostic_precise_range_can_span_lines() {
        let text = "root = {\n    foo = {\n        a = 1\n    }\n}\n";
        let ends = utf16_lines(text);
        let err = ValidationError {
            message: "Unexpected block 'foo'".into(),
            severity: ErrorSeverity::Error,
            line: 2, // `foo = {` line
            col: 4,
            file: "f".into(),
            code: Some("CW262"),
            fix: None,
            end: Some((5, 0)), // what the parser actually emits: start of `}`
            related: Vec::new(),
        };
        let diag = validation_error_to_diagnostic(&err, &ends);
        assert_eq!(diag.range.start.line, 1);
        assert_eq!(diag.range.start.character, 4);
        assert_eq!(
            diag.range.end.line, 3,
            "end stays on the closing-brace line"
        );
        assert_eq!(diag.range.end.character, 5);
    }

    // Issue #107. Each `end` below is what the parser actually emits.

    #[test]
    fn diagnostic_end_does_not_bleed_onto_the_next_line() {
        let text = "decision = {\n    custom_cost_text = a\n}\n";
        let ends = utf16_lines(text);
        let err = ValidationError {
            message: "x".into(),
            severity: ErrorSeverity::Warning,
            line: 2,
            col: 4,
            file: "f".into(),
            code: Some("CW240"),
            fix: None,
            end: Some((3, 0)), // start of the `}` line
            related: Vec::new(),
        };
        let diag = validation_error_to_diagnostic(&err, &ends);
        assert_eq!(diag.range.end.line, 1, "must stay on the statement's line");
        assert_eq!(diag.range.end.character, 24);
    }

    #[test]
    fn diagnostic_end_does_not_bleed_onto_the_next_indent() {
        let text = "c = {\n    picture = generic_economy\n    allowed = {\n    }\n}\n";
        let ends = utf16_lines(text);
        let err = ValidationError {
            message: "x".into(),
            severity: ErrorSeverity::Error,
            line: 2,
            col: 4,
            file: "f".into(),
            code: Some("CW500"),
            fix: None,
            end: Some((3, 4)), // start of `allowed` on the next line
            related: Vec::new(),
        };
        let diag = validation_error_to_diagnostic(&err, &ends);
        assert_eq!(diag.range.end.line, 1);
        assert_eq!(diag.range.end.character, 29);
    }

    #[test]
    fn diagnostic_end_never_precedes_its_start() {
        let text = "a = {\n    \n}\n";
        let ends = utf16_lines(text);
        let err = ValidationError {
            message: "x".into(),
            severity: ErrorSeverity::Error,
            line: 2,
            col: 4,
            file: "f".into(),
            code: Some("CW262"),
            fix: None,
            end: Some((3, 0)),
            related: Vec::new(),
        };
        let diag = validation_error_to_diagnostic(&err, &ends);
        assert!(
            (diag.range.end.line, diag.range.end.character)
                >= (diag.range.start.line, diag.range.start.character),
            "range must not invert: {:?}",
            diag.range
        );
    }

    #[test]
    fn diagnostic_end_clamps_before_utf16_conversion() {
        let text = "a = {\n    x = \"\u{1F600}\"\n}\n";
        let ends = utf16_lines(text);
        let err = ValidationError {
            message: "x".into(),
            severity: ErrorSeverity::Error,
            line: 2,
            col: 4,
            file: "f".into(),
            code: Some("CW500"),
            fix: None,
            end: Some((3, 0)),
            related: Vec::new(),
        };
        let diag = validation_error_to_diagnostic(&err, &ends);
        assert_eq!(diag.range.end.line, 1);
        // 11 source chars, 12 UTF-16 units — the emoji counts twice.
        assert_eq!(diag.range.end.character, 12);
    }

    #[test]
    fn diagnostic_falls_back_to_one_char_without_line_info() {
        let err = ValidationError {
            message: "x".into(),
            severity: ErrorSeverity::Warning,
            line: 5,
            col: 2,
            file: "f".into(),
            code: None,
            fix: None,
            end: None,
            related: Vec::new(),
        };
        let diag = validation_error_to_diagnostic(&err, &DocLines::none());
        assert_eq!(diag.range.start.character, 2);
        assert_eq!(diag.range.end.character, 3);
    }

    #[test]
    fn diagnostic_without_line_info_ignores_a_carried_end() {
        let err = ValidationError {
            message: "x".into(),
            severity: ErrorSeverity::Warning,
            line: 5,
            col: 2,
            file: "f".into(),
            code: Some("CW500"),
            fix: None,
            end: Some((6, 0)),
            related: Vec::new(),
        };
        let diag = validation_error_to_diagnostic(&err, &DocLines::none());
        assert_eq!(diag.range.start.line, 4);
        assert_eq!(
            diag.range.end.line, 4,
            "must not spill onto a later line without text to clamp against"
        );
        assert_eq!(diag.range.end.character, 3);
    }

    #[test]
    fn suppression_matches_codes_case_insensitively() {
        let ignored = vec!["cw100".to_string()];
        assert!(code_is_suppressed(
            Some(&NumberOrString::String("CW100".into())),
            &ignored
        ));
        assert!(code_is_suppressed(
            Some(&NumberOrString::String("cw100".into())),
            &ignored
        ));
        assert!(!code_is_suppressed(
            Some(&NumberOrString::String("CW246".into())),
            &ignored
        ));
        assert!(!code_is_suppressed(None, &ignored));
        assert!(!code_is_suppressed(
            Some(&NumberOrString::Number(100)),
            &ignored
        ));
        assert!(!code_is_suppressed(
            Some(&NumberOrString::String("CW100".into())),
            &[]
        ));
    }

    #[test]
    fn coded_diagnostics_link_to_their_documentation_row() {
        let err = ValidationError {
            message: "x".into(),
            severity: ErrorSeverity::Warning,
            line: 1,
            col: 0,
            file: "f".into(),
            code: Some("CW113"),
            fix: None,
            end: None,
            related: Vec::new(),
        };
        let diag = validation_error_to_diagnostic(&err, &DocLines::none());
        let href = diag
            .code_description
            .expect("a CW code carries a doc link")
            .href;
        assert_eq!(href.fragment(), Some("cw113"));
        assert!(href.as_str().ends_with("ERROR_CODES.md#cw113"));
    }

    #[test]
    fn uncoded_diagnostics_have_no_doc_link_or_tag() {
        let diag = parse_error_to_diagnostic(
            &cwtools_parser::ast::ParseError::Pos(1, 0, "bad".into()),
            &DocLines::none(),
        );
        assert!(diag.code_description.is_none());
        assert!(diag.tags.is_none());
    }

    #[test]
    fn deprecated_and_unnecessary_codes_are_tagged() {
        let tags = |id: &str| code_tags(&NumberOrString::String(id.into()));
        assert_eq!(tags("CW236"), Some(vec![DiagnosticTag::DEPRECATED]));
        assert_eq!(tags("cw121"), Some(vec![DiagnosticTag::UNNECESSARY]));
        assert_eq!(tags("CW239"), Some(vec![DiagnosticTag::UNNECESSARY]));
        assert_eq!(tags("CW231"), Some(vec![DiagnosticTag::UNNECESSARY]));
        assert_eq!(tags("CW240"), None);
        assert_eq!(code_tags(&NumberOrString::Number(121)), None);
    }

    #[test]
    fn related_spans_publish_against_the_same_document() {
        let text = "foo = {\n    else_if = { a = 1 }\n}\n";
        let err = ValidationError {
            message: "x".into(),
            severity: ErrorSeverity::Error,
            line: 1,
            col: 0,
            file: "file:///ws/events/test.txt".into(),
            code: Some("CW238"),
            fix: None,
            end: None,
            related: vec![cwtools_validation::RelatedSpan {
                message: "this else_if has no preceding if".into(),
                line: 2,
                col: 4,
                end: (2, 11),
            }],
        };
        let diag = validation_error_to_diagnostic(&err, &utf16_lines(text));
        let related = diag.related_information.expect("related span published");
        assert_eq!(related.len(), 1);
        assert_eq!(
            related[0].location.uri.as_str(),
            "file:///ws/events/test.txt"
        );
        assert_eq!(related[0].location.range.start.line, 1);
        assert_eq!(related[0].location.range.start.character, 4);
        assert_eq!(related[0].location.range.end.character, 11);
        assert_eq!(related[0].message, "this else_if has no preceding if");
    }

    #[test]
    fn related_spans_are_dropped_when_the_file_is_not_a_uri() {
        let err = ValidationError {
            message: "x".into(),
            severity: ErrorSeverity::Error,
            line: 1,
            col: 0,
            file: "config/rules.cwt".into(),
            code: Some("CW238"),
            fix: None,
            end: None,
            related: vec![cwtools_validation::RelatedSpan {
                message: "elsewhere".into(),
                line: 1,
                col: 0,
                end: (1, 4),
            }],
        };
        let diag = validation_error_to_diagnostic(&err, &DocLines::none());
        assert!(diag.related_information.is_none());
    }
}

#[cfg(test)]
mod ignored_tests {
    use super::*;
    use std::sync::Arc;

    use crate::state::{DocumentState, ParsedDoc};
    use cwtools_parser::parser::parse_string;
    use cwtools_string_table::string_table::StringTable;
    use futures_util::stream::StreamExt;
    use parking_lot::Mutex;
    use tower_lsp::{LanguageServer, LspService};

    fn backend_with_ignore(patterns: Vec<String>, workspace_uri: Option<&str>) -> Backend {
        let state = Arc::new(DocumentState::new());
        {
            let mut cfg = state.config.write();
            cfg.ignore_file_patterns = patterns;
            if let Some(ws) = workspace_uri {
                cfg.workspace_prefix = Some(crate::paths::workspace_prefix_of(ws));
                if let Some(path) = crate::access::file_uri_to_path(ws) {
                    if let Ok(canonical) = std::fs::canonicalize(&path) {
                        cfg.workspace_roots = vec![canonical.clone()];
                        cfg.refresh_roots();
                    } else {
                        cfg.workspace_roots = vec![path];
                        cfg.refresh_roots();
                    }
                }
            }
        }
        let captured = Arc::new(Mutex::new(None));
        let slot = captured.clone();
        let st = state.clone();
        let (_svc, _sock) = LspService::new(move |client| {
            *slot.lock() = Some(client.clone());
            Backend {
                client,
                state: st.clone(),
            }
        });
        let client = captured.lock().take().expect("client");
        Backend { client, state }
    }

    fn backend_with_socket() -> (Backend, tower_lsp::ClientSocket) {
        let state = Arc::new(DocumentState::new());
        let captured = Arc::new(Mutex::new(None));
        let slot = captured.clone();
        let st = state.clone();
        let (_svc, socket) = LspService::new(move |client| {
            *slot.lock() = Some(client.clone());
            Backend {
                client,
                state: st.clone(),
            }
        });
        let client = captured.lock().take().expect("client");
        (Backend { client, state }, socket)
    }

    fn backend_with_ignore_and_dirs(
        file_patterns: Vec<String>,
        dir_patterns: Vec<String>,
        workspace_uri: Option<&str>,
    ) -> Backend {
        let backend = backend_with_ignore(file_patterns, workspace_uri);
        {
            let mut cfg = backend.state.config.write();
            cfg.ignore_dir_patterns = dir_patterns;
        }
        backend
    }

    #[tokio::test]
    async fn revalidate_other_open_loc_files_drops_stale_snapshot() {
        let (backend, mut socket) = backend_with_socket();
        let uri = if cfg!(windows) {
            "file:///C:/ws/localisation/target_l_english.yml".to_string()
        } else {
            "file:///ws/localisation/target_l_english.yml".to_string()
        };
        backend
            .state
            .documents
            .lock()
            .open(
                uri.clone(),
                crate::state::ParsedDoc {
                    version: 1,
                    text: Arc::from("l_english:\n KEY:0 \"$changed_key$\"\n"),
                    ast: None,
                    ast_version: None,
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .unwrap();
        let changed_uri = uri.clone();
        super::loc_sweep_test_hook::set(
            &backend,
            Box::new(move |backend, _| {
                backend
                    .state
                    .documents
                    .lock()
                    .change(&changed_uri, 2, Arc::from("l_english:\n KEY:0 changed\n"))
                    .expect("target remains open");
            }),
        );

        backend
            .revalidate_other_open_loc_files(
                "",
                &HashSet::from(["changed_key".to_string()]),
                &HashSet::new(),
                &HashSet::new(),
            )
            .await;

        assert_eq!(
            backend
                .state
                .documents
                .lock()
                .get(&uri)
                .map(|document| document.version),
            Some(2)
        );
        let published =
            tokio::time::timeout(std::time::Duration::from_millis(50), socket.next()).await;
        assert!(
            published.is_err(),
            "stale diagnostics must not publish after the target changes"
        );
    }

    #[test]
    fn is_ignored_uri_respects_engine_baseline() {
        let backend = backend_with_ignore(vec![], Some("file:///ws"));
        assert!(backend.is_ignored_uri("file:///ws/README.txt"));
        assert!(backend.is_ignored_uri("file:///ws/Changelog.txt"));
        assert!(backend.is_ignored_uri("file:///ws/docs/readme.md"));
        assert!(backend.is_ignored_uri("file:///ws/sub/notes.md"));
        assert!(!backend.is_ignored_uri("file:///ws/common/ideas/foo.txt"));
        assert!(!backend.is_ignored_uri("file:///ws/events/kept.txt"));
    }

    #[test]
    fn is_ignored_uri_bare_name_matches_any_depth() {
        let backend = backend_with_ignore(vec!["ignored.txt".into()], Some("file:///ws"));
        assert!(backend.is_ignored_uri("file:///ws/ignored.txt"));
        assert!(backend.is_ignored_uri("file:///ws/common/ignored.txt"));
        assert!(backend.is_ignored_uri("file:///ws/a/b/c/ignored.txt"));
        assert!(!backend.is_ignored_uri("file:///ws/kept.txt"));
    }

    #[test]
    fn is_ignored_uri_path_glob_matches_location() {
        let backend = backend_with_ignore(vec!["**/skip.txt".into()], Some("file:///ws"));
        assert!(backend.is_ignored_uri("file:///ws/skip.txt"));
        assert!(backend.is_ignored_uri("file:///ws/common/skip.txt"));
        assert!(!backend.is_ignored_uri("file:///ws/common/keep.txt"));

        let backend2 = backend_with_ignore(vec!["common/**/skip.txt".into()], Some("file:///ws"));
        assert!(!backend2.is_ignored_uri("file:///ws/skip.txt"));
        assert!(backend2.is_ignored_uri("file:///ws/common/units/skip.txt"));
    }

    #[test]
    fn is_ignored_uri_handles_percent_encoding_and_no_workspace() {
        let backend = backend_with_ignore(vec!["ignored.txt".into()], None);
        assert!(backend.is_ignored_uri("file:///tmp/ignored.txt"));
        let backend2 = backend_with_ignore(vec![], Some("file:///tmp/My%20Mod"));
        assert!(backend2.is_ignored_uri("file:///tmp/My%20Mod/README.txt"));
        assert!(backend2.is_ignored_uri("file:///tmp/My%20Mod/sub/README.txt"));
    }

    #[test]
    fn is_ignored_uri_respects_dir_globs() {
        let backend = backend_with_ignore_and_dirs(
            vec![],
            vec!["scratch".into(), "**/temp".into()],
            Some("file:///ws"),
        );
        assert!(backend.is_ignored_uri("file:///ws/scratch/foo.txt"));
        assert!(backend.is_ignored_uri("file:///ws/common/temp/foo.txt"));
        assert!(!backend.is_ignored_uri("file:///ws/common/keep/foo.txt"));

        let backend2 = backend_with_ignore_and_dirs(
            vec![],
            vec!["common/scratch/**".into()],
            Some("file:///ws"),
        );
        assert!(!backend2.is_ignored_uri("file:///ws/scratch/foo.txt"));
        assert!(backend2.is_ignored_uri("file:///ws/common/scratch/foo.txt"));
    }

    #[test]
    fn clear_ignored_file_state_removes_all_indexes() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let ws_uri = tower_lsp::lsp_types::Url::from_file_path(tmp.path())
            .unwrap()
            .to_string();
        let backend = backend_with_ignore(vec![], Some(&ws_uri));
        {
            let mut cfg = backend.state.config.write();
            let canon = std::fs::canonicalize(tmp.path()).unwrap();
            cfg.workspace_roots = vec![canon.clone()];
            cfg.refresh_roots();
            cfg.workspace_prefix = Some(crate::paths::workspace_prefix_of(&ws_uri));
        }
        let file_path = tmp.path().join("common/foo.txt");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, "").unwrap();
        let uri = tower_lsp::lsp_types::Url::from_file_path(&file_path)
            .unwrap()
            .to_string();
        let table = StringTable::new();
        let parsed = parse_string("my_type = { }", &table);
        {
            let mut info = backend.state.info_service.write();
            Arc::make_mut(&mut info.type_index)
                .file_index
                .insert("common/foo.txt");
            assert!(info.type_index.file_index.contains("common/foo.txt"));
        }
        backend
            .state
            .doc_tokens
            .write()
            .insert(uri.clone(), crate::validate::collect_doc_tokens(&parsed));
        let mut uses = cwtools_validation::references::UsedInstances::default();
        uses.mark("my_type", "foo");
        backend.state.type_uses.write().insert(uri.clone(), uses);
        backend
            .state
            .loc_live_overlay
            .write()
            .insert(uri.clone(), ["loc_key".to_string()].into_iter().collect());
        backend
            .state
            .watched_signatures
            .lock()
            .insert(uri.clone(), (123, 456));
        backend.index_parsed_file(&uri, &parsed, None);
        let rev_before = backend
            .state
            .type_uses_revision
            .load(std::sync::atomic::Ordering::Acquire);
        backend.clear_ignored_file_state(&uri);

        assert_eq!(
            backend.state.info_service.read().export_fingerprint(&uri),
            0,
            "type index must be cleared"
        );
        assert!(!backend.state.doc_tokens.read().contains_key(uri.as_str()));
        assert!(!backend.state.type_uses.read().contains_key(uri.as_str()));
        assert!(
            !backend
                .state
                .loc_live_overlay
                .read()
                .contains_key(uri.as_str())
        );
        assert!(
            !backend
                .state
                .watched_signatures
                .lock()
                .contains_key(uri.as_str())
        );
        assert!(
            !backend
                .state
                .info_service
                .read()
                .type_index
                .file_index
                .contains("common/foo.txt"),
            "file_index entry must be removed"
        );
        assert!(
            backend.state.pending_changed_names.lock().contains("foo"),
            "clear must queue dropped type uses for dependent sweep"
        );
        assert!(
            backend
                .state
                .type_uses_revision
                .load(std::sync::atomic::Ordering::Acquire)
                > rev_before,
            "type_uses_revision must bump"
        );
    }

    #[tokio::test]
    async fn did_close_ignored_clears_index_state() {
        let backend = backend_with_ignore(vec!["ignored.txt".into()], Some("file:///ws"));
        let uri = Url::parse("file:///ws/ignored.txt").unwrap();
        let uri_string = uri.to_string();
        backend
            .state
            .documents
            .lock()
            .open(
                uri_string.clone(),
                ParsedDoc {
                    version: 1,
                    text: Arc::from("my_type = { }"),
                    ast: None,
                    ast_version: None,
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .unwrap();
        backend
            .state
            .watched_signatures
            .lock()
            .insert(uri_string.clone(), (123, 456));

        backend
            .did_close(tower_lsp::lsp_types::DidCloseTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri },
            })
            .await;

        assert!(!backend.state.documents.lock().contains_key(&uri_string));
        assert!(
            !backend
                .state
                .watched_signatures
                .lock()
                .contains_key(&uri_string)
        );
    }

    #[tokio::test]
    async fn parse_and_validate_ignored_returns_empty_and_clears_index() {
        let backend = backend_with_ignore(vec!["ignored.txt".into()], Some("file:///ws"));
        let uri = "file:///ws/ignored.txt";
        let text = "my_type = { broken }";
        let table = StringTable::new();
        let seed = parse_string("my_type = { }", &table);
        backend.index_parsed_file("file:///ws/common/keep.txt", &seed, None);
        backend.index_parsed_file(uri, &seed, None);
        {
            let mut info = backend.state.info_service.write();
            info.clear_file(uri);
            drop(info);
            backend.index_parsed_file(uri, &seed, None);
        }
        let (diags, ast) = backend
            .parse_and_validate(uri, text, crate::ValidateTrigger::DidOpen, None)
            .await;
        assert!(diags.is_empty(), "ignored file must produce no diagnostics");
        assert!(ast.is_none(), "ignored file must not return an AST");
        assert_eq!(
            backend.state.info_service.read().export_fingerprint(uri),
            0,
            "ignored file must be cleared from the type index"
        );
        assert!(!backend.state.doc_tokens.read().contains_key(uri));
    }

    #[tokio::test]
    async fn index_parsed_file_defensively_skips_ignored() {
        let backend = backend_with_ignore(vec!["README.txt".into()], Some("file:///ws"));
        let uri = "file:///ws/README.txt";
        let parsed = parse_string("x = 1", &StringTable::new());
        backend.index_parsed_file(uri, &parsed, None);
        assert_eq!(
            backend.state.info_service.read().export_fingerprint(uri),
            0,
            "engine-baseline ignored file must never enter the index"
        );
    }

    #[tokio::test]
    async fn validate_parsed_prebuilt_returns_empty_for_ignored() {
        let backend = backend_with_ignore(vec!["ignored.txt".into()], Some("file:///ws"));
        let uri = "file:///ws/ignored.txt";
        let parsed = parse_string("x = 1", &StringTable::new());
        let lines = DocLines::new("x = 1", tower_lsp::lsp_types::PositionEncodingKind::UTF16);
        let diags = backend.validate_parsed_prebuilt(
            uri,
            &parsed,
            &std::collections::HashSet::new(),
            &cwtools_rules::rules_types::RuleSet::default(),
            None,
            None,
            &lines,
        );
        assert!(
            diags.is_empty(),
            "prebuilt validation of ignored file must be empty"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn did_open_ignored_is_not_indexed() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let ws_uri = Url::from_file_path(tmp.path()).unwrap().to_string();
        let backend = backend_with_ignore(vec!["ignored.txt".into()], Some(&ws_uri));
        let file_path = tmp.path().join("ignored.txt");
        std::fs::write(&file_path, "").unwrap();
        let uri = Url::from_file_path(&file_path).unwrap();
        backend
            .did_open(tower_lsp::lsp_types::DidOpenTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "paradox".into(),
                    version: 1,
                    text: "my_type = { }\n".into(),
                },
            })
            .await;
        assert!(backend.state.documents.lock().contains_key(uri.as_str()));
        assert_eq!(
            backend
                .state
                .info_service
                .read()
                .export_fingerprint(uri.as_str()),
            0
        );
        assert!(!backend.state.doc_tokens.read().contains_key(uri.as_str()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn did_change_ignored_clears_index_and_tokens() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let ws_uri = Url::from_file_path(tmp.path()).unwrap().to_string();
        let backend = backend_with_ignore(vec!["ignored.txt".into()], Some(&ws_uri));
        let file_path = tmp.path().join("kept.txt");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, "").unwrap();
        let uri = Url::from_file_path(&file_path).unwrap();
        backend
            .did_open(tower_lsp::lsp_types::DidOpenTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "paradox".into(),
                    version: 1,
                    text: "kept = { }\n".into(),
                },
            })
            .await;
        {
            let mut cfg = backend.state.config.write();
            cfg.ignore_file_patterns = vec!["kept.txt".into()];
        }
        backend
            .did_change(tower_lsp::lsp_types::DidChangeTextDocumentParams {
                text_document: tower_lsp::lsp_types::VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version: 2,
                },
                content_changes: vec![tower_lsp::lsp_types::TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "kept = { changed }\n".into(),
                }],
            })
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(
            backend
                .state
                .info_service
                .read()
                .export_fingerprint(uri.as_str()),
            0
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revalidate_all_open_docs_ignored_is_cleared_and_kept_stays() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let ws_uri = Url::from_file_path(tmp.path()).unwrap().to_string();
        let backend = backend_with_ignore(vec![], Some(&ws_uri));
        let ignored_path = tmp.path().join("ignored.txt");
        let kept_path = tmp.path().join("kept.txt");
        std::fs::write(&ignored_path, "").unwrap();
        std::fs::write(&kept_path, "").unwrap();
        let ignored_uri = Url::from_file_path(&ignored_path).unwrap().to_string();
        let kept_uri = Url::from_file_path(&kept_path).unwrap().to_string();
        {
            let mut docs = backend.state.documents.lock();
            docs.open(
                ignored_uri.clone(),
                crate::state::ParsedDoc {
                    version: 1,
                    text: Arc::from("ignored = { }"),
                    ast: None,
                    ast_version: None,
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .unwrap();
            docs.open(
                kept_uri.clone(),
                crate::state::ParsedDoc {
                    version: 1,
                    text: Arc::from("kept = { }"),
                    ast: None,
                    ast_version: None,
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .unwrap();
        }
        let table = StringTable::new();
        let parsed = parse_string("x = 1", &table);
        backend
            .state
            .doc_tokens
            .write()
            .insert(ignored_uri.clone(), collect_doc_tokens(&parsed));
        backend
            .state
            .doc_tokens
            .write()
            .insert(kept_uri.clone(), collect_doc_tokens(&parsed));
        {
            let mut info = backend.state.info_service.write();
            let type_index = Arc::make_mut(&mut info.type_index);
            type_index.file_index.insert("ignored.txt");
            type_index.file_index.insert("kept.txt");
        }
        {
            let mut cfg = backend.state.config.write();
            cfg.ignore_file_patterns = vec!["ignored.txt".into()];
        }
        backend
            .revalidate_all_open_docs(crate::state::ValidateTrigger::ConfigChange)
            .await;
        assert!(
            !backend
                .state
                .doc_tokens
                .read()
                .contains_key(ignored_uri.as_str()),
            "ignored open doc must be cleared from doc_tokens"
        );
        assert!(
            backend
                .state
                .doc_tokens
                .read()
                .contains_key(kept_uri.as_str()),
            "kept doc must keep its tokens"
        );
        assert!(
            !backend
                .state
                .info_service
                .read()
                .type_index
                .file_index
                .contains("ignored.txt"),
            "ignored file_index entry must be removed"
        );
        assert!(
            backend
                .state
                .info_service
                .read()
                .type_index
                .file_index
                .contains("kept.txt"),
            "kept file_index entry must stay"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revalidate_open_dependents_skips_ignored() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let ws_uri = Url::from_file_path(tmp.path()).unwrap().to_string();
        let backend = backend_with_ignore(vec!["ignored.txt".into()], Some(&ws_uri));
        let kept_path = tmp.path().join("kept.txt");
        let ignored_path = tmp.path().join("ignored.txt");
        std::fs::write(&kept_path, "").unwrap();
        std::fs::write(&ignored_path, "").unwrap();
        let kept_uri = Url::from_file_path(&kept_path).unwrap().to_string();
        let ignored_uri = Url::from_file_path(&ignored_path).unwrap().to_string();
        let table = StringTable::new();
        let kept_parsed = Arc::new(parse_string("kept = { }", &table));
        let ignored_parsed = Arc::new(parse_string("ignored = { }", &table));
        {
            let mut docs = backend.state.documents.lock();
            docs.open(
                kept_uri.clone(),
                crate::state::ParsedDoc {
                    version: 1,
                    text: Arc::from("kept"),
                    ast: Some(kept_parsed.clone()),
                    ast_version: Some(1),
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .unwrap();
            docs.open(
                ignored_uri.clone(),
                crate::state::ParsedDoc {
                    version: 1,
                    text: Arc::from("ignored"),
                    ast: Some(ignored_parsed.clone()),
                    ast_version: Some(1),
                    ast_source_bytes: 0,
                    loc_cache: None,
                },
            )
            .unwrap();
        }
        backend
            .state
            .doc_tokens
            .write()
            .insert(kept_uri.clone(), collect_doc_tokens(&kept_parsed));
        backend
            .state
            .doc_tokens
            .write()
            .insert(ignored_uri.clone(), collect_doc_tokens(&ignored_parsed));
        let mut changed = std::collections::HashSet::new();
        let token = ignored_parsed
            .arena
            .leaves
            .first()
            .and_then(|l| table.get_string(l.key.lower))
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_else(|| "ignored".into());
        changed.insert(token);
        backend
            .revalidate_open_dependents(&kept_uri, 1, Some(&changed))
            .await;
        assert!(!backend.is_ignored_uri(&kept_uri));
        assert!(backend.is_ignored_uri(&ignored_uri));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn watched_batch_ignored_not_inserted_into_file_index() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let ws_uri = Url::from_file_path(tmp.path()).unwrap().to_string();
        let backend = backend_with_ignore(vec!["ignored.txt".into()], Some(&ws_uri));
        let ignored_path = tmp.path().join("ignored.txt");
        let kept_path = tmp.path().join("kept.txt");
        std::fs::write(&ignored_path, "ignored = { }").unwrap();
        std::fs::write(&kept_path, "kept = { }").unwrap();
        let ignored_uri = Url::from_file_path(&ignored_path).unwrap().to_string();
        let kept_uri = Url::from_file_path(&kept_path).unwrap().to_string();
        {
            let mut info = backend.state.info_service.write();
            Arc::make_mut(&mut info.type_index)
                .file_index
                .insert("dummy.txt");
        }
        let mut changes = std::collections::HashSet::new();
        changes.insert(ignored_uri.clone());
        changes.insert(kept_uri.clone());
        backend.process_watched_batch(changes, vec![]).await;
        assert!(
            !backend
                .state
                .info_service
                .read()
                .type_index
                .file_index
                .contains("ignored.txt"),
            "watched batch must not index ignored file"
        );
        assert!(
            backend
                .state
                .info_service
                .read()
                .type_index
                .file_index
                .contains("kept.txt"),
            "watched batch must index kept file"
        );
        assert!(
            !backend
                .state
                .watched_signatures
                .lock()
                .contains_key(ignored_uri.as_str()),
            "ignored watched file must not retain signature"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn watched_batch_ignored_dir_not_inserted_into_file_index() {
        let tmp = tempfile::TempDir::new().expect("tmpdir");
        let ws_uri = Url::from_file_path(tmp.path()).unwrap().to_string();
        let backend = backend_with_ignore_and_dirs(vec![], vec!["scratch".into()], Some(&ws_uri));
        let ignored_path = tmp.path().join("scratch/ignored.txt");
        let kept_path = tmp.path().join("kept.txt");
        std::fs::create_dir_all(ignored_path.parent().unwrap()).unwrap();
        std::fs::write(&ignored_path, "ignored = { }").unwrap();
        std::fs::write(&kept_path, "kept = { }").unwrap();
        let ignored_uri = Url::from_file_path(&ignored_path).unwrap().to_string();
        let kept_uri = Url::from_file_path(&kept_path).unwrap().to_string();
        {
            let mut info = backend.state.info_service.write();
            Arc::make_mut(&mut info.type_index)
                .file_index
                .insert("dummy.txt");
        }
        let mut changes = std::collections::HashSet::new();
        changes.insert(ignored_uri.clone());
        changes.insert(kept_uri.clone());
        backend.process_watched_batch(changes, vec![]).await;
        assert!(
            !backend
                .state
                .info_service
                .read()
                .type_index
                .file_index
                .contains("scratch/ignored.txt"),
            "watched batch must not index file under ignored directory"
        );
        assert!(
            backend
                .state
                .info_service
                .read()
                .type_index
                .file_index
                .contains("kept.txt"),
            "watched batch must index kept file"
        );
        assert!(
            !backend
                .state
                .watched_signatures
                .lock()
                .contains_key(ignored_uri.as_str()),
            "ignored watched file must not retain signature"
        );
    }
}
