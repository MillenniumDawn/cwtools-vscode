//! `rules`: parse a `.cwt` file or directory, print a summary, and report what
//! is wrong with the ruleset itself. Owns the rules loader and the summary
//! printer, which `parse` and `cache` reuse.

use cwtools_driver::RulesInput;
use cwtools_rules::rules_types::RuleSet;
use cwtools_rules::ruleset_loader::RuleParseError;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::ErrorSeverity;
use std::path::{Path, PathBuf};

use crate::diag::{Diag, SourceLines, cli_row, csv_row, json_row, rule_error_to_diag};
use crate::report::{self, ReportType};
use crate::run::{FailOn, color_enabled, exit_code, status};

pub(super) fn run(file: PathBuf, report_type: Option<ReportType>, fail_on: Option<FailOn>) {
    let report_type = report_type.unwrap_or(ReportType::Cli);
    let fail_on = fail_on.unwrap_or_default();
    let table = StringTable::new();
    let (ruleset, errors) = load_rules_reporting(&file, &table);

    // The summary is the human half of this command, so it gives way to stderr
    // whenever a machine-readable report is the thing being consumed.
    let divert = report_type != ReportType::Cli;
    let label = if file.is_dir() {
        format!("rule directory: {}", file.display())
    } else {
        format!("rules file: {}", file.display())
    };
    status(format!("Parsed {}", label), divert);
    print_ruleset_summary(&ruleset, divert);

    let mut sources = SourceLines::default();
    let diags: Vec<Diag> = errors
        .into_iter()
        .map(|err| {
            let line_text = sources
                .trimmed(&err.file.to_string_lossy(), err.line)
                .to_string();
            rule_error_to_diag(&file, err, &line_text, false)
        })
        .collect();
    let total_errors = diags
        .iter()
        .filter(|d| d.severity == ErrorSeverity::Error)
        .count();
    let total_warnings = diags
        .iter()
        .filter(|d| d.severity == ErrorSeverity::Warning)
        .count();

    print!(
        "{}",
        render(
            report_type,
            &diags,
            total_errors,
            total_warnings,
            color_enabled(true)
        )
    );

    let code = exit_code(
        fail_on.failing(diags.iter().map(|d| d.severity)),
        false,
        false,
    );
    if code != 0 {
        std::process::exit(code);
    }
}

/// Render the rules problems in the requested format. Same row helpers, and the
/// same grouped-by-file `cli` shape, as the `validate` report.
fn render(
    report_type: ReportType,
    diags: &[Diag],
    total_errors: usize,
    total_warnings: usize,
    color: bool,
) -> String {
    let mut out = String::new();
    match report_type {
        ReportType::Csv => {
            out.push_str("file,line,severity,code,message,hash\n");
            for d in diags {
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
            for d in diags {
                out.push_str(&report::github_row(d, &root));
            }
        }
        ReportType::Sarif => {
            let refs: Vec<&Diag> = diags.iter().collect();
            out.push_str(&report::sarif_report(&refs, &report::report_root(), None));
        }
        ReportType::Cli => {
            let mut current = "";
            for d in diags {
                if &*d.file != current {
                    out.push_str(&format!("\n  {}:\n", d.file));
                    current = &d.file;
                }
                out.push_str(&cli_row(d, color));
            }
            out.push_str(&format!(
                "\nRules check complete: {} errors, {} warnings\n",
                total_errors, total_warnings
            ));
        }
    }
    out
}

/// Load a RuleSet and hand back the ruleset's own problems for the caller to
/// report. A rules path that can't be read at all is fatal: there is no ruleset
/// left to say anything about.
fn load_rules_reporting(rules_path: &Path, table: &StringTable) -> (RuleSet, Vec<RuleParseError>) {
    cwtools_driver::load_rules(&RulesInput::from_path(rules_path.to_path_buf()), table)
        .unwrap_or_else(|e| {
            eprintln!("Error loading rules: {}", e);
            std::process::exit(1);
        })
}

/// Load a RuleSet, printing any rules problems on stderr. For the subcommands
/// whose output is a ruleset rather than a report; `rules` reports them instead.
pub(super) fn load_rules(rules_path: &Path, table: &StringTable) -> RuleSet {
    let (ruleset, errors) = load_rules_reporting(rules_path, table);
    for err in &errors {
        eprintln!("warn: {}", err);
    }
    ruleset
}

/// Print a compact summary of a loaded RuleSet. Shared by the Parse-on-directory
/// and Rules subcommands (previously copy-pasted between them). `to_stderr`
/// keeps it out of a report that owns stdout.
pub(super) fn print_ruleset_summary(ruleset: &RuleSet, to_stderr: bool) {
    let mut out = format!("  Types:         {}\n", ruleset.types.len());
    for t in &ruleset.types {
        out.push_str(&format!(
            "    - {} (path: {:?}, subtypes: {})\n",
            t.name,
            t.path_options.paths,
            t.subtypes.len()
        ));
    }
    out.push_str(&format!("  Enums:         {}\n", ruleset.enums.len()));
    for e in &ruleset.enums {
        out.push_str(&format!("    - {} ({} values)\n", e.key, e.values.len()));
    }
    out.push_str(&format!("  Aliases:       {}\n", ruleset.aliases.len()));
    out.push_str(&format!(
        "  SingleAliases: {}\n",
        ruleset.single_aliases.len()
    ));
    out.push_str(&format!(
        "  ComplexEnums:  {}\n",
        ruleset.complex_enums.len()
    ));
    if to_stderr {
        eprint!("{out}");
    } else {
        print!("{out}");
    }
}
