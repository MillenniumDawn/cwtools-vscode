//! Project-level loc-file validation.
//!
//! Runs the scope-independent loc-entry checks (`validate_loc_file`) over every
//! loaded loc file and normalizes the results to the F# numeric error codes
//! (CW001/CW225/CW234/CW259/CW268/CW275/CW276), plus the per-file name/header checks
//! (CW254/CW255/CW256/CW257). The scope-dependent command checks
//! (CW226/CW260/CW266) run at the config reference site, where the scope of the
//! referencing field is known; [`validate_loc_project_commands`] is the
//! standalone counterpart for a caller that has a ruleset but no game files.

use crate::commands::{Lang, LocFile};
use crate::loc_index::LocKeySet;
use crate::scope_validation::{LocCommandDiagnostic, LocScopeData, validate_loc_commands};
use crate::service::LocService;
use crate::validation::{LocErrorKind, hardcoded_loc_set, validate_loc_file_with_hardcoded};
use crate::yaml_parser::{LangHeaderDiagnostic, check_loc_file_lang, parse_loc_text};
use cwtools_error_codes::{ErrorCode, ErrorSeverity};
use cwtools_game::scope_engine::{SCOPE_ANY, ScopeId};
use cwtools_game::scope_registry::ScopeRegistry;
use std::collections::HashSet;

/// A normalized loc diagnostic ready to be surfaced as a `ValidationError` or an
/// LSP `Diagnostic`. `line`/`col` are 1-based.
#[derive(Debug, Clone, PartialEq)]
pub struct LocDiagnostic {
    pub file: String,
    pub line: usize,
    pub col: usize,
    pub code: &'static str,
    pub severity: ErrorSeverity,
    pub message: String,
    /// Optional machine-applicable fix (CW268). Pure metadata; the CLI `fix`
    /// subcommand and the LSP code-action provider consume it. The report/hash
    /// path never reads it.
    pub fix: Option<cwtools_parser::fix::SuggestedFix>,
}

/// Single source of truth for a scope-independent loc-entry error's code and
/// severity (matching the F# `ErrorCodes` mapping). Splitting this from the
/// message means the code / severity accessors don't build (and discard) a
/// formatted `String`, and the emission path formats each message exactly once.
fn loc_error_code_severity(kind: &LocErrorKind) -> (&'static str, ErrorSeverity) {
    match kind {
        LocErrorKind::UndefinedLocReference { .. } => ("CW225", ErrorSeverity::Error),
        LocErrorKind::RecursiveLocRef => ("CW259", ErrorSeverity::Error),
        LocErrorKind::ReplaceMe => ("CW234", ErrorSeverity::Information),
        LocErrorKind::LocMissingQuote => ("CW268", ErrorSeverity::Warning),
        LocErrorKind::LocInvalidChars => ("CW275", ErrorSeverity::Warning),
        LocErrorKind::LocKeyInvalidChars => ("CW276", ErrorSeverity::Warning),
    }
}

/// Code, severity, and human-readable message for a loc-entry error, built in one
/// pass so the emission path (`build_diagnostics`) formats the message once.
fn loc_error_parts(
    kind: &LocErrorKind,
    key: &str,
    lang: Option<Lang>,
) -> (&'static str, ErrorSeverity, String) {
    let (code, severity) = loc_error_code_severity(kind);
    (code, severity, loc_error_message(kind, key, lang))
}

/// The F# numeric code for a scope-independent loc-entry error.
pub fn loc_error_code(kind: &LocErrorKind) -> &'static str {
    loc_error_code_severity(kind).0
}

/// The severity for a scope-independent loc-entry error.
pub fn loc_error_severity(kind: &LocErrorKind) -> ErrorSeverity {
    loc_error_code_severity(kind).1
}

/// Build the human-readable message, matching the F# `ErrorCodes` text.
fn loc_error_message(kind: &LocErrorKind, key: &str, lang: Option<Lang>) -> String {
    let lang_label = lang
        .map(|l| l.to_string())
        .unwrap_or_else(|| "?".to_string());
    match kind {
        LocErrorKind::UndefinedLocReference { other_key } => format!(
            "Localisation key \"{}\" references \"{}\" which doesn't exist in {}",
            key, other_key, lang_label
        ),
        LocErrorKind::RecursiveLocRef => "This localisation string refers to itself".to_string(),
        LocErrorKind::ReplaceMe => {
            format!(
                "Localisation key {} is a placeholder for {}",
                key, lang_label
            )
        }
        LocErrorKind::LocMissingQuote => format!(
            "Localisation key {} doesn't start and end with double quotes",
            key
        ),
        LocErrorKind::LocInvalidChars => format!(
            "Localisation value for {} contains unexpected characters, and may not render correctly",
            key
        ),
        LocErrorKind::LocKeyInvalidChars => format!(
            "Localisation key {} contains invalid characters (spaces or special characters are not allowed)",
            key
        ),
    }
}

