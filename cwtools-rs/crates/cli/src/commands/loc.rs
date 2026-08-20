//! `loc`: the standalone localisation lint over a directory of `.yml` files.

use std::path::Path;

use cwtools_driver::RulesInput;
use cwtools_game::constants::Game;
use cwtools_localization::{LocScopeData, validate_loc_project_commands};
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::build_scope_registry_arc;

use crate::cli::LocArgs;
use crate::diag::{
    Diag, SourceLines, csv_row, is_ignored, json_row, loc_diagnostic_to_diag,
    loc_parse_error_to_diag, severity_rank,
};
use crate::report::ReportType;
use crate::run::{
    EXIT_DISCOVERY_FAILED, announce_config, exit_code, exit_if_empty, load_config,
    missing_required, note, report_owns_stdout, resolved_path, status,
};
use crate::{codes, config, report};

pub(super) fn run(args: LocArgs) {
    let LocArgs {
        directory,
        config,
        game,
        rules,
        report_type,
        output_file,
        ignore_hashes,
        output_hashes,
        ignore_files,
        ignore_dirs,
        loc_language,
        min_severity,
        fail_on,
        ignore_codes,
        only_codes,
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
    announce_config("loc", fc, &applied, config::LOC_KEYS);
    let directory =
        directory.unwrap_or_else(|| missing_required("loc", "<DIRECTORY>", "directory", fc));

    // A path that doesn't resolve is never a clean run, and --allow-empty
    // doesn't excuse it (that flag covers a deliberately empty scan).
    if !directory.is_dir() {
        eprintln!(
            "error: directory does not exist: {}",
            resolved_path(&directory)
        );
        std::process::exit(EXIT_DISCOVERY_FAILED);
    }

    // Rules are optional. Without them `loc` stays the scope-independent lint it
    // has always been; with --game and --rules the ruleset's scope registry
    // comes up too and the command checks (CW226/CW260/CW266) run alongside it.
    // Only the ruleset is read — no game files are discovered or indexed.
    let scope_data = loc_scope_data(game.as_deref(), rules.as_deref());

    let divert = report_owns_stdout(report_type, output_file.as_ref());
    status(
        format!("Scanning localisation in {}", directory.display()),
        divert,
    );
    let langs = (!loc_language.is_empty()).then_some(loc_language.as_slice());
    let service =
        cwtools_driver::load_loc_service(&[&directory], langs, &ignore_files, &ignore_dirs);
    exit_if_empty(
        service.files().len(),
        allow_empty,
        "no localisation files found under",
        &directory,
    );

    let total_entries: usize = service.files().iter().map(|f| f.entries.len()).sum();

    // Load the ignore-hash baseline, if given. Same placement as
    // `validate`: diagnostics are dropped before the report is
    // rendered and before they're counted for the exit code.
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

    // The scope-independent checks (CW225 etc.) always run. The command checks
    // follow when a ruleset supplied a scope registry; with no reference site to
    // seed them they start every chain at `any`, so they report what is wrong in
    // every scope rather than what is wrong where the key is used.
    let want_legacy_hash = !ignored.is_empty();
    let mut sources = SourceLines::default();
    let keep = |d: &Diag| {
        !is_ignored(&ignored, d)
            && min_severity.is_none_or(|m| severity_rank(d.severity) >= severity_rank(m))
    };
    let mut loc_diags = cwtools_localization::validate_loc_project_scoped(
        &service,
        langs,
        &std::collections::HashSet::new(),
    );
    if let Some(data) = &scope_data {
        loc_diags.extend(validate_loc_project_commands(&service, langs, data));
    }
    let diags: Vec<Diag> = loc_diags
        .into_iter()
        .filter(|d| codes::wanted(d.code, &only_codes, &ignore_codes))
        .map(|d| {
            let line_text = sources.trimmed(&d.file, d.line as u32).to_string();
            loc_diagnostic_to_diag(&directory, d, &line_text, want_legacy_hash)
        })
        .filter(keep)
        .collect();

    // Surface parse failures too (files that couldn't even be
    // lenient-parsed), kept separate from `diags` since they carry no
    // line/code and get their own text-report line below.
    // Neither code filter touches these: they carry no code to name, and
    // dropping a file the parser couldn't read at all would exit 0 on a
    // broken mod. Suppression is per code; these have none.
    let parse_errors: Vec<Diag> = service
        .errors()
        .iter()
        .map(|(file, message)| {
            loc_parse_error_to_diag(&directory, file.clone(), message.clone(), want_legacy_hash)
        })
        .filter(keep)
        .collect();

    let total_issues = diags.len() + parse_errors.len();

    // Render the report in the requested format. The `cli` default
    // reproduces the original hand-rolled text report byte-for-byte;
    // csv/json reuse the same row helpers `validate` uses.
    let mut out = String::new();
    match report_type {
        ReportType::Csv => {
            out.push_str("file,line,severity,code,message,hash\n");
            for d in diags.iter().chain(parse_errors.iter()) {
                out.push_str(&csv_row(d));
            }
        }
        ReportType::Json => {
            let all: Vec<&Diag> = diags.iter().chain(parse_errors.iter()).collect();
            out.push_str("[\n");
            for (i, d) in all.iter().enumerate() {
                out.push_str(&json_row(d, i + 1 >= all.len()));
            }
            out.push_str("]\n");
        }
        ReportType::Github => {
            let root = report::report_root();
            for d in diags.iter().chain(parse_errors.iter()) {
                out.push_str(&report::github_row(d, &root));
            }
        }
        ReportType::Sarif => {
            let all: Vec<&Diag> = diags.iter().chain(parse_errors.iter()).collect();
            out.push_str(&report::sarif_report(&all, &report::report_root(), None));
        }
        ReportType::Cli => {
            let mut by_file: std::collections::BTreeMap<&str, Vec<&Diag>> =
                std::collections::BTreeMap::new();
            for d in &diags {
                by_file.entry(&d.file).or_default().push(d);
            }
            for (file, ds) in &by_file {
                out.push_str(&format!("\n  {} — {} issues:\n", file, ds.len()));
                for d in ds {
                    out.push_str(&format!(
                        "    [line {}] {}: {}\n",
                        d.line, d.code, d.message
                    ));
                }
            }
            for d in &parse_errors {
                out.push_str(&format!("\n  {} — PARSE ERROR: {}\n", d.file, d.message));
            }
            out.push_str(&format!(
                "\nLoc validation complete: {} entries, {} issues\n",
                total_entries, total_issues
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
                        "Wrote {} report ({} issues) to {}",
                        report_type.as_str(),
                        total_issues,
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
        let mut hashes: Vec<&str> = diags
            .iter()
            .chain(parse_errors.iter())
            .map(|d| d.hash.as_str())
            .collect();
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

    // Severity-aware like `validate`: a parse failure is always an error, while
    // a lint diagnostic only counts once it reaches the gate, so e.g. an
    // Information-severity CW234 placeholder doesn't fail CI by default.
    let failing = fail_on.failing(diags.iter().chain(&parse_errors).map(|d| d.severity));
    let code = exit_code(failing, false, write_failed);
    if code != 0 {
        std::process::exit(code);
    }
}

/// Build the loc scope settings from `--game` and `--rules`, or `None` when the
/// run didn't ask for the scope checks. Loads the ruleset for its scope and link
/// definitions only; the ruleset's own problems are counted, not reported, since
/// a loc report has no place to put them (`cwtools rules` does).
///
/// Both settings are needed: the registry is per-game, and a ruleset without a
/// game has no scopes to build it from. One on its own warns and checks nothing,
/// which keeps a `cwtools.toml` written for `validate` usable here.
///
/// No scripted-variable registry: this lint reads the `.yml` files and the
/// ruleset, and never walks the game files a variable index is collected from.
/// A half-built registry would report every mod-set variable as undefined, so
/// the probe is withheld and multi-segment chains stay lenient, the same way an
/// unscanned workspace behaves in `validate`. Hence `'static`: nothing borrowed.
fn loc_scope_data(game: Option<&str>, rules: Option<&Path>) -> Option<LocScopeData<'static>> {
    let (game, rules) = match (game, rules) {
        (Some(game), Some(rules)) => (game, rules),
        (None, None) => return None,
        (game, _) => {
            let given = if game.is_some() { "--game" } else { "--rules" };
            eprintln!(
                "warn: the loc scope checks (CW226/CW260/CW266) need both --game and --rules; \
                 {given} on its own does nothing"
            );
            return None;
        }
    };
    let Some(game) = Game::from_str(game) else {
        eprintln!(
            "Unknown game: {game}. Supported: hoi4, stellaris, eu4, ck2, ck3, vic2, vic3, ir, \
             eu5, custom"
        );
        std::process::exit(1);
    };

    let table = StringTable::new();
    let (ruleset, problems) =
        cwtools_driver::load_rules(&RulesInput::from_path(rules.to_path_buf()), &table)
            .unwrap_or_else(|e| {
                eprintln!("error: {e}");
                std::process::exit(EXIT_DISCOVERY_FAILED);
            });
    note(format!(
        "Loaded {} scopes, {} links and {} loc commands from {} for the {game} loc command checks",
        ruleset.scope_inputs.len(),
        ruleset.link_inputs.len(),
        ruleset.localisation_commands.len(),
        rules.display()
    ));
    if !problems.is_empty() {
        eprintln!(
            "warn: the ruleset has {} problems; run `cwtools rules {}` for them",
            problems.len(),
            rules.display()
        );
    }
    Some(LocScopeData {
        game: Some(game),
        terminal_commands: ruleset.localisation_commands.iter().cloned().collect(),
        registry: build_scope_registry_arc(&ruleset, Some(game)),
        ..Default::default()
    })
}
