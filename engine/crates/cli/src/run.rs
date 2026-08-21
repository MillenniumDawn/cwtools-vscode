//! Plumbing every subcommand shares: exit codes, `cwtools.toml` resolution and
//! reporting, and the status lines that have to keep out of a redirected report.

use clap::CommandFactory;
use cwtools_game::constants::Game;
use cwtools_validation::ErrorSeverity;
use std::path::{Path, PathBuf};

use crate::cli::Cli;
use crate::config;
use crate::diag::severity_rank;
use crate::report::ReportType;

/// The file walk itself failed (the path doesn't resolve, a dir is unreadable).
pub(crate) const EXIT_DISCOVERY_FAILED: i32 = 3;

/// An input resolved to nothing: a ruleset with no types, or a target directory
/// with no files. Distinct from [`EXIT_DISCOVERY_FAILED`] so CI can tell
/// "nothing to check" from "the walk errored".
pub(crate) const EXIT_EMPTY_INPUT: i32 = 4;

/// An input the run couldn't act on: a `cwtools.toml` that wouldn't read or
/// parse, a `--since` ref git couldn't resolve. Shares clap's usage-error code,
/// since the run never started and so has no validation result to report.
pub(crate) const EXIT_USAGE: i32 = 2;

/// Resolve the run's config file, failing loudly on a broken one. `anchor` is
/// the directory the upward search starts from when `--config` wasn't given.
pub(crate) fn load_config(
    explicit: Option<&Path>,
    anchor: Option<&Path>,
) -> Option<config::FileConfig> {
    config::resolve(explicit, anchor).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(EXIT_USAGE);
    })
}

// ── Output style (--quiet / --no-color) ──────────────────────────────────────
//
// Both are global flags read once at startup rather than threaded through every
// command: they shape lines emitted from a dozen places across four subcommands,
// and no call site wants a decision about them.

static QUIET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static NO_COLOR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record the run's `--quiet` / `--no-color`. Called once, before dispatch.
pub(crate) fn set_output_style(quiet: bool, no_color: bool) {
    use std::sync::atomic::Ordering::Relaxed;
    QUIET.store(quiet, Relaxed);
    NO_COLOR.store(no_color, Relaxed);
}

/// A progress or summary line, dropped under `--quiet`. Warnings and errors go
/// straight to `eprintln!` instead: a quiet run still has to say what it could
/// not do, or a typo'd path reads as a clean one.
pub(crate) fn note(line: impl AsRef<str>) {
    if !QUIET.load(std::sync::atomic::Ordering::Relaxed) {
        eprintln!("{}", line.as_ref());
    }
}

/// Whether the `cli` report should carry ANSI color. Off unless it is going to
/// a terminal, so a redirected report, an `--output-file` and every CI log stay
/// byte-for-byte what they were before color existed. `--no-color` and a
/// non-empty `NO_COLOR` each force it off.
pub(crate) fn color_enabled(to_stdout: bool) -> bool {
    use std::io::IsTerminal;
    to_stdout
        && !NO_COLOR.load(std::sync::atomic::Ordering::Relaxed)
        && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
        && std::io::stdout().is_terminal()
}

/// Whether stdout is carrying one of the CI report formats, in which case status
/// lines have to go to stderr instead: `cwtools loc . --report-type sarif >
/// out.sarif` must not have a progress banner in the middle of the JSON. Only
/// the two new formats divert — cli, csv and json keep every line where it was.
pub(crate) fn report_owns_stdout(report_type: ReportType, output_file: Option<&PathBuf>) -> bool {
    output_file.is_none() && matches!(report_type, ReportType::Github | ReportType::Sarif)
}