/// F# `STLLang` case name, used to reproduce the CW257 message (`%A`).
fn lang_fsharp_name(lang: Lang) -> &'static str {
    match lang {
        Lang::English => "English",
        Lang::French => "French",
        Lang::German => "German",
        Lang::Spanish => "Spanish",
        Lang::Russian => "Russian",
        Lang::Polish => "Polish",
        Lang::BrazPor => "Braz_Por",
        Lang::SimpChinese => "Chinese",
        Lang::Japanese => "Japanese",
        Lang::Korean => "Korean",
        Lang::Turkish => "Turkish",
        Lang::Default => "Default",
    }
}

/// Per-file name/header language check (CW255/CW256/CW257).
///
/// Mirrors F# `STLLocalisationString.checkLocFileName`: a loc file's name must
/// carry a recognised `l_xxx` tag, the first line must be a recognised
/// `l_xxx:` header, and the two must agree. CSV loc (CK2/VIC2) has no YAML
/// header line at all, so the check doesn't apply — `language_prefix` there is
/// a bare language name ("english"), which `key_to_language` never matches.
fn lang_header_diagnostic(file: &LocFile) -> Option<LocDiagnostic> {
    if file.is_csv {
        return None;
    }
    let (code, severity, message): (&'static str, ErrorSeverity, String) = match check_loc_file_lang(
        &file.path,
        &file.language_prefix,
    )? {
        LangHeaderDiagnostic::MissingLocFileLangHeader { .. } => (
            cwtools_error_codes::CW256_MISSING_LOC_FILE_LANG_HEADER.id,
            ErrorSeverity::Error,
            "Localisation file should start with \"l_language:\" on the first line (or a comment)"
                .to_string(),
        ),
        LangHeaderDiagnostic::MissingLocFileLang { .. } => (
            cwtools_error_codes::CW255_MISSING_LOC_FILE_LANG.id,
            ErrorSeverity::Error,
            "Localisation file name should contain (and ideally end with) \"l_language.yml\""
                .to_string(),
        ),
        LangHeaderDiagnostic::LocFileLangMismatch {
            filename_lang,
            header_lang,
            ..
        } => (
            cwtools_error_codes::CW257_LOC_FILE_LANG_MISMATCH.id,
            ErrorSeverity::Error,
            format!(
                "Localisation file's name has language {} doesn't match the header language {}",
                lang_fsharp_name(filename_lang),
                lang_fsharp_name(header_lang)
            ),
        ),
    };
    Some(LocDiagnostic {
        file: file.path.clone(),
        line: 1,
        col: 1,
        code,
        severity,
        message,
        fix: None,
    })
}

/// Build the per-file diagnostics for one parsed loc file, in the fixed F# order:
/// CW255/256/257 (lang header) → CW254 (encoding) → CW001 (parse errors) →
/// CW225/234/259/268/275 (loc-entry checks).
///
/// `file_path` is the path used for every diagnostic's `file` field (the project
/// path passes `&file.path`; the single-file path passes its `path` argument —
/// both are the same string the file was parsed under).
///
/// `emit_cw254` controls the one DELIBERATE divergence between the two callers:
/// the project (directory-loading) path knows the on-disk encoding and passes
/// `true` only when the file is `Utf8NoBom`/`NonUtf8`; the single-file text path
/// has no on-disk bytes to inspect and always passes `false`, so it never emits
/// CW254. Do not flip this without changing the corpus.
fn build_diagnostics(
    file: &LocFile,
    file_path: &str,
    union: &LocKeySet,
    additional_loc_keys: &HashSet<String>,
    extra_valid_refs: &HashSet<String>,
    hardcoded: &HashSet<String>,
    emit_cw254: bool,
) -> Vec<LocDiagnostic> {
    let lang = file.lang;
    let mut out: Vec<LocDiagnostic> = Vec::new();

    // CW255/256/257: file name vs language header.
    if let Some(d) = lang_header_diagnostic(file) {
        out.push(d);
    }

    // CW254: localisation files must be UTF-8 with BOM. Only enforced when the
    // on-disk encoding is known (the directory-loading path); the caller has
    // already resolved that condition into `emit_cw254`.
    if emit_cw254 {
        out.push(LocDiagnostic {
            file: file_path.to_string(),
            line: 1,
            col: 1,
            code: cwtools_error_codes::CW254_WRONG_ENCODING.id,
            severity: ErrorSeverity::Error,
            message: "Localisation files must be UTF-8 BOM, this file is not".to_string(),
            fix: None,
        });
    }

    // CW001: line-level parse errors collected during lenient recovery.
    for pe in &file.parse_errors {
        out.push(LocDiagnostic {
            file: file_path.to_string(),
            line: pe.line,
            col: 1,
            code: cwtools_error_codes::CW001_PARSE_ERROR.id,
            severity: ErrorSeverity::Error,
            message: cwtools_error_codes::CW001_PARSE_ERROR.format(&[pe.message.as_str()]),
            fix: None,
        });
    }

    for err in validate_loc_file_with_hardcoded(
        file,
        union,
        additional_loc_keys,
        extra_valid_refs,
        hardcoded,
    ) {
        let (code, severity, message) = loc_error_parts(&err.kind, &err.key, lang);
        out.push(LocDiagnostic {
            file: file_path.to_string(),
            line: err.line,
            col: err.col,
            code,
            severity,
            message,
            fix: err.fix,
        });
    }
    out
}

