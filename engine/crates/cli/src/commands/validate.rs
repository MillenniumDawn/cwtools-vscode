//! `validate`: run the whole engine over a mod directory and render a report.

use cwtools_driver::{
    RulesInput, Session, SessionConfig, VanillaCacheAuto, index_game_dir,
    index_game_dir_with_parse_cache,
};
use cwtools_game::constants::Game;
use cwtools_info::vanilla_cache;
use cwtools_rules::ruleset_loader::RuleParseError;

use crate::cli::ValidateArgs;
use crate::diag::{
    Diag, SourceLines, cli_row, csv_row, is_ignored, json_row, loc_diagnostic_to_diag,
    rule_error_to_diag, severity_rank, validation_to_diag,
};
use crate::report::ReportType;
use crate::run::{
    EXIT_USAGE, announce_config, color_enabled, exit_code, exit_if_empty, load_config,
    missing_required, note, report_owns_stdout, status, vanilla_notice,
};
use crate::{codes, config, report, scope};

pub(super) fn run(args: ValidateArgs) {
    let ValidateArgs {
        config,
        game,
        directory,
        rules,
        vanilla,
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
    } = args;

    let file_cfg = load_config(config.as_deref(), directory.as_deref());
    let mut applied: Vec<&'static str> = Vec::new();
    let fc = file_cfg.as_ref();
    let game = config::pick(game, fc.and_then(|c| c.game.clone()), "game", &mut applied);
    let directory = config::pick(
        directory,
        fc.and_then(|c| c.directory.clone()),
        "directory",
        &mut applied,
    );
    let rules = config::pick(
        rules,
        fc.and_then(|c| c.rules.clone()),
        "rules",
        &mut applied,
    );
    let vanilla = config::pick(
        vanilla,
        fc.and_then(|c| c.vanilla.clone()),
        "vanilla",
        &mut applied,
    );
    let vanilla_cache = config::pick(
        vanilla_cache,
        fc.and_then(|c| c.vanilla_cache.clone()),
        "vanilla-cache",
        &mut applied,
    );
    let no_vanilla_cache = config::pick_flag(
        no_vanilla_cache,
        fc.is_some_and(|c| c.no_vanilla_cache),
        "no-vanilla-cache",
        &mut applied,
    );
    let refresh_vanilla_cache = config::pick_flag(
        refresh_vanilla_cache,
        fc.is_some_and(|c| c.refresh_vanilla_cache),
        "refresh-vanilla-cache",
        &mut applied,
    );
    let case_sensitive_files = config::pick_flag_default(
        case_sensitive_files,
        fc.and_then(|c| c.case_sensitive_files),
        "case-sensitive-files",
        &mut applied,
    );
    let report_type = config::pick(
        report_type,
        fc.and_then(|c| c.report_type),
        "report-type",
        &mut applied,
    )
    .unwrap_or(ReportType::Cli);
    let min_severity = config::pick(
        min_severity,
        fc.and_then(|c| c.min_severity),
        "min-severity",
        &mut applied,
    );
    let fail_on = config::pick(fail_on, fc.and_then(|c| c.fail_on), "fail-on", &mut applied)
        .unwrap_or_default();
    let ignore_files = config::pick_list(
        ignore_files,
        fc.map(|c| c.ignore_files.clone()).unwrap_or_default(),
        "ignore-files",
        &mut applied,
    );
    let ignore_dirs = config::pick_list(
        ignore_dirs,
        fc.map(|c| c.ignore_dirs.clone()).unwrap_or_default(),
        "ignore-dirs",
        &mut applied,
    );
    let loc_language = config::pick_list(
        loc_language,
        fc.map(|c| c.loc_languages.clone()).unwrap_or_default(),
        "loc-languages",
        &mut applied,
    );
    let ignore_codes = config::pick_list(
        ignore_codes,
        fc.map(|c| c.ignore_codes.clone()).unwrap_or_default(),
        "ignore-codes",
        &mut applied,
    );
    let only_codes = config::pick_list(
        only_codes,
        fc.map(|c| c.only_codes.clone()).unwrap_or_default(),
        "only-codes",
        &mut applied,
    );
    let allow_empty = config::pick_flag(
        allow_empty,
        fc.is_some_and(|c| c.allow_empty),
        "allow-empty",
        &mut applied,
    );
    announce_config("validate", fc, &applied, config::VALIDATE_KEYS);

    let game = game.unwrap_or_else(|| missing_required("validate", "--game <GAME>", "game", fc));
    let directory = directory.unwrap_or_else(|| {
        missing_required("validate", "--directory <DIRECTORY>", "directory", fc)
    });
    let rules =
        rules.unwrap_or_else(|| missing_required("validate", "--rules <RULES>", "rules", fc));

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

    let game_id = Game::from_str(&game).unwrap_or_else(|| {
        eprintln!("Unknown game: {}. Supported: hoi4, stellaris, eu4, ck2, ck3, vic2, vic3, ir, eu5, custom", game);
        std::process::exit(1);
    });
    let parse_cache_game = game_id.to_string();

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
    let vanilla_cache_index = vanilla_cache.as_ref().and_then(|cache_path| {
        match vanilla_cache::load(cache_path) {
            Ok((cache_game, cached_fp, data)) => {
                if cache_game != game {
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
        }
    });
    let (cached_fingerprint, vanilla_cache_index) = vanilla_cache_index.unzip();

    // Without an explicit --vanilla-cache, keep one under the OS cache dir
    // so repeat runs don't re-parse the whole install. The driver keys it
    // on game version + ruleset shape and rebuilds it when either moves.
    let vanilla_cache_auto = if no_vanilla_cache || vanilla_cache.is_some() {
        None
    } else {
        cwtools_driver::default_cache_dir().map(|dir| VanillaCacheAuto {
            dir,
            refresh: refresh_vanilla_cache,
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
            vanilla: vanilla.clone(),
            vanilla_cache: vanilla_cache_index,
            vanilla_cache_auto,
            ignore_files: &ignore_files,
            ignore_dirs: &ignore_dirs,
            loc_languages: if loc_language.is_empty() {
                None
            } else {
                Some(loc_language)
            },
            case_sensitive_files,
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
    if !session.discovery_failed {
        exit_if_empty(
            ruleset.types.len(),
            allow_empty,
            "--rules loaded 0 types",
            &rules,
        );
        exit_if_empty(
            session.parsed_files().len(),
            allow_empty,
            "--directory contains no files to validate",
            &directory,
        );
    }

    // Vanilla-cache freshness check. If both --vanilla-cache and --vanilla
    // are given we can compute the combined fingerprint (game version +
    // ruleset shape) and detect staleness. THIS run already used the
    // cached data (the cache short-circuits the vanilla walk); the
    // rebuild makes the next run correct.
    if let (Some(cache_path), Some(fp_loaded), Some(vanilla_dir)) =
        (&vanilla_cache, &cached_fingerprint, &vanilla)
    {
        let fp_live = vanilla_cache::combined_fingerprint(vanilla_dir, ruleset);
        if *fp_loaded != fp_live {
            eprintln!(
                "  warn: vanilla cache is stale (cached: {}, live: {}); rebuilding",
                fp_loaded, fp_live
            );
            let rules_table = session.string_table();
            let var_effects = cwtools_info::variable_defining_effects(ruleset);
            let index = if no_vanilla_cache {
                index_game_dir(vanilla_dir, ruleset, rules_table, &var_effects)
            } else if let Some(cache_dir) = cwtools_driver::default_cache_dir() {
                index_game_dir_with_parse_cache(
                    vanilla_dir,
                    ruleset,
                    rules_table,
                    &var_effects,
                    &cache_dir,
                    &parse_cache_game,
                )
            } else {
                index_game_dir(vanilla_dir, ruleset, rules_table, &var_effects)
            };
            let aux = cwtools_driver::build_vanilla_cache_aux(vanilla_dir, &index);
            match vanilla_cache::save(&index, &game, &fp_live, cache_path, aux) {
                Ok(n) => note(format!("  Rebuilt vanilla cache with {} instances", n)),
                Err(e) => eprintln!(
                    "  warn: could not write rebuilt cache {}: {}",
                    cache_path.display(),
                    e
                ),
            }
        }
    }

    tlog!("load");

    // Load the ignore-hash baseline, if given.
    let ignored: std::collections::HashSet<String> = ignore_hashes
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| {
            s.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default();

    // The driver validates files in parallel, in input order, so the
    // report is byte-for-byte identical to the sequential version.
    let want_legacy_hash = !ignored.is_empty();
    let mut sources = SourceLines::default();
    let mut diags: Vec<Diag> = Vec::new();

    // The ruleset's own problems lead the report: they explain whatever the
    // checks below then failed to catch. Hashed against the rules root so a
    // baseline survives the ruleset being checked out elsewhere, and filtered
    // by the same code and hash policy as everything else.
    for err in rule_errors {
        if !codes::wanted(err.code, &only_codes, &ignore_codes)
            || !scope::wanted(scope.as_ref(), &err.file.to_string_lossy())
            || sources.inline_suppressed(&err.file.to_string_lossy(), err.line, err.code)
        {
            continue;
        }
        let line_text = sources
            .trimmed(&err.file.to_string_lossy(), err.line)
            .to_string();
        let d = rule_error_to_diag(&rules, err, &line_text, want_legacy_hash);
        if is_ignored(&ignored, &d) {
            continue;
        }
        diags.push(d);
    }

    for (path, errors) in session.validate_selected(selected.as_ref()) {
        let file_str = path.to_str().unwrap_or("");
        if !scope::wanted(scope.as_ref(), file_str) {
            continue;
        }
        for err in errors {
            // Same placement as the hash baseline: a suppressed code
            // never reaches the counts, the report or --output-hashes.
            // The inline `# cwtools-ignore` directive suppresses the same
            // way, scoped to the diagnostic's own line and its neighbours.
            if !codes::wanted(err.code.unwrap_or_default(), &only_codes, &ignore_codes)
                || sources.inline_suppressed(file_str, err.line, err.code.unwrap_or_default())
            {
                continue;
            }
            let line_text = sources.trimmed(file_str, err.line);
            let d = validation_to_diag(&directory, err, line_text, want_legacy_hash);
            if is_ignored(&ignored, &d) {
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
            || !codes::wanted(d.code, &only_codes, &ignore_codes)
            || !scope::wanted(scope.as_ref(), &d.file)
            || sources.inline_suppressed(&d.file, d.line as u32, d.code)
        {
            continue;
        }
        let line_text = sources.trimmed(&d.file, d.line as u32).to_string();
        let d = loc_diagnostic_to_diag(&directory, d, &line_text, want_legacy_hash);
        if is_ignored(&ignored, &d) {
            continue;
        }
        diags.push(d);
    }
    tlog!("validate-loc");

    // Same placement as the ignore_hashes filter above: strip diags
    // before they reach the error/warning counts, the report, and the
    // hash output. No-op unless --min-severity was passed.
    if let Some(min_sev) = min_severity {
        diags.retain(|d| severity_rank(d.severity) >= severity_rank(min_sev));
    }

    let total_errors = diags
        .iter()
        .filter(|d| d.severity == cwtools_validation::ErrorSeverity::Error)
        .count();
    let total_warnings = diags
        .iter()
        .filter(|d| d.severity == cwtools_validation::ErrorSeverity::Warning)
        .count();

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
            "  [profile]   string_table: {} ({} entries, strings {}, keys {})",
            mib(st.total_bytes()),
            st.entries,
            mib(st.id_to_string_bytes),
            mib(st.map_key_bytes),
        );
        let type_instances: usize = type_index.map.values().map(|v| v.len()).sum();
        eprintln!(
            "  [profile]   parsed ASTs released after indexing ({} files)",
            parsed.len()
        );
        eprintln!(
            "  [profile]   type_index: {} instances in {} types; loc union: {} keys",
            type_instances,
            type_index.map.len(),
            loc_index.union().len()
        );
    }

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
            // cli: grouped by file
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

    let write_failed = match &output_file {
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

    let failing = fail_on.failing(diags.iter().map(|d| d.severity));
    let code = exit_code(failing, session.discovery_failed, write_failed);
    if code != 0 {
        std::process::exit(code);
    }
}
