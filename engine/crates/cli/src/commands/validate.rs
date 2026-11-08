//! `validate`: run the whole engine over a mod directory and render a report.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use cwtools_driver::{RulesInput, Session, SessionConfig, SessionWithFiles, VanillaCacheAuto};
use cwtools_file_manager::path_identity;
use cwtools_game::Game;
use cwtools_info::vanilla_cache;
use cwtools_localization::Lang;
use cwtools_rules::ruleset_loader::RuleParseError;
use cwtools_validation::ErrorSeverity;

use super::watch;
use crate::cli::ValidateArgs;
use crate::diag::{
    Diag, SourceLines, cli_row, csv_row, is_ignored, json_row, loc_diagnostic_to_diag,
    rule_error_to_diag, severity_rank, validation_to_diag_with_path,
};
use crate::report::ReportType;
use crate::run::{
    EXIT_USAGE, ModLayers, announce_config, color_enabled, exit_code, exit_if_empty,
    gate_vanilla_cache_for_replacements, load_config, load_ignore_hashes, missing_required, note,
    parse_game, rebuild_vanilla_cache_if_stale, report_owns_stdout, status, vanilla_notice,
};
use crate::scope::FileScope;
use crate::{codes, config, report, scope};

pub(super) fn run(args: ValidateArgs) {
    let ValidateArgs {
        config,
        game,
        directory,
        rules,
        vanilla,
        parents: parent_roots,
        vanilla_cache,
        no_vanilla_cache,
        refresh_vanilla_cache,
        report_type,
        output_file,
        ignore_hashes,
        output_hashes,
        ignore_files,
        ignore_dirs,
        loc_language,
        case_sensitive_files,
        min_severity,
        fail_on,
        ignore_codes,
        only_codes,
        files,
        since,
        allow_empty,
        watch: watching,
    } = args;
    let file_cfg = load_config(config.as_deref(), directory.as_deref());
    let mut applied: Vec<&'static str> = Vec::new();
    let fc = file_cfg.as_ref();
    config::pick_fields!(fc, applied;
        option game => game "game";
        option directory => directory "directory";
        option rules => rules "rules";
        option vanilla => vanilla "vanilla";
        option vanilla_cache => vanilla_cache "vanilla-cache";
        flag no_vanilla_cache => no_vanilla_cache "no-vanilla-cache";
        flag refresh_vanilla_cache => refresh_vanilla_cache "refresh-vanilla-cache";
        default case_sensitive_files => case_sensitive_files "case-sensitive-files";
        option report_type => report_type "report-type";
        option min_severity => min_severity "min-severity";
        option fail_on => fail_on "fail-on";
        list ignore_files => ignore_files "ignore-files";
        list ignore_dirs => ignore_dirs "ignore-dirs";
        list parent_roots => parents "parents";
        list loc_language => loc_languages "loc-languages";
        list ignore_codes => ignore_codes "ignore-codes";
        list only_codes => only_codes "only-codes";
        flag allow_empty => allow_empty "allow-empty";
    );
    let mut report_type = report_type.unwrap_or(ReportType::Cli);
    if watching && report_type != ReportType::Cli {
        // The flag form is a clap conflict; this is a `cwtools.toml` written for CI.
        eprintln!("warn: --watch reports as text; ignoring report-type from the config");
        report_type = ReportType::Cli;
    }
    let fail_on = fail_on.unwrap_or_default();
    announce_config("validate", fc, &applied, config::VALIDATE_KEYS);

    let game = game.unwrap_or_else(|| missing_required("validate", "--game <GAME>", "game", fc));
    let directory = directory.unwrap_or_else(|| {
        missing_required("validate", "--directory <DIRECTORY>", "directory", fc)
    });
    let rules =
        rules.unwrap_or_else(|| missing_required("validate", "--rules <RULES>", "rules", fc));
    let ignored = load_ignore_hashes(ignore_hashes.as_deref());

    // Resolved before the session loads: an unresolvable `--since` should fail
    // in a moment, not after a full index.
    let scope = scope::resolve(&files, since.as_deref(), &directory).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(EXIT_USAGE);
    });
    if let Some(s) = &scope {
        for path in s.missing() {
            eprintln!("warn: --file {} is not on disk", path.display());
        }
        if output_hashes.is_some() {
            eprintln!(
                "warn: --output-hashes with --file/--since writes only the scoped subset's \
                 hashes; a baseline built this way drops everything outside it"
            );
        }
    }

    let game_id = parse_game(&game);
    let parent_roots =
        crate::run::resolve_parent_roots(&parent_roots, &directory, game_id, vanilla.as_deref());

    let rules_label = if rules.is_dir() {
        format!("directory {}", rules.display())
    } else {
        format!("file {}", rules.display())
    };
    note(format!(
        "Validating {} files in {} against rules {}",
        game_id,
        directory.display(),
        rules_label
    ));

    // Snapshotted before the first load, so an edit saved while it runs still
    // counts as a change. Ctrl-C ends a watch with status 0, even mid-load.
    let poller = watching.then(|| {
        watch::exit_on_interrupt();
        let roots = watch::roots(&rules, &directory, &parent_roots, vanilla.as_deref());
        watch::Poller::new(roots)
    });

    // One pass is the whole load-and-validate pipeline. `--watch` repeats it
    // in full rather than patching the last result: a changed file can move
    // diagnostics in any other (cross-file references, the CW239 use pass), and
    // the parse cache already keeps the unchanged files cheap.
    let job = Job {
        game,
        game_id,
        directory,
        rules,
        vanilla,
        parent_roots,
        vanilla_cache,
        no_vanilla_cache,
        refresh_vanilla_cache,
        ignored,
        ignore_files,
        ignore_dirs,
        loc_language,
        case_sensitive_files,
        min_severity,
        ignore_codes,
        only_codes,
        scope,
        allow_empty,
    };
    let Analysis {
        diags,
        vanilla_notice,
        discovery_failed,
        discovery_incomplete,
        session,
        ..
    } = analyze(&job, true);

    let total_errors = diags
        .iter()
        .filter(|d| d.severity == cwtools_validation::ErrorSeverity::Error)
        .count();
    let total_warnings = diags
        .iter()
        .filter(|d| d.severity == cwtools_validation::ErrorSeverity::Warning)
        .count();

    // Render the report in the requested format.
    let mut out = String::new();
    match report_type {
        ReportType::Csv => {
            out.push_str("file,line,severity,code,message,hash\n");
            for d in &diags {
                out.push_str(&csv_row(d));
            }
        }
        ReportType::Json => {
            out.push_str("[\n");
            for (i, d) in diags.iter().enumerate() {
                out.push_str(&json_row(d, i + 1 >= diags.len()));
            }
            out.push_str("]\n");
        }
        ReportType::Github => {
            let root = report::report_root();
            if let Some(notice) = &vanilla_notice {
                out.push_str(&report::github_notice(notice));
            }
            for d in &diags {
                out.push_str(&report::github_row(d, &root));
            }
        }
        ReportType::Sarif => {
            let refs: Vec<&Diag> = diags.iter().collect();
            out.push_str(&report::sarif_report(
                &refs,
                &report::report_root(),
                vanilla_notice.as_deref(),
            ));
        }
        ReportType::Cli => {
            let color = color_enabled(output_file.is_none());
            let mut current = "";
            for d in &diags {
                if &*d.file != current {
                    out.push_str(&format!("\n  {}:\n", d.file));
                    current = &d.file;
                }
                out.push_str(&cli_row(d, color));
            }
            out.push_str(&format!(
                "\nValidation complete: {} errors, {} warnings\n",
                total_errors, total_warnings
            ));
        }
    }

    let mut write_failed = match &output_file {
        Some(p) => {
            if let Err(e) = std::fs::write(p, &out) {
                eprintln!("Error writing report {}: {}", p.display(), e);
                true
            } else {
                status(
                    format!(
                        "Wrote {} report ({} errors, {} warnings) to {}",
                        report_type.as_str(),
                        total_errors,
                        total_warnings,
                        p.display()
                    ),
                    false,
                );
                false
            }
        }
        None => {
            print!("{}", out);
            false
        }
    };

    // Write the surviving hashes for use as a future baseline.
    if let Some(p) = &output_hashes {
        let mut hashes: Vec<&str> = diags.iter().map(|d| d.hash.as_str()).collect();
        hashes.sort_unstable();
        hashes.dedup();
        if let Err(e) = std::fs::write(p, hashes.join("\n")) {
            eprintln!("Error writing hashes {}: {}", p.display(), e);
            write_failed = true;
        } else {
            status(
                format!(
                    "Wrote {} diagnostic hashes to {}",
                    hashes.len(),
                    p.display()
                ),
                report_owns_stdout(report_type, output_file.as_ref()),
            );
        }
    }

    // A walk that failed on the first pass has nothing to watch: fall through
    // to the usual exit code. Errors in the report do not end a watch.
    if !discovery_failed && !discovery_incomplete && let Some(poller) = poller {
        drop(session);
        watch::follow(poller, diags, || {
            let rerun = analyze(&job, false);
            if rerun.discovery_failed || rerun.discovery_incomplete || rerun.rules_unreadable {
                eprintln!("warn: could not read the inputs; keeping the previous results");
                return None;
            }
            Some(rerun.diags)
        });
    }

    let failing = fail_on.failing(diags.iter().map(|d| d.severity));
    if discovery_incomplete {
        eprintln!("error: validation is incomplete because some inputs could not be scanned");
    }
    let code = validation_exit_code(failing, discovery_failed, discovery_incomplete, write_failed);
    if code != 0 {
        std::process::exit(code);
    }
}