/// A progress/status line, diverted to stderr when the report owns stdout.
/// Chatter like [`note`], so `--quiet` drops it wherever it was headed.
pub(crate) fn status(line: String, to_stderr: bool) {
    if QUIET.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if to_stderr {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
}

/// Report which config file the run used and what it contributed, on stderr so
/// a redirected report stays clean. `reads` is the running subcommand's key set:
/// anything the file sets outside it is named, since a key that quietly does
/// nothing is the failure mode a shared config invites.
pub(crate) fn announce_config(
    subcommand: &str,
    cfg: Option<&config::FileConfig>,
    applied: &[&'static str],
    reads: &[&str],
) {
    let Some(cfg) = cfg else { return };
    let what = if applied.is_empty() {
        "no settings applied".to_string()
    } else {
        format!("applied: {}", applied.join(", "))
    };
    note(format!("Using config {} ({})", cfg.path.display(), what));
    let unread: Vec<&str> = cfg
        .present
        .iter()
        .copied()
        .filter(|k| !reads.contains(k))
        .collect();
    if !unread.is_empty() {
        eprintln!(
            "warn: {} sets {}, which `{subcommand}` does not read",
            cfg.path.display(),
            unread.join(", ")
        );
    }
}

/// The run-level notice for a validate that loaded no base-game index, or
/// `None` when it did. A report is read as "nothing wrong here", so the checks
/// that could not run have to be named rather than left to look clean.
pub(crate) fn vanilla_notice(game: Game, has_vanilla: bool) -> Option<String> {
    let codes = cwtools_driver::vanilla_gated_checks(game, has_vanilla);
    if codes.is_empty() {
        return None;
    }
    Some(format!(
        "no base-game data loaded, so {} report nothing; pass --vanilla or --vanilla-cache to run them",
        codes.join(", ")
    ))
}

/// Bail on a setting that neither a flag nor the config file supplied, through
/// clap so the message, the usage line and the exit code match every other
/// usage error.
pub(crate) fn missing_required(
    subcommand: &str,
    arg: &str,
    key: &str,
    cfg: Option<&config::FileConfig>,
) -> ! {
    let hint = match cfg {
        Some(c) => format!("{} does not set `{key}`", c.path.display()),
        None => format!(
            "no {} was found; `{key}` could come from one",
            config::FILE_NAME
        ),
    };
    let kind = clap::error::ErrorKind::MissingRequiredArgument;
    let message = format!("the following required argument was not provided: {arg}\n\n  {hint}");
    let mut root = Cli::command();
    if let Some(sub) = root.find_subcommand_mut(subcommand) {
        sub.error(kind, message).exit()
    }
    root.error(kind, message).exit()
}

/// How severe a surviving diagnostic has to be for the run to fail. `Never` is
/// the report-only case: publish the findings and leave the build green.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum FailOn {
    Never,
    AtLeast(ErrorSeverity),
}

impl Default for FailOn {
    /// Errors fail the run, which is what every release before `--fail-on` did.
    fn default() -> Self {
        FailOn::AtLeast(ErrorSeverity::Error)
    }
}

impl FailOn {
    /// How many of `severities` trip the gate.
    pub(crate) fn failing(self, severities: impl Iterator<Item = ErrorSeverity>) -> usize {
        match self {
            FailOn::Never => 0,
            FailOn::AtLeast(min) => severities
                .filter(|s| severity_rank(*s) >= severity_rank(min))
                .count(),
        }
    }
}

/// Parse a `--fail-on` value, for clap's `value_parser`. `none` is spelled out
/// rather than left to `--fail-on` with no value: a flag that silently disarms
/// the exit code is worth reading in a CI log.
pub(crate) fn parse_fail_on(s: &str) -> Result<FailOn, String> {
    if s.eq_ignore_ascii_case("none") {
        return Ok(FailOn::Never);
    }
    crate::cli::parse_min_severity(s)
        .map(FailOn::AtLeast)
        .map_err(|_| {
            format!("invalid severity '{s}': valid values are error, warning, info, hint, none")
        })
}

/// Map a run's outcome to a process exit code. Operational failures (couldn't
/// discover the files, couldn't write the report) are distinct from validation
/// finding errors, so CI can tell "the tool couldn't run" apart from "validation
/// found problems". `failing` is the count `--fail-on` selected, which defaults
/// to the errors. 0 = clean run, nothing reached the gate.
pub(crate) fn exit_code(failing: usize, discovery_failed: bool, write_failed: bool) -> i32 {
    if discovery_failed {
        EXIT_DISCOVERY_FAILED
    } else if write_failed {
        2
    } else if failing > 0 {
        1
    } else {
        0
    }
}

/// A path as the run resolved it: absolute where that can be computed, so an
/// error names the location a relative CI path actually pointed at.
pub(crate) fn resolved_path(path: &std::path::Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

/// Message for an input that resolved to nothing. `what` names the input and
/// what came back empty; the path is what the run resolved it to.
fn empty_input_error(what: &str, path: &std::path::Path) -> String {
    format!(
        "error: {what}: {} (nothing to check; pass --allow-empty if this is intended)",
        resolved_path(path)
    )
}

/// Fail loudly when an input resolved to nothing. Validating against an empty
/// ruleset, or over an empty file set, reports "0 errors" and exits 0, which
/// leaves a CI job with a typo'd path permanently green.
pub(crate) fn exit_if_empty(count: usize, allow_empty: bool, what: &str, path: &std::path::Path) {
    if count == 0 && !allow_empty {
        eprintln!("{}", empty_input_error(what, path));
        std::process::exit(EXIT_EMPTY_INPUT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_separates_operational_from_validation() {
        assert_eq!(exit_code(0, false, false), 0); // clean
        assert_eq!(exit_code(5, false, false), 1); // validation errors
        assert_eq!(exit_code(0, false, true), 2); // report write failed
        assert_eq!(exit_code(0, true, false), 3); // discovery failed
        // operational failures take precedence over validation errors
        assert_eq!(exit_code(5, false, true), 2);
        assert_eq!(exit_code(5, true, true), 3);
    }

    /// Every severity, so a gate test reads as the whole range rather than a
    /// pair that happens to straddle it.
    const ALL: [ErrorSeverity; 4] = [
        ErrorSeverity::Error,
        ErrorSeverity::Warning,
        ErrorSeverity::Information,
        ErrorSeverity::Hint,
    ];

    /// A report bound for a file is never colored, whatever the terminal is
    /// doing — the escapes would land in the artifact. The terminal path is
    /// only reachable interactively, so this is the half a test can hold.
    #[test]
    fn a_report_written_to_a_file_is_never_colored() {
        assert!(!color_enabled(false));
    }

    #[test]
    fn fail_on_defaults_to_counting_errors() {
        assert_eq!(FailOn::default(), FailOn::AtLeast(ErrorSeverity::Error));
        assert_eq!(FailOn::default().failing(ALL.into_iter()), 1);
    }

    #[test]
    fn fail_on_counts_everything_at_or_above_the_level() {
        let count = |f: FailOn| f.failing(ALL.into_iter());
        assert_eq!(count(FailOn::AtLeast(ErrorSeverity::Warning)), 2);
        assert_eq!(count(FailOn::AtLeast(ErrorSeverity::Information)), 3);
        assert_eq!(count(FailOn::AtLeast(ErrorSeverity::Hint)), 4);
    }

    #[test]
    fn fail_on_none_never_trips() {
        assert_eq!(FailOn::Never.failing(ALL.into_iter()), 0);
        assert_eq!(
            exit_code(FailOn::Never.failing(ALL.into_iter()), false, false),
            0
        );
    }

    #[test]
    fn parse_fail_on_takes_the_severities_and_none() {
        assert_eq!(parse_fail_on("none").unwrap(), FailOn::Never);
        assert_eq!(parse_fail_on("NONE").unwrap(), FailOn::Never);
        assert_eq!(
            parse_fail_on("warning").unwrap(),
            FailOn::AtLeast(ErrorSeverity::Warning)
        );
        let e = parse_fail_on("critical").unwrap_err();
        assert!(e.contains("critical") && e.contains("none"), "got: {e}");
    }

    #[test]
    fn vanilla_notice_is_silent_with_a_base_game_index() {
        assert_eq!(vanilla_notice(Game::Hoi4, true), None);
        assert_eq!(vanilla_notice(Game::Stellaris, true), None);
    }

    #[test]
    fn vanilla_notice_names_the_disabled_checks_and_the_flags() {
        let msg = vanilla_notice(Game::Hoi4, false).expect("notice without vanilla data");
        assert!(msg.contains("CW113, CW222, CW500"), "got: {msg}");
        assert!(msg.contains("--vanilla-cache"), "got: {msg}");
        // Stellaris adds the ship-design and planet-killer families.
        let stl = vanilla_notice(Game::Stellaris, false).expect("notice without vanilla data");
        assert!(stl.contains("CW227, CW229"), "got: {stl}");
    }

    #[test]
    fn empty_input_error_names_the_input_and_resolved_path() {
        let msg = empty_input_error("--rules loaded 0 types", std::path::Path::new("."));
        assert!(msg.contains("--rules loaded 0 types"), "got: {msg}");
        assert!(msg.contains("--allow-empty"), "got: {msg}");
        // The path is absolutized, so a relative CI path is identifiable.
        let here = std::path::absolute(".").unwrap().display().to_string();
        assert!(msg.contains(&here), "got: {msg}");
    }
}
