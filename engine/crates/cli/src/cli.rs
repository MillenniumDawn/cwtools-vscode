//! The clap surface: every subcommand, flag and value parser the binary
//! accepts. Execution lives in `commands`.

use clap::{Args, Parser, Subcommand};
use cwtools_localization::Lang;
use cwtools_validation::ErrorSeverity;
use std::path::PathBuf;

use crate::codes;
use crate::report::{self, ReportType};
use crate::run::{self, FailOn};

#[derive(Parser)]
#[command(name = "cwtools")]
#[command(about = "CWTools CLI — Paradox mod tooling")]
// From CARGO_PKG_VERSION, the same source `cwtools-server --version` prints.
#[command(version)]
pub(crate) struct Cli {
    /// Suppress progress and summary chatter on stderr. Warnings, errors and
    /// the report itself are unaffected, so a quiet run still says what it
    /// could not do. Accepted on any subcommand.
    #[arg(long, short, global = true)]
    pub(crate) quiet: bool,
    /// Never colorize the `cli` report. Color is off already unless stdout is
    /// a terminal, so a redirected report, `--output-file` and every CI log are
    /// plain text either way; `NO_COLOR` in the environment does the same.
    /// Accepted on any subcommand.
    #[arg(long, global = true)]
    pub(crate) no_color: bool,
    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Parse a single Paradox script file (or a directory of .cwt rule files) and print summary
    Parse {
        /// Path to a file or a directory of .cwt files
        file: PathBuf,
    },
    /// Discover and parse all files under a directory
    Discover {
        /// Root directory to search
        directory: PathBuf,
    },
    /// Serialize AST to cache file (.cwb)
    Serialize {
        /// Input script file
        input: PathBuf,
        /// Output cache file
        output: PathBuf,
    },
    /// Deserialize cache file (.cwb) and verify
    Deserialize {
        /// Input cache file
        input: PathBuf,
    },
    /// Parse a .cwt rules file or directory, print a summary, and report the
    /// problems the ruleset itself has. Exits 1 when any of them is an error.
    Rules {
        /// Path to a .cwt file or a directory containing .cwt files
        file: PathBuf,
        /// Report format for the rules problems: cli (default, grouped text),
        /// csv, json, github (Actions workflow commands) or sarif (SARIF
        /// 2.1.0). Anything but cli moves the ruleset summary to stderr so the
        /// report has stdout to itself.
        #[arg(long, value_name = "FORMAT", value_parser = report::parse_report_type)]
        report_type: Option<ReportType>,
        /// Exit 1 when a reported problem reaches this severity. Valid values:
        /// error (default), warning, info, hint, none. `none` reports without
        /// ever failing the run.
        #[arg(long, value_name = "LEVEL", value_parser = run::parse_fail_on)]
        fail_on: Option<FailOn>,
    },
    /// Validate a directory of game files against .cwt rules
    Validate(ValidateArgs),
    /// Pre-generate a vanilla type index from a base-game install, for use with
    /// `validate --vanilla-cache`. Parses and indexes the install once so later
    /// runs resolve base-game references without re-parsing it.
    CacheVanilla {
        /// Game identifier (hoi4, stellaris, eu4, ck2, ck3, vic2, vic3, ir, eu5, custom)
        #[arg(long, short)]
        game: String,
        /// Base-game install directory to index
        #[arg(long)]
        vanilla: PathBuf,
        /// Path to a .cwt rules file OR a directory containing .cwt rule files
        #[arg(long, short)]
        rules: PathBuf,
        /// Output cache file to write
        #[arg(long, short)]
        output: PathBuf,
    },
    /// Parse and validate localisation files (.yml)
    Loc(LocArgs),
    /// Apply machine-applicable fixes for the curated fixable diagnostics.
    /// Dry-run by default (prints a unified-diff preview); pass `--apply` to write.
    Fix(FixArgs),
    /// Print what one CWxxx code means: its severity, its message template, the
    /// long form from the error-code reference, and whether the check is wired.
    Explain {
        /// The code to explain, e.g. CW113 (case-insensitive)
        #[arg(value_name = "CODE")]
        code: String,
    },
    /// List every diagnostic code, with its severity and a one-line summary.
    ListCodes,
    /// Print a shell completion script on stdout. Source it, or drop it where
    /// the shell looks: `cwtools completions bash > /etc/bash_completion.d/cwtools`.
    Completions {
        /// Shell to generate for: bash, elvish, fish, powershell or zsh
        #[arg(value_name = "SHELL")]
        shell: clap_complete::Shell,
    },
}