fn prepare_script_source(
    sources: &mut SourceLines,
    session: &SessionWithFiles,
    source: &cwtools_driver::SourceFile,
    key: &str,
) {
    match session.read_source_text(source) {
        Ok(text) => sources.set_text(key, Some(&text)),
        Err(error) => {
            eprintln!("warn: could not read report source {}: {}", source.path.display(), error);
            sources.set_text(key, None);
        }
    }
}

fn validation_exit_code(
    failing: usize,
    discovery_failed: bool,
    discovery_incomplete: bool,
    write_failed: bool,
) -> i32 {
    exit_code(failing, discovery_failed || discovery_incomplete, write_failed)
}

#[cfg(test)]
mod tests {
    use super::{prepare_script_source, validation_exit_code};
    use crate::diag::SourceLines;
    use cwtools_driver::{RulesInput, Session, SessionConfig};
    use cwtools_file_manager::path_identity;
    use cwtools_game::Game;

    #[test]
    fn reporting_never_rereads_a_grown_script_without_its_snapshot_bound() {
        for grow_before_validation in [true, false] {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("mod");
            let scripts = root.join("common/ideas");
            std::fs::create_dir_all(&scripts).unwrap();
            let path = scripts.join("test.txt");
            std::fs::write(&path, "x = 1\n").unwrap();
            let rules = tmp.path().join("rules.cwt");
            std::fs::write(&rules, "").unwrap();
            let session = Session::load(SessionConfig {
                game: Game::Hoi4,
                rules: RulesInput::File(rules),
                directory: root,
                parent_roots: vec![],
                vanilla: None,
                vanilla_cache: None,
                vanilla_cache_auto: None,
                ignore_files: &[],
                ignore_dirs: &[],
                loc_languages: None,
                case_sensitive_files: false,
                on_rules_diagnostic: None,
            });
            let source = session.parsed_files().iter().find(|f| f.path == path).unwrap();
            if grow_before_validation {
                std::fs::write(&path, "changed = ".repeat(1024)).unwrap();
            }
            let _ = session.validate_all();
            if !grow_before_validation {
                assert!(!session.is_incomplete());
                std::fs::write(&path, "changed = ".repeat(1024)).unwrap();
            }
            let key = path_identity(&path);
            let mut sources = SourceLines::default();
            prepare_script_source(&mut sources, &session, source, &key);
            assert_eq!(sources.trimmed_path(&key, &path, 1), "");
            assert!(session.is_incomplete());
        }
    }

