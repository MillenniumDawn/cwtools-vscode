use std::collections::HashSet;
use std::sync::Arc;

use tower_lsp::lsp_types::*;

use cwtools_parser::ast::{ParseError, ParsedFile};
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_types::RuleSet;
use cwtools_string_table::string_table::StringId;
use cwtools_validation::references::{UsedInstances, check_unused_instances, needs_use_tracking};
use cwtools_validation::{
    Prepared, ValidationError, validate_prepared, validate_prepared_tracking_uses,
};

use crate::paths::{
    encoded_position_len, logical_path_from_uri, source_column_to_lsp, uri_to_path_str,
};
use crate::{Backend, LocTextMap};

#[allow(clippy::too_many_arguments)]
pub(crate) fn make_prepared<'a>(
    ruleset: &'a cwtools_rules::rules_types::RuleSet,
    table: &'a cwtools_string_table::string_table::StringTable,
    game: Option<cwtools_game::constants::Game>,
    type_index: &'a cwtools_info::TypeIndex,
    modifier_keys: &'a std::collections::HashSet<String>,
    loc_index: Option<&'a cwtools_localization::LocIndex>,
    extra_loc_keys: Option<&'a std::collections::HashSet<String>>,
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
        // The editor keeps no inline-script registry yet (#256), so a call site
        // is accepted as written instead of being checked against a body the
        // server hasn't loaded.
        inline_scripts: None,
        registry,
        scope_checks,
        var_checks,
    }
}

/// Per-file diagnostic cap. Beyond this, a file's errors are truncated with a
/// summary marker so one broken file can't flood the editor.
pub(crate) const MAX_FILE_ERRORS: usize = 100;

/// Convert a loc-file diagnostic into a `ValidationError` so it shares the
/// `validation_error_to_diagnostic` rendering path. Loc positions are 1-based;
/// `ValidationError.col` is 0-based (used directly by the renderer).
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
        // Carry the loc fix (CW268 quote-wrap) so it reaches the code-action
        // path through the shared `validation_error_to_diagnostic` renderer.
        fix: d.fix.clone(),
        // Loc diagnostics expose only a single (line, col) point, so they keep the
        // whole-line squiggle (no cheap end to derive — see task-18 loc decision).
        end: None,
        related: Vec::new(),
    }
}

/// Parse one loc buffer into the `LocFile`s every consumer of an edit shares:
/// the live-overlay key set, the diagnostics, and the hover text. Empty when the
/// text doesn't parse as loc at all (#87).
fn parse_loc_buffer(text: &str, path: &str) -> Vec<cwtools_localization::LocFile> {
    cwtools_localization::parse_loc_files(path, text, None).unwrap_or_default()
}

/// Lowercased loc keys defined in a single loc file's text. A cheap single-file
/// parse used to keep the live overlay current on edit (#36).
fn loc_keys_of(text: &str, path: &str) -> HashSet<String> {
    loc_keys_from(&parse_loc_buffer(text, path))
}

/// [`loc_keys_of`] over an already-parsed buffer.
fn loc_keys_from(files: &[cwtools_localization::LocFile]) -> HashSet<String> {
    let mut keys = HashSet::new();
    for file in files {
        for entry in &file.entries {
            keys.insert(entry.key.to_lowercase());
        }
    }
    keys
}

/// Names whose dependents a loc-key change may affect: the changed keys
/// themselves (literal loc references, CW122) plus, for every name-derived
/// `localisation = { … }` rule in the ruleset, the definition name a changed
/// key would be derived from (`prefix$suffix` stripped — CW100 flags
/// `<name>_desc`-style keys, and a game file's token set contains `<name>`,
/// not the derived key). Everything is compared lowercased, matching both the
/// loc index and the doc token sets.
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

/// Cap a file's validation errors at [`MAX_FILE_ERRORS`], appending a summary
/// marker for the remainder. Returns the pre-truncation total (for logging).
/// Shared by the batch and single-file paths so the cap stays consistent.
///
/// CW277 is held out of the cap. It is emitted last (validation stopped, so the
/// rest of the file was never looked at) and says the diagnostics that ARE here
/// are incomplete — dropping it with the tail turns a truncated file into one
/// that merely flooded.
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

/// Names a loc `$ref$` may resolve to besides loc keys: modifier keys plus every
/// loc-bindable index name (type instances, dynamic modifiers, ideas, and
/// defined variables). Mirrors `cwtools_driver::Session::loc_extra_valid_refs`
/// so CI, the workspace scan, and the per-edit path accept the same references —
/// the keystroke path used to take idea instances alone, so a `$my_variable$`
/// reference grew a CW225 the moment its file was opened and lost it again on
/// the next rescan.
pub(crate) fn loc_extra_valid_refs(
    modifier_keys: &HashSet<String>,
    type_index: &cwtools_info::TypeIndex,
) -> HashSet<String> {
    let mut extra = modifier_keys.clone();
    extra.extend(type_index.loc_bindable_names());
    extra
}

/// CW100: objects defined here whose `## required` localisation keys aren't
/// provided by any loc file. Gated on the loc index being built — before the
/// initial scan finishes it's empty and everything would falsely report
/// missing. Shared by the batch/scan path and the single-file keystroke path
/// so both apply the same gate (a prior drift left the keystroke path without
/// this check, so CW100 would flicker off on every edit until the next scan).
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

/// Validate one already-parsed file against a caller-supplied [`Prepared`],
/// returning LSP diagnostics. The prebuilt state is passed in (not re-locked
/// here) so the full-workspace pass can take its read guards once and share the
/// `Prepared` across rayon threads — it is `Copy` and all-borrows, so `Sync`.
///
/// With `track_uses`, the file's `<type>` references to tracked
/// (`should_be_used`) types are recorded and returned alongside the
/// diagnostics; the caller folds them into the workspace-wide store the
/// unused-instance check (CW239/CW231) reads. `false` keeps the plain path.
pub(crate) fn validate_parsed_with_indexes(
    uri: &str,
    parsed: &ParsedFile,
    prepared: &Prepared,
    lines: &DocLines,
    track_uses: bool,
) -> (Vec<Diagnostic>, Option<UsedInstances>) {
    let mut diagnostics: Vec<Diagnostic> = parsed
        .errors
        .iter()
        .map(|e| parse_error_to_diagnostic(e, lines))
        .collect();
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

/// A document's lines plus the negotiated position encoding: everything the
/// diagnostic builders need to place a squiggle. The parser reports 1-based
/// lines and 0-based CHAR columns, but the client reads columns in the encoding
/// negotiated at `initialize`, so every published position goes through
/// [`source_column_to_lsp`] — the same conversion hover, rename, and the
/// code-action fix edits use. Publishing the raw column instead put the
/// diagnostic and its own quick fix on different spans of any line holding a
/// non-BMP character.
///
/// Lines are resolved once per file: `source_position_to_lsp` re-scans the whole
/// text per call, which the keystroke path can't afford at 100 diagnostics.
pub(crate) struct DocLines<'a> {
    lines: Vec<&'a str>,
    encoding: PositionEncodingKind,
}

impl<'a> DocLines<'a> {
    pub(crate) fn new(text: &'a str, encoding: PositionEncodingKind) -> Self {
        Self {
            lines: text.lines().collect(),
            encoding,
        }
    }