// Deliberately not a doc comment: the `Validate` variant carries the
// subcommand's about text, and a second one here would compete with it.
#[derive(Args)]
pub(crate) struct ValidateArgs {
    /// Read settings from this cwtools.toml instead of searching for one.
    ///
    /// Without it, the first `cwtools.toml` at or above --directory (or the
    /// working directory) is used. Flags override file values; the boolean
    /// switches are the exception — they can only add to a `true` in the
    /// file, never turn one off.
    /// Recognised keys: game, directory, rules, vanilla, vanilla-cache,
    /// no-vanilla-cache, refresh-vanilla-cache, report-type, min-severity,
    /// ignore-files, ignore-dirs, loc-languages, ignore-codes, only-codes,
    /// allow-empty. Relative paths resolve against the config file.
    #[arg(long, value_name = "FILE")]
    pub(crate) config: Option<PathBuf>,
    /// Game identifier (hoi4, stellaris, eu4, ck2, ck3, vic2, vic3, ir, eu5, custom)
    #[arg(long, short)]
    pub(crate) game: Option<String>,
    /// Directory containing game files. A single mod root is validated as-is.
    /// A workspace of mods (a directory that is not itself a mod root but whose
    /// `mod/`/`mods/` folder holds `.mod` descriptors) is auto-detected and
    /// expanded: every referenced mod is validated together, layered by load
    /// order (a later-resolved mod overrides a shared logical path; a mod's
    /// `replace_path` suppresses lower-priority files under that prefix).
    #[arg(long, short)]
    pub(crate) directory: Option<PathBuf>,
    /// Path to a .cwt rules file OR a directory containing .cwt rule files
    #[arg(long, short)]
    pub(crate) rules: Option<PathBuf>,
    /// Optional path to the base game install (e.g. the vanilla HOI4 folder).
    /// Its files are indexed for reference resolution but not validated, so a
    /// mod can reference base-game content (operation_tokens, ship_names, …)
    /// without false "not a known instance" errors. The index is cached under
    /// the OS cache dir (XDG_CACHE_HOME/cwtools, %LOCALAPPDATA%\cwtools,
    /// ~/.cache/cwtools) and reused while the install and rules are unchanged;
    /// see --no-vanilla-cache / --refresh-vanilla-cache.
    #[arg(long)]
    pub(crate) vanilla: Option<PathBuf>,
    /// Optional pre-generated vanilla index (see `cache-vanilla`). Loaded for
    /// reference resolution without re-parsing the game install. Faster than
    /// `--vanilla`; can be combined with it.
    #[arg(long)]
    pub(crate) vanilla_cache: Option<PathBuf>,
    /// Don't read or write the automatic base-game cache: re-parse the
    /// `--vanilla` install on every run.
    #[arg(long)]
    pub(crate) no_vanilla_cache: bool,
    /// Ignore any existing automatic base-game cache, re-parse the
    /// `--vanilla` install, and overwrite the cache with the result.
    #[arg(long)]
    pub(crate) refresh_vanilla_cache: bool,
    /// Report format: cli (default, grouped text), csv, json, github
    /// (Actions workflow commands, annotating the PR diff), or sarif
    /// (SARIF 2.1.0 for code-scanning upload).
    #[arg(long, value_name = "FORMAT", value_parser = report::parse_report_type)]
    pub(crate) report_type: Option<ReportType>,
    /// Write the report to this file instead of stdout.
    #[arg(long)]
    pub(crate) output_file: Option<PathBuf>,
    /// Suppress diagnostics whose hash is listed in this file (one hash per
    /// line). Lets you baseline known/accepted diagnostics and see only new ones.
    #[arg(long)]
    pub(crate) ignore_hashes: Option<PathBuf>,
    /// Write the surviving diagnostics' hashes (one per line) to this file, to
    /// use later with --ignore-hashes.
    #[arg(long)]
    pub(crate) output_hashes: Option<PathBuf>,
    /// Extra filename glob patterns to skip (in addition to the engine
    /// defaults like Changelog.txt, README.md, *.md). May be repeated.
    /// Examples: --ignore-file "secret*" --ignore-file "*.notes"
    #[arg(long = "ignore-file", value_name = "GLOB")]
    pub(crate) ignore_files: Vec<String>,
    /// Extra directory glob patterns to skip during workspace discovery.
    /// May be repeated. Examples: --ignore-dir "build" --ignore-dir "temp*"
    #[arg(long = "ignore-dir", value_name = "GLOB")]
    pub(crate) ignore_dirs: Vec<String>,
    /// Restrict loc validation/lookup to this language (repeatable). Valid
    /// values: english, french, german, spanish, russian, polish, braz_por,
    /// simp_chinese, japanese, korean, turkish, default. Omit to use every
    /// language with data (current behavior).
    #[arg(long = "loc-language", value_name = "LANG", value_parser = parse_lang)]
    pub(crate) loc_language: Vec<Lang>,
    /// Enforce exact on-disk case for CW113 `filepath` references. On by
    /// default; pass `false` (or set `case-sensitive-files = false` in
    /// `cwtools.toml`) for a Windows-authored mod that must tolerate case
    /// mismatches.
    #[arg(long, num_args = 0..=1, default_missing_value = "true", value_parser = clap::value_parser!(bool))]
    pub(crate) case_sensitive_files: Option<bool>,
    /// Only report diagnostics at or above this severity. Valid values:
    /// error, warning, info, hint. Omit to report everything (current
    /// behavior).
    #[arg(long, value_name = "LEVEL", value_parser = parse_min_severity)]
    pub(crate) min_severity: Option<ErrorSeverity>,
    /// Exit 1 when a reported diagnostic reaches this severity. Valid values:
    /// error (default), warning, info, hint, none. `none` reports without ever
    /// failing the run, for a job that publishes findings and gates on nothing.
    /// Counts what the report holds, so `--min-severity` filters first: asking
    /// to fail on something it dropped can never trip.
    #[arg(long, value_name = "LEVEL", value_parser = run::parse_fail_on)]
    pub(crate) fail_on: Option<FailOn>,
    /// Drop every diagnostic with this CW code (repeatable). The same
    /// suppression the editor applies via `cwtools.errors.ignore`, so one
    /// policy can cover both. Example: --ignore-code CW100
    #[arg(long = "ignore-code", value_name = "CWxxx", value_parser = codes::parse_code)]
    pub(crate) ignore_codes: Vec<String>,
    /// Report only diagnostics with this CW code (repeatable). Omit to
    /// report every code. `--ignore-code` still applies on top.
    #[arg(long = "only-code", value_name = "CWxxx", value_parser = codes::parse_code)]
    pub(crate) only_codes: Vec<String>,
    /// Report only diagnostics in this file (repeatable). The directory is
    /// still indexed whole — the cross-file checks need it — so this scopes the
    /// report, the counts and the exit code, not the run.
    /// Example: --file common/ideas/mine.txt
    #[arg(long = "file", value_name = "PATH")]
    pub(crate) files: Vec<PathBuf>,
    /// Report only diagnostics in files that changed since this git ref: the
    /// `--file` set a pre-commit hook or a PR job would otherwise assemble
    /// itself. Compares the working tree against the merge base of the ref and
    /// HEAD, so a branch reports what it changed rather than what its base
    /// branch did, and counts untracked files. Unions with --file.
    /// Example: --since origin/main
    #[arg(long, value_name = "GIT-REF")]
    pub(crate) since: Option<String>,
    /// Accept a run with nothing to validate. Without this, a ruleset that
    /// loads no types or a directory that yields no files is an error
    /// (exit 4) instead of a silent "0 errors".
    #[arg(long)]
    pub(crate) allow_empty: bool,
}