    #[test]
    fn incomplete_validation_is_not_a_successful_exit() {
        assert_eq!(validation_exit_code(0, false, false, false), 0);
        assert_ne!(validation_exit_code(0, false, true, false), 0);
    }
}

/// Everything one pass reads, resolved once from the flags and `cwtools.toml`.
struct Job {
    game: String,
    game_id: Game,
    directory: PathBuf,
    rules: PathBuf,
    vanilla: Option<PathBuf>,
    parent_roots: Vec<PathBuf>,
    vanilla_cache: Option<PathBuf>,
    no_vanilla_cache: bool,
    refresh_vanilla_cache: bool,
    ignored: HashSet<String>,
    ignore_files: Vec<String>,
    ignore_dirs: Vec<String>,
    loc_language: Vec<Lang>,
    case_sensitive_files: bool,
    min_severity: Option<ErrorSeverity>,
    ignore_codes: Vec<String>,
    only_codes: Vec<String>,
    scope: Option<FileScope>,
    allow_empty: bool,
}

/// What a pass found.
struct Analysis {
    diags: Vec<Diag>,
    vanilla_notice: Option<String>,
    discovery_failed: bool,
    discovery_incomplete: bool,
    rules_unreadable: bool,
    /// Held so a failing plain run exits before paying for the teardown.
    session: SessionWithFiles,
}