    /// No document text in hand (the workspace scan's non-open files, ruleset
    /// load errors): positions keep the parser's raw char column and the
    /// squiggle stays one character wide. The encoding is never consulted
    /// without a line to convert against.
    pub(crate) fn none() -> Self {
        Self {
            lines: Vec::new(),
            encoding: PositionEncodingKind::UTF16,
        }
    }

    /// LSP position for a parser position (0-based `line`, 0-based char `col`).
    fn position(&self, line: u32, col: u32) -> Position {
        let character = self
            .lines
            .get(line as usize)
            .map_or(col, |l| source_column_to_lsp(l, col, &self.encoding));
        Position { line, character }
    }

    /// Whether any document text is held, i.e. whether positions can be resolved
    /// against real lines. False for the workspace scan and the ruleset load.
    fn has_text(&self) -> bool {
        !self.lines.is_empty()
    }

    /// LSP position for a parser range end (0-based `line`, 0-based char `col`),
    /// walked back over whitespace to the last content character.
    ///
    /// The parser records a node's end as the cursor after the node *and* the
    /// whitespace behind it, so a raw end sits on the start of the next token and
    /// published verbatim bleeds onto the following line (#107). Floors at
    /// `start`, so the range cannot invert.
    fn clamped_end_position(&self, line: u32, col: u32, start: Position) -> Position {
        let (mut line, mut col) = (line, col);
        loop {
            let text = self.lines.get(line as usize).copied().unwrap_or("");
            // Chars up to and including the last non-whitespace one in the first
            // `col`, counted in a single pass — this runs per diagnostic, so the
            // old collect-then-trim allocated a String per squiggle.
            let mut content_end = 0;
            for (i, c) in text.chars().take(col as usize).enumerate() {
                if !c.is_whitespace() {
                    content_end = i as u32 + 1;
                }
            }
            if content_end > 0 {
                col = content_end;
                break;
            }
            let Some(prev) = line.checked_sub(1) else {
                col = 0;
                break;
            };
            line = prev;
            col = self
                .lines
                .get(line as usize)
                .map_or(0, |l| l.chars().count() as u32);
        }

        let end = self.position(line, col);
        if (end.line, end.character) < (start.line, start.character) {
            start
        } else {
            end
        }
    }

    /// Range for a secondary span, given the emit site's 1-based line, 0-based
    /// char column and exclusive end. Applies the same whole-line fallback the
    /// primary squiggle uses when there's no document text to walk the end back
    /// through.
    fn related_range(&self, line: u32, col: u16, end: (u32, u16)) -> Range {
        let line = line.saturating_sub(1);
        let start = self.position(line, col as u32);
        let end = if self.has_text() {
            self.clamped_end_position(end.0.saturating_sub(1), end.1 as u32, start)
        } else {
            self.end_position(line, start.character)
        };
        Range { start, end }
    }

    /// End position for a diagnostic whose start is at encoded column `start`:
    /// the end of that line's content, but always at least one past `start` so
    /// the range is never empty. With no line info, a single-character span.
    fn end_position(&self, line: u32, start: u32) -> Position {
        let character = self
            .lines
            .get(line as usize)
            .map_or(0, |l| encoded_position_len(l.trim_end(), &self.encoding))
            .max(start + 1);
        Position { line, character }
    }
}

/// Codes whose diagnostics carry an LSP tag. DEPRECATED strikes the span
/// through, UNNECESSARY fades it: both say "this line has no business being
/// here" at a glance, before the message is read. Kept to the codes that mean
/// exactly that — a tag on a code the reader still has to act on would fade
/// script that works.
const CODE_TAGS: &[(&str, DiagnosticTag)] = &[
    ("CW121", DiagnosticTag::UNNECESSARY),
    ("CW231", DiagnosticTag::UNNECESSARY),
    ("CW236", DiagnosticTag::DEPRECATED),
    ("CW239", DiagnosticTag::UNNECESSARY),
];

/// The tag list for a diagnostic's code, if it has one.
fn code_tags(code: &NumberOrString) -> Option<Vec<DiagnosticTag>> {
    let NumberOrString::String(id) = code else {
        return None;
    };
    CODE_TAGS
        .iter()
        .find(|(c, _)| c.eq_ignore_ascii_case(id))
        .map(|(_, tag)| vec![tag.clone()])
}

/// The error-code reference entry for a diagnostic's code, so the editor can
/// offer "open documentation" on a `CWxxx` the reader hasn't met before.
fn code_description(code: &NumberOrString) -> Option<CodeDescription> {
    let NumberOrString::String(id) = code else {
        return None;
    };
    let href = Url::parse(&cwtools_error_codes::doc_url(id)).ok()?;
    Some(CodeDescription { href })
}

/// Build a whole-statement-line diagnostic at `(line, col)` — 0-based line,
/// 0-based parser char column. The squiggle spans from `col` to the line's
/// content end. Shared skeleton behind the `*_to_diagnostic` builders.
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

/// The LSP severity for an engine severity.
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

/// Convert a `.cwt` rule-config error (parse or structural reference) into an LSP
/// diagnostic. `RuleParseError.line` is 1-based; `col` is a 0-based character.
/// Shared by the load-time path (`config.rs`) and the live per-file CWT lint.
/// A directory-targeted error (an unreadable rules folder) keeps the directory
/// as its file, so it lands in Problems under the folder rather than vanishing.
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

/// Whether a diagnostic carrying `code` should be dropped given the user's
/// lowercased suppression list (`errors.ignore` → `ignoredErrorCodes`). Only the
/// string codes the validator emits (e.g. `CW100`) can be suppressed; compared
/// case-insensitively. Numeric/absent codes are never suppressed.
pub(crate) fn code_is_suppressed(code: Option<&NumberOrString>, ignored: &[String]) -> bool {
    match code {
        Some(NumberOrString::String(c)) => ignored.contains(&c.to_ascii_lowercase()),
        _ => false,
    }
}

/// Drop diagnostics whose code an inline `# cwtools-ignore` directive
/// suppresses on the diagnostic's line or the lines beside it. A no-op when
/// the file carries no directive, which is the common case and keeps this off
/// the keystroke hot path's cost.
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
    // Precise span: when the emit site carried the node's end position, publish
    // the real range instead of diagnostic_at's whole-line squiggle. The end uses
    // the same 1-based-line / 0-based-char convention as the start (and as
    // code_action's fix-edit conversion), so only the whole-line end is overridden.
    // Fix edits are unaffected: they read `SuggestedFix.range`, which still needs
    // the untrimmed span to delete a line cleanly.
    // With `end` absent, the whole-line fallback stands byte-for-byte.
    // Without text the end cannot be walked back off the following token, and
    // publishing it raw bleeds onto the next line, so the whole-line fallback
    // stands instead.
    if let Some((end_line, end_col)) = err.end
        && lines.has_text()
    {
        diag.range.end = lines.clamped_end_position(
            end_line.saturating_sub(1),
            end_col as u32,
            diag.range.start,
        );
    }
    // Secondary spans (the `if` a stray `else` is missing, the case a path is
    // indexed under) are all in the file being validated, so they hang off its
    // own URI. `err.file` is the document URI on this path; a ruleset-load
    // error whose "file" is not a URI simply publishes no related information.
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
    // Carry any machine-applicable fix into `data` so the code-action handler
    // can round-trip it back into a QUICKFIX WorkspaceEdit. Covers both the
    // validation and loc paths (loc diagnostics flow through here too).
    if let Some(fix) = &err.fix {
        diag.data = Some(crate::code_action::fix_to_data(fix));
    }
    diag
}