/// Whether CW254 (wrong encoding) should fire for a file, given its detected
/// on-disk encoding. Only the directory-loading path populates `encoding`; the
/// text-only path leaves it `None`, which is correctly treated as "don't fire".
fn should_emit_cw254(file: &LocFile) -> bool {
    matches!(
        file.encoding,
        Some(cwtools_file_manager::FileEncoding::Utf8NoBom)
            | Some(cwtools_file_manager::FileEncoding::NonUtf8)
    )
}

/// Validate every loaded loc file and return normalized diagnostics.
pub fn validate_loc_project(service: &LocService) -> Vec<LocDiagnostic> {
    validate_loc_project_scoped(service, None, &HashSet::new())
}

/// As [`validate_loc_project`], but only emit per-file diagnostics for files
/// whose language is in `langs` (when `Some`). Files with no detectable language
/// are always validated (they may be malformed). Every file still contributes to
/// the key `union`, so `$ref$` existence resolves against all loaded languages.
/// `langs = None` validates every file (the previous behavior).
///
/// `extra_valid_refs` are additional lowercased names a `$ref$` may resolve to
/// besides loc keys — game-definition registries the engine resolves in loc
/// context (modifiers, ideas). A ref matching one of these is treated as
/// defined, suppressing CW225. Pass `&HashSet::new()` for none.
pub fn validate_loc_project_scoped(
    service: &LocService,
    langs: Option<&[Lang]>,
    extra_valid_refs: &HashSet<String>,
) -> Vec<LocDiagnostic> {
    use rayon::prelude::*;

    // Union of keys across all languages, to resolve `$ref$` existence.
    // Borrowed from the service's single owned copy — no second copy of any loc
    // file is ever materialized (a full clone OOMs on large projects like MD).
    // Built in parallel: on large projects (~2M entries) the sequential
    // lowercase+insert dominated. Same case-folding (`to_lowercase`) as before;
    // the resulting set is identical regardless of insert order.
    let union: LocKeySet = service
        .files()
        .par_iter()
        .flat_map_iter(|file| file.entries.iter().map(|e| e.key.to_lowercase().into()))
        .collect();
    validate_loc_project_with_union(service, langs, &union, extra_valid_refs)
}

/// As [`validate_loc_project_scoped`], but reuses a caller-owned key `union`
/// instead of rebuilding it. The [`crate::LocIndex`] already holds the lowercased
/// union (with any merged vanilla-cache keys); passing it by reference avoids a
/// third full materialization of the ~2M-key universe per run. When the union
/// carries cached vanilla keys it's a superset, but a `$ref$` found in it only
/// triggers the recursion check on a self-reference (the entry's own key, always
/// present regardless), so the emitted diagnostics are unchanged.
pub fn validate_loc_project_with_union(
    service: &LocService,
    langs: Option<&[Lang]>,
    union: &LocKeySet,
    extra_valid_refs: &HashSet<String>,
) -> Vec<LocDiagnostic> {
    use rayon::prelude::*;

    // Each file validates independently against the read-only key union, so the
    // per-file pass runs in parallel. `par_iter` over the indexed `files` slice
    // collects in input order — output matches the sequential version.
    // Lowercased hardcoded-loc set, built once and shared read-only across the
    // per-file parallel pass (was re-lowercased + re-collected per file).
    let hardcoded = hardcoded_loc_set();
    let additional_loc_keys = HashSet::new();
    service
        .files()
        .par_iter()
        .filter(|file| lang_selected(file, langs))
        .flat_map_iter(|file| {
            // Directory-loading path: CW254 fires when the detected on-disk
            // encoding is missing/wrong BOM.
            build_diagnostics(
                file,
                &file.path,
                union,
                &additional_loc_keys,
                extra_valid_refs,
                hardcoded,
                should_emit_cw254(file),
            )
            .into_iter()
        })
        .collect()
}