// See the note on `ValidateArgs`.
#[derive(Args)]
pub(crate) struct LocArgs {
    /// Directory containing localisation .yml files
    pub(crate) directory: Option<PathBuf>,
    /// Read settings from this cwtools.toml instead of searching for one.
    /// `loc` reads game, directory, rules, report-type, min-severity,
    /// ignore-files, ignore-dirs, loc-languages, ignore-codes, only-codes and
    /// allow-empty; see `validate --help` for the schema.
    #[arg(long, value_name = "FILE")]
    pub(crate) config: Option<PathBuf>,
    /// Game identifier (hoi4, stellaris, eu4, ck2, ck3, vic2, vic3, ir, eu5, custom).
    /// Together with --rules it turns on the scope-aware loc command checks
    /// (CW226/CW260/CW266); on its own it does nothing.
    #[arg(long, short)]
    pub(crate) game: Option<String>,
    /// Path to a .cwt rules file OR a directory containing .cwt rule files.
    /// Read for its scope and link definitions only — no game files are
    /// discovered or indexed. Needs --game to take effect.
    #[arg(long, short)]
    pub(crate) rules: Option<PathBuf>,
    /// Report format: cli (default, grouped text), csv, json, github
    /// (Actions workflow commands), or sarif (SARIF 2.1.0).
    #[arg(long, value_name = "FORMAT", value_parser = report::parse_report_type)]
    pub(crate) report_type: Option<ReportType>,
    /// Write the report to this file instead of stdout.
    #[arg(long)]
    pub(crate) output_file: Option<PathBuf>,
    /// Suppress diagnostics whose hash is listed in this file (one hash per
    /// line). Lets you baseline known/accepted diagnostics and see only new ones.
    #[arg(long)]
    pub(crate) ignore_hashes: Option<PathBuf>,
    /// Write the surviving diagnostics' hashes (one per line) to this file, to
    /// use later with --ignore-hashes.
    #[arg(long)]
    pub(crate) output_hashes: Option<PathBuf>,
    /// Filename glob patterns to skip while scanning. May be repeated.
    /// Examples: --ignore-file "wip*" --ignore-file "*_draft_l_english.yml"
    #[arg(long = "ignore-file", value_name = "GLOB")]
    pub(crate) ignore_files: Vec<String>,
    /// Directory glob patterns to skip while scanning, matched against any
    /// directory name under the target. May be repeated.
    /// Examples: --ignore-dir "build" --ignore-dir "temp*"
    #[arg(long = "ignore-dir", value_name = "GLOB")]
    pub(crate) ignore_dirs: Vec<String>,
    /// Restrict the scan to this language (repeatable). Valid values: english,
    /// french, german, spanish, russian, polish, braz_por, simp_chinese,
    /// japanese, korean, turkish, default. Omit to check every language with
    /// data. A `$ref$` only resolves against the languages that are loaded.
    #[arg(long = "loc-language", value_name = "LANG", value_parser = parse_lang)]
    pub(crate) loc_language: Vec<Lang>,
    /// Only report diagnostics at or above this severity. Valid values:
    /// error, warning, info, hint. Omit to report everything.
    #[arg(long, value_name = "LEVEL", value_parser = parse_min_severity)]
    pub(crate) min_severity: Option<ErrorSeverity>,
    /// Exit 1 when a reported diagnostic reaches this severity. Valid values:
    /// error (default), warning, info, hint, none. Counts what the report
    /// holds, so `--min-severity` filters first.
    #[arg(long, value_name = "LEVEL", value_parser = run::parse_fail_on)]
    pub(crate) fail_on: Option<FailOn>,
    /// Drop every diagnostic with this CW code (repeatable).
    #[arg(long = "ignore-code", value_name = "CWxxx", value_parser = codes::parse_code)]
    pub(crate) ignore_codes: Vec<String>,
    /// Report only diagnostics with this CW code (repeatable).
    #[arg(long = "only-code", value_name = "CWxxx", value_parser = codes::parse_code)]
    pub(crate) only_codes: Vec<String>,
    /// Accept a run with nothing to check. Without this, a directory that
    /// holds no localisation files is an error (exit 4).
    #[arg(long)]
    pub(crate) allow_empty: bool,
}