/// Load the session and collect every diagnostic the report keeps. A reload
/// (`first_run` false) skips what only the first pass should do: exiting on an
/// empty input, which a half-saved file can cause for a moment; rebuilding the
/// base-game cache; and honouring `--refresh-vanilla-cache`.
fn analyze(job: &Job, first_run: bool) -> Analysis {
    let Job {
        game,
        game_id,
        directory,
        rules,
        vanilla,
        parent_roots,
        vanilla_cache,
        no_vanilla_cache,
        refresh_vanilla_cache,
        ignored,
        ignore_files,
        ignore_dirs,
        loc_language,
        case_sensitive_files,
        min_severity,
        ignore_codes,
        only_codes,
        scope,
        allow_empty,
    } = job;
    let game_id = *game_id;

    // Per-phase timings on stderr when CWTOOLS_TIMINGS is set.
    let _timings = std::env::var_os("CWTOOLS_TIMINGS").is_some();
    let mut _tprev = std::time::Instant::now();
    macro_rules! tlog {
        ($label:expr) => {{
            if _timings {
                eprintln!("  [t] {} {:?}", $label, _tprev.elapsed());
            }
            _tprev = std::time::Instant::now();
        }};
    }

    // Load a pre-generated vanilla index, if given (faster than --vanilla;
    // resolves base-game references without re-parsing the install).
    // Fingerprint comparison happens after the session is loaded (needs
    // the ruleset); stale caches are detected there and re-generated.
    let (cached_fingerprint, vanilla_cache_index) = vanilla_cache
        .as_ref()
        .map_or((None, None), |cache_path| {
            let loaded = match vanilla_cache::load(cache_path) {
                Ok((cache_game, cached_fp, data)) => {
                    if Game::from_str(&cache_game) != Some(game_id) {
                        eprintln!(
                            "  warn: vanilla cache was built for game '{}', validating '{}'",
                            cache_game, game
                        );
                    }
                    let total: usize = data.per_type.values().map(|v| v.len()).sum();
                    note(format!(
                        "  Loaded {} base-game instances, {} loc languages, {} files from cache {} (fp: {})",
                        total,
                        data.aux.loc_keys.len(),
                        data.aux.file_paths.len(),
                        cache_path.display(),
                        cached_fp,
                    ));
                    Some((cached_fp, data))
                }
                Err(e) => {
                    eprintln!(
                        "  warn: could not load vanilla cache {}: {}",
                        cache_path.display(),
                        e
                    );
                    None
                }
            };
            gate_vanilla_cache_for_replacements(
                cache_path,
                loaded,
                ModLayers {
                    primary: directory,
                    parents: parent_roots,
                },
                vanilla.as_deref(),
            )
        });

    // Without an explicit --vanilla-cache, keep one under the OS cache dir
    // so repeat runs don't re-parse the whole install. The driver keys it
    // on game version + ruleset shape and rebuilds it when either moves.
    let vanilla_cache_auto = if *no_vanilla_cache || vanilla_cache.is_some() {
        None
    } else {
        cwtools_driver::default_cache_dir().map(|dir| VanillaCacheAuto {
            dir,
            refresh: *refresh_vanilla_cache && first_run,
        })
    };

    // Build the whole engine pipeline through the shared driver: parse
    // rules, discover/parse mod files, build the type/var/vanilla indexes,
    // expand modifier keys, build the loc index, prebuild the scope
    // registry. The CLI and LSP share this one implementation.
    //
    // A broken `.cwt` silently degrades every check below it, so the
    // ruleset's own problems are collected here and reported as ordinary
    // diagnostics rather than left on stderr for nobody to read.
    let mut rule_errors: Vec<RuleParseError> = Vec::new();
    let session = Session::load_with_parse_cache(
        SessionConfig {
            game: game_id,
            rules: RulesInput::from_path(rules.clone()),
            directory: directory.clone(),
            parent_roots: parent_roots.clone(),
            vanilla: vanilla.clone(),
            vanilla_cache: vanilla_cache_index,
            vanilla_cache_auto,
            ignore_files,
            ignore_dirs,
            loc_languages: if loc_language.is_empty() {
                None
            } else {
                Some(loc_language.clone())
            },
            case_sensitive_files: *case_sensitive_files,
            on_rules_diagnostic: Some(&mut |e: RuleParseError| rule_errors.push(e)),
        },
        cwtools_driver::default_cache_dir(),
    );
    let ruleset = session.ruleset();
    note(format!(
        "  Loaded {} types, {} enums, {} aliases",
        ruleset.types.len(),
        ruleset.enums.len(),
        ruleset.aliases.len()
    ));
    note(format!(
        "  Discovered {} files",
        session.parsed_files().len()
    ));

    // Whole-run notice, not a diagnostic: nothing in the mod is wrong, the run
    // just could not answer for these families. `complete` is the gate the
    // checks themselves read, so the two cannot drift apart. Kept on stderr for
    // every format, and carried in the report body for the two CI formats that
    // have a run-level slot for it.
    let vanilla_notice = vanilla_notice(game_id, session.type_index().complete);
    if let Some(notice) = &vanilla_notice {
        note(format!("  note: {notice}"));
    }

    // The discovered files the scope covers. Everything is still indexed, so
    // the cross-file checks are unaffected; the driver skips validating the
    // rest where it can, and the report filter below is what makes the run
    // correct either way.
    let selected = scope
        .as_ref()
        .map(|s| s.select(session.parsed_files().iter().map(|f| f.path.as_path())));
    if let Some(selected) = &selected {
        note(format!("  Reporting on {} of them", selected.len()));
    }

    // Nothing to validate is a failure, not a clean run. A failed walk
    // already exits 3 below, so don't relabel it as an empty input.
    if first_run && !session.discovery_failed && !session.discovery_incomplete {
        exit_if_empty(
            ruleset.types.len(),
            *allow_empty,
            "--rules loaded 0 types",
            rules,
        );
        exit_if_empty(
            session.parsed_files().len(),
            *allow_empty,
            "--directory contains no files to validate",
            directory,
        );
    }

    // Vanilla-cache freshness check. If both --vanilla-cache and --vanilla
    // are given we can compute the install, ruleset and replacement-view
    // fingerprint and detect staleness. THIS run already used the
    // cached data (the cache short-circuits the vanilla walk); the
    // rebuild makes the next run correct.
    let mut rebuild_incomplete = false;
    if first_run
        && let (Some(cache_path), Some(fp_loaded), Some(vanilla_dir)) =
            (vanilla_cache, &cached_fingerprint, vanilla)
    {
        rebuild_incomplete = rebuild_vanilla_cache_if_stale(
            &session,
            cache_path,
            fp_loaded,
            ModLayers {
                primary: directory,
                parents: parent_roots,
            },
            vanilla_dir,
            game_id,
            !*no_vanilla_cache,
        );
    }

    tlog!("load");

    // The driver validates files in parallel, in input order, so the
    // report is byte-for-byte identical to the sequential version.
    let want_legacy_hash = !ignored.is_empty();
    let mut sources = SourceLines::default();
    let mut diags: Vec<Diag> = Vec::new();

    let rules_unreadable = rule_errors.iter().any(|e| e.retryable_io);

    // The ruleset's own problems lead the report: they explain whatever the
    // checks below then failed to catch. Hashed against the rules root so a
    // baseline survives the ruleset being checked out elsewhere, and filtered
    // by the same code and hash policy as everything else.
    for err in rule_errors {
        if !codes::wanted(err.code, only_codes, ignore_codes)
            || !scope::wanted(scope.as_ref(), &err.file.to_string_lossy())
            || sources.inline_suppressed(&err.file.to_string_lossy(), err.line, err.code)
        {
            continue;
        }
        let line_text = sources
            .trimmed(&err.file.to_string_lossy(), err.line)
            .to_string();
        let d = rule_error_to_diag(rules, err, &line_text, want_legacy_hash);
        if is_ignored(ignored, &d) {
            continue;
        }
        diags.push(d);
    }

    let indexed_sources: HashMap<_, _> = session
        .parsed_files()
        .iter()
        .map(|file| (file.path.as_path(), file))
        .collect();
    for (path, errors) in session.validate_selected(selected.as_ref()) {
        let file_key = path_identity(&path);
        let display_file = path.display().to_string();
        if !scope::wanted(scope.as_ref(), &display_file) {
            continue;
        }
        if !errors.is_empty() {
            let source = indexed_sources
                .get(path.as_path())
                .expect("validation only returns indexed sources");
            prepare_script_source(&mut sources, &session, source, &file_key);
        }
        for err in errors {
            // Same placement as the hash baseline: a suppressed code
            // never reaches the counts, the report or --output-hashes.
            // The inline `# cwtools-ignore` directive suppresses the same
            // way, scoped to the diagnostic's own line and its neighbours.
            if !codes::wanted(err.code.unwrap_or_default(), only_codes, ignore_codes)
                || sources.inline_suppressed_path(
                    &file_key,
                    &path,
                    err.line,
                    err.code.unwrap_or_default(),
                )
            {
                continue;
            }
            let line_text = sources.trimmed_path(&file_key, &path, err.line);
            let d = validation_to_diag_with_path(
                directory,
                err,
                &path,
                display_file.clone(),
                line_text,
                want_legacy_hash,
            );
            if is_ignored(ignored, &d) {
                continue;
            }
            diags.push(d);
        }
    }
    tlog!("validate-config");

    // Loc-file checks (CW225/CW234/CW259/CW268/CW275). Resolve refs
    // against the full mod+vanilla union but only report mod-path files.
    // Ensure the prefix has a trailing separator so `/mods/MD` doesn't
    // accidentally match `/mods/MD-assets`.
    let dir_prefix = {
        let s = directory.to_string_lossy();
        if s.ends_with(std::path::MAIN_SEPARATOR) {
            s.into_owned()
        } else {
            format!("{}{}", s, std::path::MAIN_SEPARATOR)
        }
    };
    for d in session.loc_project_diagnostics() {
        if !d.file.starts_with(&dir_prefix)
            || !codes::wanted(d.code, only_codes, ignore_codes)
            || !scope::wanted(scope.as_ref(), &d.file)
            || sources.inline_suppressed(&d.file, d.line as u32, d.code)
        {
            continue;
        }
        let line_text = sources.trimmed(&d.file, d.line as u32).to_string();
        let d = loc_diagnostic_to_diag(directory, d, &line_text, want_legacy_hash);
        if is_ignored(ignored, &d) {
            continue;
        }
        diags.push(d);
    }
    tlog!("validate-loc");

    // Same placement as the ignore_hashes filter above: strip diags
    // before they reach the error/warning counts, the report, and the
    // hash output. No-op unless --min-severity was passed.
    if let Some(min_sev) = min_severity {
        diags.retain(|d| severity_rank(d.severity) >= severity_rank(*min_sev));
    }

    // Memory report (CWTOOLS_PROFILE=1): RSS at the end of a single
    // validate pass (a good proxy for peak) plus a per-component
    // breakdown, to track the 1.5 GB target and see where bytes go.
    if cwtools_profiling::profile_enabled() {
        let mib = |b: usize| cwtools_profiling::format_mib(b as u64);
        let parsed = session.parsed_files();
        let type_index = session.type_index();
        let loc_index = session.loc_index();
        let rules_table = session.string_table();
        if let Some(rss) = cwtools_profiling::current_rss_bytes() {
            eprintln!(
                "  [profile] RSS {} after validating {} files",
                cwtools_profiling::format_mib(rss),
                parsed.len()
            );
        }
        let st = rules_table.stats();
        eprintln!(
            "  [profile]   string_table: {} ({} entries, strings {}, keys {}, {} overlay entries in {} regions)",
            mib(st.total_bytes()),
            st.entries,
            mib(st.id_to_string_bytes),
            mib(st.map_key_bytes),
            st.overlay_entries,
            st.overlay_regions,
        );
        let type_instances: usize = type_index.map().values().map(|v| v.len()).sum();
        eprintln!(
            "  [profile]   parsed ASTs released after indexing ({} files)",
            parsed.len()
        );
        eprintln!(
            "  [profile]   type_index: {} instances in {} types; loc union: {} keys",
            type_instances,
            type_index.map().len(),
            loc_index.union().len()
        );
    }

    Analysis {
        diags,
        vanilla_notice,
        discovery_failed: session.discovery_failed,
        discovery_incomplete: session.is_incomplete() || rebuild_incomplete,
        rules_unreadable,
        session,
    }
}