/// Run the scope-aware loc-command checks (CW226/CW260/CW266) over every loaded
/// entry. For a caller that has a ruleset but no game files — the standalone
/// `cwtools loc` lint.
///
/// `cwtools validate` runs the same checks at each reference site, seeded with
/// the scope of the field using the key. There is no reference site here, so
/// every chain starts at `any` and only what is wrong in every scope is
/// reported. `data.registry` is what turns the checks on: without one the
/// command validator stays fully lenient and this finds nothing.
pub fn validate_loc_project_commands(
    service: &LocService,
    langs: Option<&[Lang]>,
    data: &LocScopeData,
) -> Vec<LocDiagnostic> {
    use rayon::prelude::*;

    let registry = data.registry.as_deref();
    service
        .files()
        .par_iter()
        .filter(|file| lang_selected(file, langs))
        .flat_map_iter(|file| {
            file.entries.iter().flat_map(move |entry| {
                validate_loc_commands(entry, SCOPE_ANY, data)
                    .into_iter()
                    .map(move |diag| {
                        let (code, message) = loc_command_parts(&diag, &entry.key, registry);
                        LocDiagnostic {
                            file: file.path.clone(),
                            line: entry.position.line,
                            col: entry.position.column,
                            code: code.id,
                            severity: code.severity,
                            message,
                            fix: None,
                        }
                    })
            })
        })
        .collect()
}

/// The code and message a loc-command diagnostic is reported under. Shared by
/// the reference-site check in `cwtools_validation` and by
/// [`validate_loc_project_commands`], so the same finding reads the same way
/// whichever pass found it.
///
/// `loc_key` is the key whose value carries the command. `registry` names the
/// scopes in a CW260 message; without one their numeric ids are printed.
pub fn loc_command_parts(
    diag: &LocCommandDiagnostic,
    loc_key: &str,
    registry: Option<&ScopeRegistry>,
) -> (&'static ErrorCode, String) {
    let scope_name = |id: u32| match registry {
        Some(reg) => reg.name_of(ScopeId(id)),
        None => id.to_string(),
    };
    match diag {
        LocCommandDiagnostic::WrongScope {
            command,
            current_scope,
            expected_scopes,
        } => {
            let expected = expected_scopes
                .iter()
                .map(|s| scope_name(*s))
                .collect::<Vec<_>>()
                .join(", ");
            let code = &cwtools_error_codes::CW260_LOC_COMMAND_WRONG_SCOPE;
            let message = code.format(&[command, &scope_name(*current_scope), &expected]);
            (code, message)
        }
        LocCommandDiagnostic::ChainEndsInScope { command } => {
            let code = &cwtools_error_codes::CW266_LOC_COMMAND_NOT_IN_DATA_TYPE;
            (code, code.format(&[loc_key, command.as_str(), "scope"]))
        }
        LocCommandDiagnostic::NotFound { command } => {
            let code = &cwtools_error_codes::CW226_INVALID_LOC_COMMAND;
            (code, code.format(&[loc_key, command.as_str()]))
        }
        LocCommandDiagnostic::ScriptedGuiNotFound { callback } => {
            let code = &cwtools_error_codes::CW283_SCRIPTED_GUI_CALLBACK_NOT_FOUND;
            (code, code.format(&[loc_key, callback.as_str()]))
        }
    }
}

/// Whether a file's language is in the scoped set. A file with no detectable
/// language can't be scoped out — it may be malformed, which is what the checks
/// are for.
fn lang_selected(file: &LocFile, langs: Option<&[Lang]>) -> bool {
    match langs {
        Some(set) => file.lang.is_none_or(|l| set.contains(&l)),
        None => true,
    }
}

/// Validate a single loc file's text against a precomputed key union. Used by
/// the LSP to lint a `.yml`/`.csv` file on open/change without rebuilding the
/// whole service. Returns an empty vec if the text can't be parsed as loc.
pub fn validate_loc_file_text(
    text: &str,
    path: &str,
    union: &LocKeySet,
    extra_valid_refs: &HashSet<String>,
) -> Vec<LocDiagnostic> {
    let Ok(file) = parse_loc_text(text, path) else {
        return Vec::new();
    };
    // Text-only path: no on-disk bytes to inspect, so CW254 never fires here.
    // This is the deliberate divergence from the project path — do not change it.
    build_diagnostics(
        &file,
        path,
        union,
        &HashSet::new(),
        extra_valid_refs,
        hardcoded_loc_set(),
        false,
    )
}

/// As [`validate_loc_file_text`], but for a caller that already parsed the text
/// (see [`crate::parse_loc_files`]), so one edited buffer isn't parsed once per
/// consumer (#87).
///
/// `.csv` files produce nothing here, matching what the text entry point has
/// always done — its `parse_loc_text` rejects them. The project path
/// ([`validate_loc_project`]) is the one that lints CSV loc.
pub fn validate_parsed_loc_files(
    files: &[LocFile],
    path: &str,
    union: &LocKeySet,
    extra_valid_refs: &HashSet<String>,
) -> Vec<LocDiagnostic> {
    validate_parsed_loc_files_with_additional_keys(
        files,
        path,
        union,
        &HashSet::new(),
        extra_valid_refs,
    )
}