// See the note on `ValidateArgs`.
#[derive(Args)]
pub(crate) struct FixArgs {
    /// Read settings from this cwtools.toml instead of searching for one.
    /// `fix` reads every key `validate` does except report-type and
    /// min-severity; see `validate --help` for the schema.
    #[arg(long, value_name = "FILE")]
    pub(crate) config: Option<PathBuf>,
    /// Game identifier (hoi4, stellaris, eu4, ck2, ck3, vic2, vic3, ir, eu5, custom)
    #[arg(long, short)]
    pub(crate) game: Option<String>,
    /// Directory containing game files
    #[arg(long, short)]
    pub(crate) directory: Option<PathBuf>,
    /// Path to a .cwt rules file OR a directory containing .cwt rule files
    #[arg(long, short)]
    pub(crate) rules: Option<PathBuf>,
    /// Optional path to the base game install, indexed for reference
    /// resolution (see `validate --vanilla`).
    #[arg(long)]
    pub(crate) vanilla: Option<PathBuf>,
    /// Optional pre-generated vanilla index (see `cache-vanilla`).
    #[arg(long)]
    pub(crate) vanilla_cache: Option<PathBuf>,
    /// Don't read or write the automatic base-game cache (see `validate`).
    #[arg(long)]
    pub(crate) no_vanilla_cache: bool,
    /// Ignore any existing automatic base-game cache and overwrite it.
    #[arg(long)]
    pub(crate) refresh_vanilla_cache: bool,
    /// Extra filename glob patterns to skip. May be repeated.
    #[arg(long = "ignore-file", value_name = "GLOB")]
    pub(crate) ignore_files: Vec<String>,
    /// Extra directory glob patterns to skip. May be repeated.
    #[arg(long = "ignore-dir", value_name = "GLOB")]
    pub(crate) ignore_dirs: Vec<String>,
    /// Restrict loc validation/lookup to this language (repeatable).
    #[arg(long = "loc-language", value_name = "LANG", value_parser = parse_lang)]
    pub(crate) loc_language: Vec<Lang>,
    /// Enforce exact on-disk case for CW113 `filepath` references. On by
    /// default; pass `false` (or set `case-sensitive-files = false` in
    /// `cwtools.toml`) for a Windows-authored mod that must tolerate case
    /// mismatches.
    #[arg(long, num_args = 0..=1, default_missing_value = "true", value_parser = clap::value_parser!(bool))]
    pub(crate) case_sensitive_files: Option<bool>,
    /// Only fix diagnostics with this CW code (repeatable). Omit to fix every
    /// fixable diagnostic. Example: --code CW282 --code CW280
    #[arg(long = "code", value_name = "CWxxx")]
    pub(crate) codes: Vec<String>,
    /// Write the fixes to disk. Without this the command is a dry run and
    /// prints a preview only.
    #[arg(long)]
    pub(crate) apply: bool,
    /// Accept a run with nothing to fix. Without this, a ruleset that loads
    /// no types or a directory that yields no files is an error (exit 4).
    #[arg(long)]
    pub(crate) allow_empty: bool,
}

/// Parse a `--loc-language` value into a `Lang`, for clap's `value_parser`.
pub(crate) fn parse_lang(s: &str) -> Result<Lang, String> {
    Lang::from_name(s).ok_or_else(|| {
        format!(
            "invalid language '{s}': valid values are english, french, german, spanish, russian, \
             polish, braz_por, simp_chinese, japanese, korean, turkish, default"
        )
    })
}

/// Parse a `--min-severity` value into an `ErrorSeverity`, for clap's `value_parser`.
pub(crate) fn parse_min_severity(s: &str) -> Result<ErrorSeverity, String> {
    match s.to_ascii_lowercase().as_str() {
        "error" => Ok(ErrorSeverity::Error),
        "warning" => Ok(ErrorSeverity::Warning),
        "info" => Ok(ErrorSeverity::Information),
        "hint" => Ok(ErrorSeverity::Hint),
        _ => Err(format!(
            "invalid severity '{s}': valid values are error, warning, info, hint"
        )),
    }
}