/// Collect the identifier-like tokens a parsed file mentions — every key and
/// every (quoted or unquoted) string value — as interned `.lower` [`StringId`]s
/// straight from the arena, so no string-table locks or allocations are paid.
/// Used by the dependent sweep to decide which open docs reference a changed
/// export (the sweep interns the changed names once at the comparison site).
/// Deliberately broad (an over-approximation): including a token that isn't
/// really a cross-file reference only costs an extra revalidation, while
/// missing one would silently skip a file that should be revalidated.
pub(crate) fn collect_doc_tokens(ast: &ParsedFile) -> HashSet<StringId> {
    use cwtools_parser::ast::Value;
    // The arena holds every element flatly, so iterating the per-kind vectors
    // covers the whole tree without a recursive walk. `.lower` is the canonical
    // lowercased form, so the resulting set is already case-folded.
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
    // Reserved slot 0 is the empty string; changed names are never empty.
    tokens.remove(&StringId(0));
    tokens
}

impl Backend {
    /// Refresh the per-document token set used to scope the dependent sweep.
    /// `ast = None` (e.g. a file that failed to parse) clears the set, so the
    /// sweep treats the doc as "unknown" and always includes it.
    pub(crate) fn update_doc_tokens(&self, uri: &str, ast: Option<&Arc<ParsedFile>>) {
        // Build the token set BEFORE taking the write lock. collect_doc_tokens
        // walks the whole arena; holding doc_tokens.write() across it blocks the
        // dependent sweep's readers (doc_tokens.read()) for the whole walk.
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

    /// Workspace scan would exclude `uri`.
    pub(crate) fn is_ignored_uri(&self, uri: &str) -> bool {
        let cfg = self.state.config.read();
        let logical = logical_path_from_uri(uri, &cfg.workspace_prefix);
        cwtools_file_manager::file_manager::is_ignored_path(
            &logical,
            &cfg.ignore_file_patterns,
            &cfg.ignore_dir_patterns,
        )
    }

    /// Drop all index state for `uri`.
    pub(crate) fn clear_ignored_file_state(&self, uri: &str) {
        // Compute file_index path before locking info.
        let rel = self.workspace_rel_for_file_index(uri);
        let bump_info = {
            let mut info = self.state.info_service.write();
            let before = info.export_fingerprint(uri);
            info.clear_file(uri);
            if let Some(rel) = rel
                && !info.type_index.file_index.is_empty()
            {
                info.type_index.file_index.remove(&rel);
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
        let loc_removed = self.state.loc_live_overlay.write().remove(uri).is_some()
            || self.state.loc_watched_overlay.write().remove(uri).is_some();
        self.state.watched_signatures.lock().remove(uri);
        if bump_info || loc_removed {
            self.bump_info_revision();
        }
    }

    /// Index an already-parsed AST into the info index, so the workspace scan
    /// can index cache-hit ASTs without re-parsing.
    ///
    /// `parsed_version` is the document version `parsed` came from, when it came
    /// from an open document. Callers indexing from disk (the workspace scan,
    /// the did_close restore) pass `None`.
    ///
    /// The info revision is bumped only when the file's export fingerprint
    /// moved, the same condition `debounced_validate` gates the dependent sweep
    /// on. See the comment at the bump for why that covers every consumer.
    #[tracing::instrument(skip_all, fields(uri = %uri))]
    pub(crate) fn index_parsed_file(
        &self,
        uri: &str,
        parsed: &ParsedFile,
        parsed_version: Option<i32>,
    ) {
        // Ignored files must not enter the index.
        if self.is_ignored_uri(uri) {
            self.clear_ignored_file_state(uri);
            return;
        }
        let ws_prefix = self.state.config.read().workspace_prefix.clone();
        let logical_path = logical_path_from_uri(uri, &ws_prefix);
        // Snapshot the ruleset instead of holding `rules` across the write: the
        // guard would otherwise outrank the `documents` check below, and the
        // `Arc` makes the snapshot free.
        let ruleset = self.state.rules.read().ruleset.clone();
        // Base instances and subtype-qualified membership share one walk before
        // the write guard. Completion takes a read guard on the same lock, so
        // every microsecond kept outside it is a keystroke the UI path avoids.
        let collected = ruleset.as_ref().map(|ruleset| {
            cwtools_info::collect_type_instances_with_subtypes(
                ruleset,
                parsed,
                &logical_path,
                &self.state.string_table,
                cwtools_validation::subtype_membership_for_instance,
            )
        });
        // Stale-write guard, the same version check debounced_validate publishes
        // behind: a newer edit's validate may have raced ahead and indexed this
        // file already, and installing the older parse would silently roll the
        // index back to it until the next keystroke.
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
        // Both caches keyed on the info revision are built from exactly the
        // symbols this fingerprint covers: `fallback_cache` from
        // `variable_counts` + `event_target_counts`, `loc_ref_names_cache` from
        // the type index's instance names + `var_index` (plus the ruleset's
        // modifier keys, which this path never touches). An edit inside a rule
        // body leaves all of them identical, so bumping unconditionally threw
        // both away on every keystroke and paid a full rebuild for nothing.
        if exports_changed {
            self.bump_info_revision();
        }
    }

    /// Validate an already-parsed document against the (already-built) workspace
    /// index, with the ruleset already locked and the per-run scope registry
    /// prebuilt by the caller. Multi-file callers (the workspace scan, the
    /// dependent sweep) build those ONCE outside their loop and reuse them.
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
        // Overlay computed before the other guards (its lock is independent and
        // never nested inside info/loc — see validate_loc_text).
        let overlay = self.loc_overlay_keys();
        let info_guard = self.state.info_service.read();
        let loc_guard = self.state.loc_index.read();
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

    /// Replace `uri`'s recorded `<type>` uses and return the instance names
    /// whose "is it used?" answer may have changed (empty when nothing moved).
    /// The merge revision is only bumped on a real change, so the common
    /// keystroke — uses identical to last time — keeps [`Self::merged_type_uses`]
    /// hitting its cache.
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

    /// Re-record `uri`'s `<type>` uses from an AST the caller holds, queueing the
    /// names whose used-status moved for the dependent sweep. For a path that
    /// replaces a file's content without validating it (`did_close` restoring the
    /// disk AST over a discarded buffer), where the stored entry would otherwise
    /// keep describing text that never reached disk (#133).
    ///
    /// The diagnostics this pass produces are dropped: the file isn't open, so
    /// nothing publishes them, and only the uses are wanted. The unsaved-loc-key
    /// overlay is left out for the same reason. It steers the loc checks alone,
    /// not which instances a file references.
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
            rules_guard.scope_registry.as_ref(),
            scope_checks,
            var_checks,
        );
        let (_, used) = validate_prepared_tracking_uses(parsed, uri, &prepared);
        drop(loc_guard);
        drop(info_guard);
        drop(rules_guard);
        let changed = self.refresh_type_uses(uri, used);
        if !changed.is_empty() {
            self.state.pending_changed_names.lock().extend(changed);
        }
    }

    /// The union of every file's recorded uses — what the batch driver folds
    /// per run, kept as a cache keyed on `type_uses_revision` here because the
    /// LSP needs it per validated file rather than once.
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

    /// Fold one file's freshly-recorded uses into the store, queue the names
    /// whose used-status changed for the dependent sweep, and return the
    /// CW239/CW231 errors for the definitions in `uri` nothing in the
    /// workspace references.
    ///
    /// Gated on `index_ready`: before the first scan the store only covers the
    /// files validated so far, so "nothing uses this" would be answered from a
    /// fragment and flag almost everything. The scan itself runs the check
    /// against its own complete pass instead (see `validate_entire_workspace`).
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

    /// Publish diagnostics after dropping any whose code the user suppressed via
    /// `errors.ignore` (`ignoredErrorCodes`). Every publish path funnels through
    /// here so a suppressed code can't slip out from whichever validation route
    /// produced it. A no-op (and off the hot path's cost) when nothing is
    /// suppressed, which is the common case.
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
        // Keep the `fixAllWorkspace` store in lockstep with what's about to be
        // published, so a snapshot of it always matches the Problems panel.
        // Open documents are guarded by their version; closed-file entries use
        // the source hash captured by the validation path.
        // CW100's create-key fix has no span edit (`fixable_span_edits`
        // returns nothing for it), so it never lands here.
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

    /// Publish diagnostics, but suppress them (publish an empty set) until the
    /// initial workspace index is ready. Before the index is built, a cross-file
    /// reference whose defining file isn't indexed yet would be flagged as
    /// undefined; the scan publishes the real diagnostics once it completes.
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

    /// Parse and validate a single document.
    /// Validate `uri` at `expected_version` after the debounce, but only if it is
    /// still the latest edit (a newer change supersedes it). Publishes the
    /// changed file's diagnostics, then refreshes the other open documents so
    /// cross-file references reflect the edit instead of showing stale results.
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
        // A newer change landed during the debounce or permit wait — let that
        // one validate without retaining a stale text snapshot in the queue.
        let text = {
            let docs = self.state.documents.lock();
            match docs.get(&uri) {
                Some(d) if d.version == expected_version => d.text.clone(),
                _ => return,
            }
        };

        // Snapshot the file's cross-file exports before re-indexing, so we can
        // tell whether this edit can affect any other file (see below). The
        // name set lets the dependent sweep target only docs that reference a
        // name that changed.
        let (exports_before, names_before) = {
            let info = self.state.info_service.read();
            (info.export_fingerprint(&uri), info.export_names(&uri))
        };

        let (diagnostics, parsed) = self
            .parse_and_validate(&uri, &text, trigger, Some(expected_version))
            .await;
        {
            let ast = parsed.map(Arc::new);
            // Update tokens before taking documents lock (doc_tokens must be
            // acquired before documents everywhere to avoid ABBA deadlock with
            // revalidate_open_dependents, which takes doc_tokens then documents).
            self.update_doc_tokens(&uri, ast.as_ref());
            let mut docs = self.state.documents.lock();
            // TOCTOU guard: did_close may have arrived while parse_and_validate
            // was running. Only store the AST if the document is still open at
            // the same version; if it closed, the index was already cleaned up by
            // did_close and we must not re-populate or re-publish it.
            let still_current = docs
                .get(&uri)
                .is_some_and(|document| document.version == expected_version);
            if !still_current {
                return;
            }
            // Preserve the last good AST on a transient parse failure (None):
            // a fatal mid-edit syntax error shouldn't wipe the tree that
            // completion/hover/goto resolve context from, or they collapse to
            // a generic word list until the next clean parse. The parse error
            // is still published. (#41) Loc/.cwt files always parse to None
            // here, so their (absent) AST is unaffected.
            if let Some(ast) = ast {
                docs.set_ast(&uri, expected_version, ast);
            }
        }
        // Re-check the doc is still open right before publishing: did_close may
        // have landed after the TOCTOU guard above, and publishing here would
        // race its empty publish and leave a stale diagnostic behind.
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

        // Only sweep the other open files if this edit actually changed what the
        // file exports (a definition added/renamed/removed). Editing inside a
        // rule body leaves the exports identical, so no dependent can change and
        // the sweep is skipped entirely — the common case stays cheap.
        let (exports_after, names_after) = {
            let info = self.state.info_service.read();
            (info.export_fingerprint(&uri), info.export_names(&uri))
        };
        let exports_changed = exports_before != exports_after;
        // Only the names that were added or removed can change another
        // file's diagnostics. Revalidate the open docs that reference any of
        // them (symmetric difference of the before/after name sets).
        //
        // The fingerprint also tracks multiplicity, so it can differ while
        // the name SET is unchanged (e.g. a duplicate definition added, or a
        // type changed under the same name) — a case that can still flip a
        // dependent's diagnostic. When that happens `changed_names` is empty;
        // fall back to `None` (revalidate every dependent) so we never miss
        // one. Soundness beats scoping here.
        let mut changed_names: HashSet<String> = names_before
            .symmetric_difference(&names_after)
            .cloned()
            .collect();
        // Drain any names accumulated from preempted prior sweeps so this
        // sweep covers their dependents too — plus the names this edit's own
        // validate queued when the file's `<type>` use set changed, which is
        // how an added or removed reference reaches the file DEFINING the
        // instance and flips its CW239/CW231 without its exports moving.
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
            // Tagged with this edit's generation so a newer edit preempts it.
            self.revalidate_open_dependents(&uri, generation, scope)
                .await;
        } else {
            tracing::debug!(uri = %uri, "exports unchanged; skipping dependent sweep");
        }
    }

    /// Re-validate and republish every open document except `changed_uri`, using
    /// the freshly updated indexes. Bounded by the number of open files, so a
    /// definition edit propagates to the gui/event/etc. files that reference it.
    ///
    /// `generation` is the edit counter at the time the triggering change landed.
    /// If a newer edit bumps the counter while the sweep is running, the sweep
    /// stops: the newer edit's own sweep will revalidate everything against the
    /// fully-updated index, so finishing this one is wasted work (and would
    /// double-validate). Each dependent's diagnostics are published with that
    /// doc's current version, and skipped if the doc changed mid-sweep, so the
    /// sweep never clobbers a fresher in-flight result for a file being edited.
    ///
    /// `changed_names`, when `Some`, scopes the sweep to the open docs whose
    /// token set mentions one of the (lowercased) names that were added or
    /// removed. A doc with no recorded token set is always included (sound
    /// over-approximation: never skip a file that might depend on the change).
    ///
    /// `None` revalidates every open dependent (used when the exact set of
    /// changed names can't be pinned down, e.g. a multiplicity-only change).
    ///
    /// On preemption (newer edit arrives mid-sweep), the `changed_names` are
    /// saved to `state.pending_changed_names` so the next sweep drains and
    /// includes them, preventing stale dependents from falling through the gap.
    pub(crate) async fn revalidate_open_dependents(
        &self,
        changed_uri: &str,
        generation: u64,
        changed_names: Option<&HashSet<String>>,
    ) {
        use std::sync::atomic::Ordering;

        // Doc token sets store interned `.lower` ids, so intern each changed
        // name once here instead of resolving every doc token to a fresh String.
        let changed_ids: Option<Vec<StringId>> = changed_names.map(|names| {
            names
                .iter()
                .map(|n| self.state.string_table.intern(n).lower)
                .collect()
        });
        // Snapshot each open dependent's cached AST (a cheap `Arc` clone) with
        // its version. The dependents' own text didn't change, so they don't
        // need re-parsing or re-indexing — only re-validation against the
        // now-updated global index. When `changed_names` is `Some`, skip docs
        // whose token set references none of the changed names.
        // Capture each dependent's text (an `Arc` bump) while the docs lock is
        // held so the republished diagnostics get whole-line squiggles and
        // encoded columns, same as the edited file.
        let mut others: Vec<(String, i32, Arc<ParsedFile>, Arc<str>)> = {
            let tokens = self.state.doc_tokens.read();
            let docs = self.state.documents.lock();
            docs.iter()
                .filter(|(u, _)| u.as_str() != changed_uri)
                .filter(|(u, _)| match &changed_ids {
                    None => true,
                    Some(ids) => match tokens.get(u.as_str()) {
                        // No token set recorded for this doc — include it rather
                        // than risk missing a real dependent.
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
        // Skip ignored.
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
        // Validate every dependent synchronously, then publish. No await is held
        // across the rules lock. The single `rules` read guard covers the ruleset,
        // the cached scope registry, and the modifier-key set (none change during
        // the sweep). Do NOT lock documents inside this block (ABBA: request
        // handlers take documents then rules; we must take rules then
        // nothing-or-documents-after).
        let validated: Vec<(String, i32, Vec<Diagnostic>, u64)> = {
            let rules_guard = self.state.rules.read();
            let mut out = Vec::with_capacity(others.len());
            for (uri, snapshot_version, ast, text) in others {
                // Preempt: a newer edit arrived. Save our changed_names into the
                // shared pending set so the newer sweep drains and covers them;
                // without this, dependents of the preempted edit stay stale.
                if self.state.edit_generation.load(Ordering::Relaxed) != generation {
                    tracing::debug!(generation, "revalidate_open_dependents superseded");
                    if let Some(names) = changed_names {
                        let mut pending = self.state.pending_changed_names.lock();
                        pending.extend(names.iter().cloned());
                    }
                    // Stop computing further dependents, but fall through to
                    // publish the ones already validated this sweep instead of
                    // discarding them. The newer sweep (draining
                    // pending_changed_names) covers the rest.
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
                    None => ast
                        .errors
                        .iter()
                        .map(|e| parse_error_to_diagnostic(e, &lines))
                        .collect(),
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
        // Now check still_current without holding ruleset (documents first is
        // the order used by request handlers, so this is safe).
        let to_publish: Vec<(String, i32, Vec<Diagnostic>, u64)> = validated
            .into_iter()
            .filter(|(uri, snapshot_version, _, _)| {
                // Skip if this dependent was itself edited while we validated it —
                // its own debounced pass owns the fresher result.
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

    /// Flatten both loc overlays (per-open-`.yml` key sets plus the watched-file
    /// sets) into one set of lowercased keys, for the game-file loc-existence
    /// checks (CW100/CW122) so a key just typed into an open `.yml` — or saved
    /// to a watched one — resolves without a full rescan (#36). Bounded by the
    /// open loc files plus the distinct watched ones.
    ///
    /// Cached by `loc_overlay_revision` and handed out by `Arc`: this ran per
    /// validated file, so a watched batch cloned every overlay key once per file
    /// in the batch even though nothing between them changed (#87). The revision
    /// is read before the overlay locks, so a writer that lands in between only
    /// costs a rebuild the next call, never a stale hit.
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

    /// Update the hover loc_text map with entries from a single loc file,
    /// replacing any previous entries for the same file. Called on every loc
    /// file edit so tooltips reflect the latest changes without a full
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

        // Collect the new entries for this file.
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

        // Merge into the global loc_text map: remove old entries for this
        // file's keys, then insert the new ones. A simple remove-and-replace
        // per key would lose entries from OTHER files that share the same key.
        // Instead, rebuild the affected keys from all sources.
        let mut loc_text = self.state.loc_text.write();
        for key in new_entries.keys() {
            // Remove any existing entry for this key that came from this file.
            // We can't track per-file contributions in loc_text (it's a flat
            // map), so just overwrite — the full rescan will correct any
            // cross-file ordering issues.
            loc_text.remove(key);
        }
        for (key, translations) in new_entries {
            loc_text.entry(key).or_default().extend(translations);
        }
    }

    /// The names a `$ref$` in a loc file may resolve to: [`loc_extra_valid_refs`]
    /// plus both loc overlays (the current keys of every open `.yml` and of the
    /// watched files changed since the last scan).
    ///
    /// This is a full copy of the modifier-key and index-name universe (~200K
    /// Strings on Millennium Dawn), so it is built ONCE per edit and shared by
    /// `Arc` with every file that edit revalidates — building it per file cost
    /// the keystroke path one whole copy per open `.yml`.
    ///
    /// Cached across edits too, keyed on `(info_revision, loc_overlay_revision)`
    /// — its inputs are the ruleset's modifier keys and the type index (both
    /// covered by `info_revision`) plus the two overlays. Keying on
    /// `info_revision` alone would be wrong: an open loc edit writes the overlay
    /// before it bumps that counter, so the set the edit itself validates
    /// against would still be missing the key just typed (#87).
    fn loc_ref_names(&self) -> Arc<HashSet<String>> {
        let revision = (
            self.state
                .info_revision
                .load(std::sync::atomic::Ordering::Relaxed),
            self.state
                .loc_overlay_revision
                .load(std::sync::atomic::Ordering::Acquire),
        );
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
        // Lock order: rules -> info_service. The overlay locks are independent
        // and taken after, never nested inside the others.
        let mut extra = {
            let modifier_keys = self.state.rules.read().modifier_keys.clone();
            let info = self.state.info_service.read();
            loc_extra_valid_refs(&modifier_keys, &info.type_index)
        };
        // Lets a key just added to an open `.yml` (or saved to a watched one)
        // resolve immediately, in that file and cross-file.
        for keys in self.state.loc_live_overlay.read().values() {
            extra.extend(keys.iter().cloned());
        }
        for keys in self.state.loc_watched_overlay.read().values() {
            extra.extend(keys.iter().cloned());
        }
        let extra = Arc::new(extra);
        *self.state.loc_ref_names_cache.lock() = Some((revision, Arc::clone(&extra)));
        extra
    }

    /// Validate one loc file's text into diagnostics against the scanned union
    /// and the shared `extra` name set from [`loc_ref_names`]. Pure: it neither
    /// updates the overlay nor triggers any cross-file work, so the cross-file
    /// sweep can call it safely (#36).
    fn validate_loc_text(
        &self,
        path: &str,
        text: &str,
        lines: &DocLines,
        extra: &HashSet<String>,
    ) -> Vec<Diagnostic> {
        self.validate_loc_parsed(path, &parse_loc_buffer(text, path), lines, extra)
    }

    /// [`validate_loc_text`] over an already-parsed buffer, so the edited file's
    /// own validate shares the one parse its key set and hover text came from.
    fn validate_loc_parsed(
        &self,
        path: &str,
        files: &[cwtools_localization::LocFile],
        lines: &DocLines,
        extra: &HashSet<String>,
    ) -> Vec<Diagnostic> {
        // Hold the read guard across the validate call to avoid cloning the full
        // loc-key union (~2M Strings on Millennium Dawn).
        let loc_guard = self.state.loc_index.read();
        let empty_union = cwtools_localization::LocKeySet::default();
        let union: &cwtools_localization::LocKeySet = loc_guard
            .as_deref()
            .map(|idx| idx.union())
            .unwrap_or(&empty_union);
        cwtools_localization::validate_parsed_loc_files(files, path, union, extra)
            .iter()
            .map(|d| validation_error_to_diagnostic(&loc_diag_to_validation_error(d), lines))
            .collect()
    }

    /// Re-validate and republish every OTHER open loc file. Called when an edited
    /// loc file's key set changed, so a `$ref$` to a key that was just added or
    /// removed updates in the other open `.yml` files without a reload (#36).
    /// Bounded by the number of open loc files.
    async fn revalidate_other_open_loc_files(&self, except_uri: &str, extra: &HashSet<String>) {
        let targets: Vec<(String, Arc<str>)> = {
            let docs = self.state.documents.lock();
            docs.iter()
                .filter(|(u, _)| u.as_str() != except_uri && crate::paths::is_loc_file(u))
                .map(|(u, d)| (u.clone(), d.text.clone()))
                .collect()
        };
        let encoding = self.state.config.read().position_encoding.clone();
        for (u, text) in targets {
            let path = uri_to_path_str(&u);
            let lines = DocLines::new(&text, encoding.clone());
            let diags = self.validate_loc_text(&path, &text, &lines, extra);
            if let Ok(obj) = Url::parse(&u) {
                self.publish_gated(
                    obj,
                    diags,
                    None,
                    Some(cwtools_cache::workspace::content_hash(&text)),
                )
                .await;
            }
        }
    }

    /// Record a watched (non-open) loc file's keys in the watched-files overlay
    /// (per-file replace, like the open-doc overlay) so cross-file `$ref$` and
    /// missing-loc checks resolve them without a rescan — and keep resolving
    /// them across scans, whose index installs are built from disk reads that
    /// may predate this change. Returns the keys added or removed relative to
    /// the previous entry (first sight is every key), for the batch's coalesced
    /// sweep.
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

    /// Whether an overlay already holds exactly `new_keys` for `uri`, checked
    /// under a read lock.
    ///
    /// Taking the write guard is what bumps `loc_overlay_revision`, and most
    /// loc keystrokes change a value rather than the key set — so re-inserting
    /// an identical set invalidated both derived caches on every edit and the
    /// `$ref$` name universe was rebuilt anyway. Skipping the write when
    /// nothing moved is what makes those caches actually hit while a `.yml` is
    /// being typed in.
    fn loc_overlay_entry_matches(
        &self,
        overlay: &parking_lot::RwLock<std::collections::HashMap<String, HashSet<String>>>,
        uri: &str,
        new_keys: &HashSet<String>,
    ) -> bool {
        overlay.read().get(uri).is_some_and(|prev| prev == new_keys)
    }

    /// The cross-file refresh an open loc edit runs per file, done ONCE for a
    /// whole watched batch whose loc key sets changed (#90). `changed_keys` is
    /// the batch-wide union of additions and removals.
    pub(crate) async fn refresh_after_watched_loc_changes(&self, changed_keys: &HashSet<String>) {
        self.bump_info_revision();
        let extra = self.loc_ref_names();
        self.revalidate_other_open_loc_files("", &extra).await;
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

    /// `parsed_version` is the open-document version `text` was taken at, so the
    /// index install can tell a newer edit's validate has overtaken this one.
    /// `None` when the text came from disk (the scan's non-open pass).
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
        // statement line and lands on the columns the client reads.
        let lines = DocLines::new(text, self.state.config.read().position_encoding.clone());
        // Inline `# cwtools-ignore` directives, line → lowercased codes. Empty
        // for files without one; the filter below is then a no-op.
        let inline_ignored = cwtools_validation::inline_ignore::extract_inline_ignored_codes(text);

        // Localisation files are parsed and validated as loc, not config.
        if crate::paths::is_loc_file(uri) {
            let path = uri_to_path_str(uri);
            // Keep the live overlay current so this file's own keys (and any just
            // added) resolve immediately in `$ref$` checks, without waiting for a
            // full rescan. Record which keys were added or removed. (#36)
            //
            // Open docs only: the overlay is "unsaved keys in open .yml files".
            // The watched path reaches here for files that are NOT open; letting
            // them in grew the map a stale entry per watched file and made every
            // first sight fire the whole-file cross-file sweep (#90). Watched
            // files record into `loc_watched_overlay` instead
            // (`record_watched_loc_keys`) with one coalesced sweep per batch.
            // block_in_place: parsing and linting a loc buffer is sync CPU work
            // that would otherwise hold a runtime worker for its whole duration,
            // and MD ships loc files in the hundreds of KB. Matches how the scan
            // paths already fence their sync work. (#87)
            let (changed_keys, extra, diagnostics) = tokio::task::block_in_place(|| {
                // One parse of the edited buffer, shared by the key set, the
                // diagnostics and the hover text below — each used to parse the
                // whole file itself, and two of them copied it first (#87).
                let parsed_loc = parse_loc_buffer(text, &path);
                let is_open = self.state.documents.lock().contains_key(uri);
                let changed_keys: HashSet<String> = if is_open {
                    let new_keys = loc_keys_from(&parsed_loc);
                    // Skip the write when the key set is unchanged, which is
                    // the common keystroke: taking the write guard bumps the
                    // revision the derived caches key on, so re-inserting an
                    // identical set rebuilt the whole `$ref$` name universe on
                    // every edit. See `loc_overlay_entry_matches`.
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
                // Built once here and shared with the cross-file sweep below,
                // which would otherwise rebuild the whole name set per open
                // `.yml`.
                let extra = self.loc_ref_names();
                let diagnostics = self.validate_loc_parsed(&path, &parsed_loc, &lines, &extra);
                // Update the hover loc_text map so tooltips reflect the latest
                // edits without waiting for a full workspace rescan (#53).
                self.update_loc_text_for_file(&parsed_loc);
                (changed_keys, extra, diagnostics)
            });
            // A change to this file's key set can fix or break `$ref$` checks in
            // other open loc files, so refresh them — that's the cross-file part
            // of the index that previously only updated on a window reload.
            // It can also fix or break a missing-localisation (CW100/CW122)
            // diagnostic on open GAME files that reference the added/removed key
            // (e.g. a new event option's loc), so re-validate those too — the
            // overlay now feeds the game-file loc checks, so they resolve the new
            // key without a full rescan. (#36) The sweep is scoped to the docs
            // whose tokens mention a changed key or a definition name it derives
            // from, instead of every open game file.
            if !changed_keys.is_empty() {
                self.bump_info_revision();
                self.revalidate_other_open_loc_files(uri, &extra).await;
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

        // A loc-extension file outside any `localisation` dir (a CI workflow, an
        // editor config) is neither loc nor game script: publish nothing rather
        // than parsing YAML as Paradox script.
        if crate::paths::has_loc_ext(uri) {
            return (diagnostics, None);
        }

        // `.cwt` rule-config files are the schema the engine is built from, not
        // game content. Lint them structurally — parse errors plus references to
        // undefined types/enums/single_aliases — against the loaded merged
        // ruleset, rather than running the game-script validator (which would
        // flag every rule field as unknown). See #43.
        if crate::paths::is_cwt_file(uri) {
            let parsed = parse_string(text, &self.state.string_table);
            for parse_err in &parsed.errors {
                diagnostics.push(parse_error_to_diagnostic(parse_err, &lines));
            }
            // Structural reference check against the merged ruleset. Only runs
            // once rules are loaded; before then there's nothing to resolve
            // references against (and everything would falsely report
            // undefined).
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

        // block_in_place: everything from here down is sync CPU work (parse,
        // index, validate) with no await in it. Without the fence a 1 MB script
        // file holds a tokio worker for the whole validate, while the scan paths
        // that do the same work already fence theirs. (#87)
        tokio::task::block_in_place(|| {
            let parsed = parse_string(text, &self.state.string_table);
            for parse_err in &parsed.errors {
                diagnostics.push(parse_error_to_diagnostic(parse_err, &lines));
            }

            // Index this file the same way the workspace scan and did_close
            // disk-restore do (previously an inlined, drifted subset that
            // skipped subtype-membership indexing — an open file could lose
            // its `<type.subtype>` membership while being edited).
            self.index_parsed_file(uri, &parsed, parsed_version);

            // Validation. Lock order: rules -> info_service -> loc_index.
            let (errors, log_msg) = {
                let game = self.state.config.read().game();
                // Live overlay of unsaved loc keys in open `.yml` files, so a
                // key just added there resolves in this file's loc checks
                // (CW100/CW122) without a full rescan (#36). Computed before
                // the other guards (independent lock).
                let overlay = self.loc_overlay_keys();
                let rules_guard = self.state.rules.read();
                if let Some(ruleset) = rules_guard.ruleset.as_ref() {
                    let start = std::time::Instant::now();
                    // Pass the workspace TypeIndex for cross-file type reference checking.
                    let info_guard = self.state.info_service.read();
                    let type_index = &info_guard.type_index;
                    let loc_guard = self.state.loc_index.read();
                    // Single-file path: the scope registry is cached (built
                    // once at ruleset load).
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
                // tracing, not a client log_message, so a per-keystroke/
                // per-watched-file line doesn't flood the output channel.
                // Still captured by exportProfilingLog and stderr (RUST_LOG).
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

// ── Keystroke-validate micro-benchmark (ignored, manual) ─────────────────────
//
// Per-edit costs on a large real fixture (cc_colony_events.txt, ~165KB,
// concatenated 8x to match MD's largest script file ~1.3MB): the doc-token
// rebuild that runs after every debounced validate, and the per-request
// logical-path derivation. Run with:
//
//   cargo test --release -p cwtools_lsp --bin cwtools-server -- \
//     --ignored --nocapture perf_keystroke_validate
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
    /// used to be parsed three times — once for the live overlay's key set, once
    /// for the diagnostics, once for the hover text — and copied into an owned
    /// `String` twice on the way, because `LocService::from_files` takes
    /// ownership. Both sides exclude the diagnostics build, which is unchanged.
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

    /// The `$ref$` name set every loc-file edit builds ([`loc_ref_names`]), at
    /// Millennium-Dawn scale. `validate_loc_text` used to build one of these per
    /// call, so an edit with N loc files open paid it N+1 times; it is now built
    /// once and shared, and this is what each avoided rebuild cost.
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
    }

    /// Where the per-edit single-file index spends its time, split into the part
    /// that now runs before `info_service.write()` is taken and the part that
    /// still runs under it (completion blocks on that lock). Needs a real
    /// ruleset and a real game file; skips when neither is on this machine.
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

        // A focus file (no subtypes), an events file, and an equipment file
        // (`<type.subtype>` archetypes) — the three shapes of hot single-file
        // reindex, so the split isn't read off one unrepresentative case.
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
            // One InfoService across iterations, as in the server: `clear_file`
            // then has real entries to drop, and the ruleset-derived reference
            // map is reused instead of rebuilt per iteration.
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

/// `index_parsed_file` bumps `info_revision` only when the reindexed file's
/// export fingerprint moved (#289). These pin both halves: an edit that changes
/// no export must leave the derived caches valid, and any edit that adds,
/// renames or removes one must invalidate them.
#[cfg(test)]
mod info_revision_tests {
    use super::*;
    use crate::{Backend, CompletionCacheEntry, DocumentState};
    use cwtools_rules::rules_types::{PathOptions, TypeDefinition};
    use std::sync::atomic::Ordering;

    /// Workspace URI for the fixtures. Drive-lettered on Windows so the derived
    /// path is absolute there too (see the `abs()` helpers elsewhere).
    const WORKSPACE_URI: &str = if cfg!(windows) {
        "file:///C:/ws"
    } else {
        "file:///ws"
    };

    /// A file under the one path the fixture ruleset indexes.
    fn ideas_uri() -> String {
        format!("{WORKSPACE_URI}/common/ideas/00_ideas.txt")
    }

    /// A `Backend` over a real (never-initialized) `Client`, carrying a
    /// one-type ruleset and the workspace prefix the logical path is derived
    /// from. Same construction as `scan`'s `test_backend`.
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

    /// Seed `fallback_cache` the way the completion handler does, so the tests
    /// assert against the real hit condition (`request.rs`: revision match plus
    /// a non-empty list) rather than a paraphrase of it.
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

        // Same definition, different body, the shape of nearly every keystroke.
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

        // What a deleted definition (or a file emptied on disk before a
        // watched revalidate) leaves behind.
        index(&backend, &uri, "\n");

        assert_ne!(revision(&backend), settled);
        assert!(!fallback_cache_hits(&backend));
        assert!(
            !backend.loc_ref_names().contains("my_idea"),
            "a removed definition must not keep resolving"
        );
    }

    /// The completion fallback list is built from `variable_counts` and
    /// `event_target_counts`, so those two families have to move the revision
    /// on their own. They are exports the type index never sees.
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

    /// A file that exports nothing on either side is the one case where a
    /// first index does not bump, and it is correct: neither cache reads
    /// anything it contributes.
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

    /// The last few codes, which is where the marker and CW277 land. Printing
    /// all 100+ on a failure buries the interesting end of the list.
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

    /// CW277 says the file's remaining diagnostics were never produced. It is
    /// emitted last, so a plain `truncate` to the cap dropped exactly the
    /// diagnostic that explains the truncation.
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

    /// The held-back CW277 must not create a summary marker of its own when the
    /// rest of the file fits under the cap exactly.
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
}

#[cfg(test)]
mod whole_line_range_tests {
    use super::*;
    use cwtools_validation::ErrorSeverity;
    use std::collections::HashMap;

    #[test]
    fn loc_keys_of_extracts_lowercased_keys() {
        // Live-overlay key extraction for #36: keys are lowercased to match the
        // case-insensitive union the `$ref$` check resolves against.
        let keys = loc_keys_of(
            "l_english:\n MY_Key: \"hi\"\n other_key: \"x\"\n",
            "a_l_english.yml",
        );
        assert!(keys.contains("my_key"), "got: {:?}", keys);
        assert!(keys.contains("other_key"), "got: {:?}", keys);
        assert!(!keys.contains("absent"));
    }

    #[test]
    fn loc_change_candidates_cover_derived_cw100_keys() {
        // CW100 keys are derived (`prefix + name + suffix`), so a changed key
        // `my_focus_desc` must scope the sweep to docs mentioning `my_focus`
        // (the definition name), not just the literal key.
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
                // Optional / explicit-field entries are not CW100-flagged.
                loc("", "_opt", true, true),
            ],
            graph_related_types: Vec::new(),
            modifiers: Vec::new(),
        });
        let changed: HashSet<String> = ["my_focus_desc".to_string(), "plainkey".to_string()]
            .into_iter()
            .collect();
        let names = loc_change_candidate_names(Some(&rs), &changed);
        // Literal keys always included (CW122); derived names stripped per affix.
        assert!(names.contains("my_focus_desc"));
        assert!(names.contains("plainkey"));
        assert!(names.contains("my_focus"), "got: {:?}", names);
        assert!(
            !names.contains("my_focus_desc_opt") && !names.contains("my_focus_d"),
            "optional affixes must not expand: {:?}",
            names
        );
        // No ruleset: literal keys only.
        assert_eq!(loc_change_candidate_names(None, &changed), changed);
    }

    #[test]
    fn loc_extra_valid_refs_covers_every_bindable_name() {
        // The per-edit path used to collect `instances("idea")` alone, so a
        // `$my_variable$` (or any non-idea definition) validated clean in CI and
        // after a workspace scan, then grew a CW225 the moment the file was
        // opened and lost it again on the next rescan. All three paths build the
        // set from the same source now.
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
        // The published range must use the SAME conversion the code-action fix
        // edits use, or a quick fix on a line holding a non-BMP character
        // rewrites a different span than the one highlighted. `😀` is 1 char /
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
        // Whole-line fallback: with `end: None` the squiggle still runs from the
        // field to the line's content end (unchanged pre-Task-18 behavior).
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
        // "    custom_cost_text = a" is 24 chars.
        assert_eq!(diag.range.end.character, 24);
    }

    #[test]
    fn diagnostic_uses_precise_range_when_end_present() {
        // Task 18: when the emit site carried the node's end position, the LSP
        // publishes the exact token span instead of the whole-line squiggle. The
        // end uses the same 1-based-line / 0-based-char convention as the start.
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
            // The leaf's own SourceRange end (1-based line 2, exclusive char 24).
            end: Some((2, 24)),
            related: Vec::new(),
        };
        let diag = validation_error_to_diagnostic(&err, &ends);
        assert_eq!(diag.range.start.line, 1);
        assert_eq!(diag.range.start.character, 4);
        // Precise end from the carried range, not diag_end_col.
        assert_eq!(diag.range.end.line, 1);
        assert_eq!(diag.range.end.character, 24);
    }

    #[test]
    fn diagnostic_precise_range_can_span_lines() {
        // A block's range legitimately spans lines; only the trailing whitespace
        // the parser folded into the end has to come back off.
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
        // The CW500 shape: an indented next line puts the raw end mid-whitespace
        // rather than at column 0.
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
        // A whitespace-only statement line: the walk-back would run past the start.
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
        // Clamping after the conversion instead would land inside the surrogate pair.
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

    // The workspace scan holds no file text, so there is nothing to walk the
    // parser's end back over. Publishing it raw would bleed onto the next line
    // for every file that is not open, which is most of the Problems panel.
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
        // Suppression list is stored lowercased; the diagnostic code can be any case.
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
        // Absent and numeric codes are never suppressed.
        assert!(!code_is_suppressed(None, &ignored));
        assert!(!code_is_suppressed(
            Some(&NumberOrString::Number(100)),
            &ignored
        ));
        // Empty list suppresses nothing.
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
        // Parse errors carry no CW code, so there is no row to point at.
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
        // A code the reader still has to act on must not be faded.
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
        // Ruleset-load diagnostics name a plain path; there is no document to
        // hang a location off, and a bogus URI would be worse than none.
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

    use crate::state::DocumentState;
    use cwtools_parser::parser::parse_string;
    use cwtools_string_table::string_table::StringTable;
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
        // No workspace_prefix -> logical path is the raw decoded path; bare-name still matches.
        let backend = backend_with_ignore(vec!["ignored.txt".into()], None);
        assert!(backend.is_ignored_uri("file:///tmp/ignored.txt"));
        // Percent-encoded workspace and file.
        let backend2 = backend_with_ignore(vec![], Some("file:///tmp/My%20Mod"));
        // README baseline under encoded workspace.
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
        // Ensure the workspace root is the temp dir (canonicalized).
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
        // Seed indexes: info, doc_tokens, type_uses, file_index, loc overlays.
        let table = StringTable::new();
        let parsed = parse_string("my_type = { }", &table);
        // Manually populate file_index so removal can be asserted.
        {
            let mut info = backend.state.info_service.write();
            info.type_index.file_index.insert("common/foo.txt");
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
        // Index the file (even without a ruleset the doc_tokens/type_uses/file_index
        // and overlay state are what this test cares about; fingerprint may stay 0).
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
    async fn parse_and_validate_ignored_returns_empty_and_clears_index() {
        let backend = backend_with_ignore(vec!["ignored.txt".into()], Some("file:///ws"));
        let uri = "file:///ws/ignored.txt";
        let text = "my_type = { broken }";
        // Seed an index entry that must be cleared.
        let table = StringTable::new();
        let seed = parse_string("my_type = { }", &table);
        backend.index_parsed_file("file:///ws/common/keep.txt", &seed, None);
        // Put something for the ignored URI so we can see it disappear.
        backend.index_parsed_file(uri, &seed, None);
        // Now park an ignored parse: the defensive guard should have cleared it,
        // but we ensure parse_and_validate also clears and returns empty.
        // First make the ignored file look indexed again (guard would have cleared it,
        // so re-seed via direct info write).
        {
            let mut info = backend.state.info_service.write();
            info.clear_file(uri);
            // Re-seed via direct index to simulate stale state before config change.
            drop(info);
            backend.index_parsed_file(uri, &seed, None);
            // Force ignore by setting pattern after indexing: re-create backend with ignore
            // (already is) and ensure clear happens on next validate.
        }
        // The uri is ignored, so parse_and_validate must return empty diagnostics.
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
        // Document is retained (so revalidate_all_open_docs can find it) but index is clear.
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
        // Open as kept first.
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
        // Reconfigure to ignore it (simulates adding glob while open).
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
        // After an edit to an ignored open doc, the index must be cleared, not repopulated.
        // Allow the async publish to settle.
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
        // Open both directly so we can seed state before the ignore takes effect.
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
        backend
            .state
            .info_service
            .write()
            .type_index
            .file_index
            .insert("ignored.txt");
        backend
            .state
            .info_service
            .write()
            .type_index
            .file_index
            .insert("kept.txt");
        // Now mark ignored.txt as ignored and revalidate all open docs.
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
                },
            )
            .unwrap();
        }
        // Both get token sets; the ignored one's tokens must be ignored by the sweep.
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
        // Trigger a dependent sweep from kept; the ignored doc mentions the changed name,
        // but must be skipped because it is ignored.
        let mut changed = std::collections::HashSet::new();
        // Use a token that the ignored doc actually contains so it would be selected without the filter.
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
        // No panic and ignored doc still not indexed (defensive).
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
        // Seed file_index as non-empty so the watched insert is not gated off.
        {
            let mut info = backend.state.info_service.write();
            info.type_index.file_index.insert("dummy.txt");
        }
        let mut changes = std::collections::HashSet::new();
        changes.insert(ignored_uri.clone());
        changes.insert(kept_uri.clone());
        backend.process_watched_batch(changes, vec![]).await;
        // Ignored must not have been inserted; kept must have been.
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
        // Ignored must have been cleared and not left with a watched signature.
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
            info.type_index.file_index.insert("dummy.txt");
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