pub fn validate_parsed_loc_files_with_additional_keys(
    files: &[LocFile],
    path: &str,
    union: &LocKeySet,
    additional_loc_keys: &HashSet<String>,
    extra_valid_refs: &HashSet<String>,
) -> Vec<LocDiagnostic> {
    files
        .iter()
        .filter(|file| !file.is_csv)
        .flat_map(|file| {
            build_diagnostics(
                file,
                path,
                union,
                additional_loc_keys,
                extra_valid_refs,
                hardcoded_loc_set(),
                false,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_file_manager::FileEncoding;

    fn service_from(files: &[(&str, &str)]) -> LocService {
        LocService::from_files(
            files
                .iter()
                .map(|(p, t)| (p.to_string(), t.to_string()))
                .collect(),
        )
    }

    #[test]
    fn parsed_and_text_entry_points_agree() {
        // #87 shares one parse across the LSP's key/diagnostic/hover consumers,
        // so the parsed entry point must return exactly what the text one did —
        // including nothing at all for CSV loc, which `parse_loc_text` rejects.
        let union = LocKeySet::default();
        let extra = HashSet::new();
        for (path, text) in [
            (
                "a_l_english.yml",
                "l_english:\n key1: \"Hello $undefined_key$\"\n",
            ),
            ("events.yml", "l_english:\n key1: \"hi\"\n"),
            ("names.csv", "key;english;french;x\nfoo;Foo;Fou;x\n"),
        ] {
            let from_text = validate_loc_file_text(text, path, &union, &extra);
            let files = crate::parse_loc_files(path, text, None).unwrap_or_default();
            let from_parsed = validate_parsed_loc_files(&files, path, &union, &extra);
            let codes = |d: &[LocDiagnostic]| d.iter().map(|d| d.code).collect::<Vec<_>>();
            assert_eq!(codes(&from_text), codes(&from_parsed), "path: {path}");
        }
    }

    /// The registry is the gate: `cwtools loc` without `--rules` has none, and
    /// an unknown command has to stay lenient there rather than be invented.
    /// The scripted-localisation registry is the second gate (#348) — without it
    /// a tail could be a `defined_text` — so this supplies an empty one, meaning
    /// "the project defines none", not "nothing is known".
    #[test]
    fn the_project_command_pass_needs_a_registry() {
        let svc = service_from(&[(
            "a_l_english.yml",
            "l_english:\n key1: \"Ruled by [totally_unknown()]\"\n",
        )]);
        assert!(
            validate_loc_project_commands(&svc, None, &LocScopeData::default()).is_empty(),
            "no registry means no command checks"
        );

        let data = LocScopeData {
            game: Some(cwtools_game::constants::Game::Hoi4),
            registry: Some(std::sync::Arc::new(ScopeRegistry::default())),
            scripted_locs: Some(&|_: &str| false),
            ..Default::default()
        };
        let diags = validate_loc_project_commands(&svc, None, &data);
        assert_eq!(diags.len(), 1, "got: {diags:?}");
        assert_eq!(diags[0].code, "CW226");
        assert_eq!(diags[0].line, 2);
        assert!(
            diags[0].message.contains("totally_unknown"),
            "message: {}",
            diags[0].message
        );
    }

    #[test]
    fn scripted_gui_not_found_maps_to_cw283() {
        let (code, message) = loc_command_parts(
            &LocCommandDiagnostic::ScriptedGuiNotFound {
                callback: "missing_click".into(),
            },
            "my_key",
            None,
        );
        assert_eq!(code.id, "CW283");
        assert_eq!(
            message,
            "Localisation key \"my_key\" calls scripted GUI callback \"missing_click\" which does not exist"
        );
    }

    #[test]
    fn parenthesised_jomini_expression_does_not_emit_an_empty_command() {
        let svc = service_from(&[(
            "a_l_english.yml",
            "l_english:\n key1: \"Keep [(Character?.GetName:'CAP_SCIENTIST')]\"\n",
        )]);
        let data = LocScopeData {
            game: Some(cwtools_game::constants::Game::Hoi4),
            registry: Some(std::sync::Arc::new(ScopeRegistry::default())),
            ..Default::default()
        };

        assert!(validate_loc_project_commands(&svc, None, &data).is_empty());
    }

    #[test]
    fn undefined_ref_maps_to_cw225() {
        let svc = service_from(&[(
            "a_l_english.yml",
            "l_english:\n key1: \"Hello $undefined_key$\"\n",
        )]);
        let diags = validate_loc_project(&svc);
        let cw225: Vec<_> = diags.iter().filter(|d| d.code == "CW225").collect();
        assert_eq!(cw225.len(), 1, "got: {:?}", diags);
        assert_eq!(cw225[0].severity, ErrorSeverity::Error);
        assert!(cw225[0].message.contains("english"));
    }

    #[test]
    fn filename_without_lang_maps_to_cw255() {
        // Valid header, but the file name carries no l_xxx tag.
        let svc = service_from(&[("events.yml", "l_english:\n key1: \"hi\"\n")]);
        let diags = validate_loc_project(&svc);
        let cw255: Vec<_> = diags.iter().filter(|d| d.code == "CW255").collect();
        assert_eq!(cw255.len(), 1, "got: {:?}", diags);
        assert_eq!(cw255[0].severity, ErrorSeverity::Error);
    }

    #[test]
    fn unrecognised_header_maps_to_cw256() {
        // File name has a lang tag, but the header language is unknown.
        let svc = service_from(&[("events_l_english.yml", "l_klingon:\n key1: \"hi\"\n")]);
        let diags = validate_loc_project(&svc);
        let cw256: Vec<_> = diags.iter().filter(|d| d.code == "CW256").collect();
        assert_eq!(cw256.len(), 1, "got: {:?}", diags);
        assert_eq!(cw256[0].severity, ErrorSeverity::Error);
    }

    #[test]
    fn name_header_mismatch_maps_to_cw257() {
        // File name says english, header says french.
        let svc = service_from(&[("events_l_english.yml", "l_french:\n key1: \"hi\"\n")]);
        let diags = validate_loc_project(&svc);
        let cw257: Vec<_> = diags.iter().filter(|d| d.code == "CW257").collect();
        assert_eq!(cw257.len(), 1, "got: {:?}", diags);
        assert!(
            cw257[0].message.contains("English") && cw257[0].message.contains("French"),
            "message: {}",
            cw257[0].message
        );
    }

    #[test]
    fn matching_name_and_header_no_lang_diag() {
        let svc = service_from(&[("events_l_english.yml", "l_english:\n key1: \"hi\"\n")]);
        let diags = validate_loc_project(&svc);
        assert!(
            diags
                .iter()
                .all(|d| !matches!(d.code, "CW255" | "CW256" | "CW257")),
            "got: {:?}",
            diags
        );
    }

    #[test]
    fn csv_loc_file_skips_yaml_lang_header_check() {
        // CK2/VIC2-style CSV loc: `language_prefix` is a bare name ("english"),
        // which the YAML-only `l_xxx` header check never matches. It must not
        // fire CW255/256/257.
        let csv = "#CODE;English;French;German;;Spanish\nKEY_A;Hello;Bonjour;Hallo;;Hola\n";
        let svc = service_from(&[("mod/localisation/localisation.csv", csv)]);
        let diags = validate_loc_project(&svc);
        assert!(
            diags
                .iter()
                .all(|d| !matches!(d.code, "CW255" | "CW256" | "CW257")),
            "CSV loc files must not trigger the YAML lang-header check: {:?}",
            diags
        );
    }

    #[test]
    fn replace_me_maps_to_cw234_info() {
        let svc = service_from(&[("a_l_english.yml", "l_english:\n key1: \"REPLACE_ME\"\n")]);
        let diags = validate_loc_project(&svc);
        let cw234: Vec<_> = diags.iter().filter(|d| d.code == "CW234").collect();
        assert_eq!(cw234.len(), 1, "got: {:?}", diags);
        assert_eq!(cw234[0].severity, ErrorSeverity::Information);
    }

    #[test]
    fn invalid_chars_message_attributes_problem_to_value() {
        // The offending characters live in the loc VALUE, not the key; the message
        // must say so (CW275). A zero-width space (U+200B) is genuine invisible junk
        // that stays flagged even after the allow-list is widened for real scripts.
        let svc = service_from(&[(
            "a_l_english.yml",
            "l_english:\n bad_loc_entry: \"hello\u{200b}world\"\n",
        )]);
        let diags = validate_loc_project(&svc);
        let cw275: Vec<_> = diags.iter().filter(|d| d.code == "CW275").collect();
        assert_eq!(cw275.len(), 1, "got: {:?}", diags);
        let msg = &cw275[0].message;
        assert!(
            msg.contains("value") && msg.contains("bad_loc_entry"),
            "CW275 should attribute the bad characters to the value of the entry, got: {msg}"
        );
    }

    fn service_with_encoding(files: &[(&str, &str, Option<FileEncoding>)]) -> LocService {
        LocService::from_files_with_encoding(
            files
                .iter()
                .map(|(p, t, e)| (p.to_string(), t.to_string(), *e))
                .collect(),
        )
    }

    #[test]
    fn no_bom_maps_to_cw254_error() {
        let svc = service_with_encoding(&[(
            "a_l_english.yml",
            "l_english:\n key1: \"hi\"\n",
            Some(FileEncoding::Utf8NoBom),
        )]);
        let cw254: Vec<_> = validate_loc_project(&svc)
            .into_iter()
            .filter(|d| d.code == "CW254")
            .collect();
        assert_eq!(cw254.len(), 1, "missing-BOM file should warn CW254");
        assert_eq!(cw254[0].severity, ErrorSeverity::Error);
    }

    #[test]
    fn non_utf8_maps_to_cw254_error() {
        let svc = service_with_encoding(&[(
            "a_l_english.yml",
            "l_english:\n key1: \"hi\"\n",
            Some(FileEncoding::NonUtf8),
        )]);
        assert_eq!(
            validate_loc_project(&svc)
                .iter()
                .filter(|d| d.code == "CW254")
                .count(),
            1
        );
    }

    #[test]
    fn bom_present_no_cw254() {
        let svc = service_with_encoding(&[(
            "a_l_english.yml",
            "l_english:\n key1: \"hi\"\n",
            Some(FileEncoding::Utf8Bom),
        )]);
        assert!(
            validate_loc_project(&svc).iter().all(|d| d.code != "CW254"),
            "UTF-8 BOM file should not warn CW254"
        );
    }

    #[test]
    fn unknown_encoding_no_cw254() {
        // The text-only path (LSP edits, tests) can't see bytes — no CW254.
        let svc =
            service_with_encoding(&[("a_l_english.yml", "l_english:\n key1: \"hi\"\n", None)]);
        assert!(validate_loc_project(&svc).iter().all(|d| d.code != "CW254"));
    }

    /// The four tests above hand the encoding over ready-made, so none of them
    /// covers the sniff that produces it. This one writes the real `EF BB BF`
    /// (and leaves it off) and takes the files through the walk-read-detect
    /// path the CLI's `loc` command uses, which is the only way CW254 fires.
    #[test]
    fn a_real_bom_on_disk_decides_cw254() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = tmp.path().join("localisation");
        std::fs::create_dir_all(&loc).unwrap();
        let body = "l_english:\n key1: \"hi\"\n";

        let mut bommed = vec![0xEF, 0xBB, 0xBF];
        bommed.extend_from_slice(body.as_bytes());
        std::fs::write(loc.join("bom_l_english.yml"), &bommed).unwrap();
        std::fs::write(loc.join("nobom_l_english.yml"), body).unwrap();

        let svc = LocService::from_folder(tmp.path(), cwtools_file_manager::ScanBudget::default());
        assert_eq!(svc.files().len(), 2, "errors: {:?}", svc.errors());
        let flagged: Vec<String> = validate_loc_project(&svc)
            .into_iter()
            .filter(|d| d.code == "CW254")
            .map(|d| d.file.replace('\\', "/"))
            .collect();
        assert_eq!(flagged.len(), 1, "got: {flagged:?}");
        assert!(
            flagged[0].ends_with("nobom_l_english.yml"),
            "only the BOM-less file may be flagged, got: {flagged:?}"
        );
    }

    /// The BOM has to survive the read as a BOM and not as three characters of
    /// the language header: a file that only just parses (`l_english:` on the
    /// first line, right after the marker) must still resolve its language,
    /// else it would collect CW255/CW256 instead of nothing.
    #[test]
    fn a_real_bom_does_not_leak_into_the_language_header() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = tmp.path().join("localisation");
        std::fs::create_dir_all(&loc).unwrap();
        let mut bommed = vec![0xEF, 0xBB, 0xBF];
        bommed.extend_from_slice(b"l_english:\n key1: \"hi\"\n");
        std::fs::write(loc.join("bom_l_english.yml"), &bommed).unwrap();

        let svc = LocService::from_folder(tmp.path(), cwtools_file_manager::ScanBudget::default());
        let codes: Vec<&str> = validate_loc_project(&svc).iter().map(|d| d.code).collect();
        assert!(codes.is_empty(), "a well-formed loc file: {codes:?}");
    }

    #[test]
    fn malformed_line_emits_cw001_and_rest_parses() {
        // A line with no ':' separator triggers CW001 at the recovery point.
        // The surrounding valid entries must still parse (parser remains lenient).
        let text = "l_english:\n good_key: \"valid\"\nthis line has no colon at all\n another_key: \"also valid\"\n";
        let svc = service_from(&[("a_l_english.yml", text)]);
        let diags = validate_loc_project(&svc);

        let cw001: Vec<_> = diags.iter().filter(|d| d.code == "CW001").collect();
        assert_eq!(
            cw001.len(),
            1,
            "exactly one CW001 for one bad line: {:?}",
            diags
        );
        assert_eq!(cw001[0].severity, ErrorSeverity::Error);
        assert_eq!(cw001[0].line, 3, "bad line is line 3");

        // The good entries still parse — no spurious CW225/CW100 from the bad line.
        assert!(
            diags.iter().all(|d| d.code != "CW225"),
            "no CW225 from recovered parse: {:?}",
            diags
        );
    }

    #[test]
    fn unterminated_string_maps_to_cw268() {
        // Regression: opening quote with no closing quote was falsely reported as
        // balanced because the truncation reduced effective to a single `"`.
        let svc = service_from(&[(
            "a_l_english.yml",
            "l_english:\n missing_quote:0 \"unclosed\n",
        )]);
        let diags = validate_loc_project(&svc);
        let cw268: Vec<_> = diags.iter().filter(|d| d.code == "CW268").collect();
        assert_eq!(
            cw268.len(),
            1,
            "unterminated string should emit CW268: {:?}",
            diags
        );
        assert_eq!(cw268[0].severity, ErrorSeverity::Warning);
    }

    #[test]
    fn key_with_space_maps_to_cw276() {
        let svc = service_from(&[("a_l_english.yml", "l_english:\n \"bad key\": \"value\"\n")]);
        let diags = validate_loc_project(&svc);
        let cw276: Vec<_> = diags.iter().filter(|d| d.code == "CW276").collect();
        assert_eq!(
            cw276.len(),
            1,
            "key with space should emit CW276: {:?}",
            diags
        );
        assert_eq!(cw276[0].severity, ErrorSeverity::Warning);
        assert!(
            cw276[0].message.contains("bad key") || cw276[0].message.contains("\"bad key\""),
            "message should reference the key: {}",
            cw276[0].message
        );
    }

    #[test]
    fn well_formed_file_no_cw001() {
        let svc = service_from(&[("a_l_english.yml", "l_english:\n key1: \"hi\"\n")]);
        assert!(
            validate_loc_project(&svc).iter().all(|d| d.code != "CW001"),
            "well-formed file must not emit CW001"
        );
    }

    #[test]
    fn parallel_command_validation_with_scripted_variables_is_sync() {
        // Regression for the rayon Sync gate: `validate_loc_project_commands`
        // fans out over `par_iter` while holding `&LocScopeData`, which carries
        // `scripted_variables: Option<&dyn Fn(&str) -> bool>`. Without `Sync`
        // on that trait object the closure passed to `flat_map_iter` fails to
        // satisfy `Sync + Send` on every platform (Windows/macOS/Linux).
        use crate::scope_validation::LocScopeData;
        use cwtools_game::constants::Game;
        use cwtools_game::scope_engine::ScopeId;
        use cwtools_game::scope_engine::ScopeLink;
        use cwtools_game::scope_registry::{ScopeDefOwned, ScopeRegistry};
        use std::sync::Arc;

        let mut reg = ScopeRegistry::default();
        for (name, id) in [("country", 100u32), ("state", 101u32)] {
            reg.by_name.insert(name.to_string(), ScopeId(id));
            reg.by_id.insert(
                ScopeId(id),
                ScopeDefOwned {
                    name: name.to_string(),
                    aliases: vec![name.to_string()],
                    subscope_of: vec![],
                },
            );
        }
        reg.links.insert(
            "owner".to_string(),
            ScopeLink {
                valid_scopes: vec![ScopeId(101)],
                target: Some(ScopeId(100)),
                ignore_keys: vec![],
            },
        );
        // Scripted variable registry must be `Sync` so the parallel pipeline
        // can share it across threads.
        fn is_known_var(name: &str) -> bool {
            name.eq_ignore_ascii_case("war_support")
        }
        let data = LocScopeData {
            game: Some(Game::Hoi4),
            registry: Some(Arc::new(reg)),
            terminal_commands: ["getname"].into_iter().map(String::from).collect(),
            question_mark_variable: true,
            parameter_variables: true,
            scripted_variables: Some(&is_known_var),
            scripted_locs: Some(&|_: &str| false),
            scripted_guis: None,
        };
        // Build a service with many entries so rayon actually parallelizes.
        let mut files = Vec::new();
        for i in 0..50 {
            files.push((
                format!("a_{i}_l_english.yml"),
                format!(
                    "l_english:\n key{i}: \"[?Root.war_support] and [owner.GetName] and [totally_unknown]\"\n"
                ),
            ));
        }
        let svc = service_from(
            &files
                .iter()
                .map(|(p, t)| (p.as_str(), t.as_str()))
                .collect::<Vec<_>>(),
        );
        // This is the exact call that previously failed to compile when
        // `ScriptedVariables` lacked `Sync`.
        let diags = validate_loc_project_commands(&svc, None, &data);
        // `totally_unknown` is an unknown terminal command: with a registry
        // and a non-empty terminal list it must be flagged (CW226 for a Jomini
        // single-segment chain, CW266 for the legacy single-segment path).
        // Either way it must surface, while `war_support` and `owner.GetName`
        // are accepted. The count proves the rayon `Sync` gate held.
        let flagged = diags
            .iter()
            .filter(|d| d.code == "CW226" || d.code == "CW266")
            .count();
        assert_eq!(flagged, 50, "each file has one unknown command: {diags:?}");
        // Ensure `LocScopeData` itself is `Sync` (compile-time assertion).
        fn assert_sync<T: Sync>() {}
        assert_sync::<LocScopeData>();
    }
}
