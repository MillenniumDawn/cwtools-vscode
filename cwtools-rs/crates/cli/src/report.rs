//! The CI report formats: GitHub Actions workflow commands and SARIF 2.1.0.
//! The `cli`/`csv`/`json` renderers stay in `diag.rs` next to the row helpers
//! they share.

use crate::codes;
use crate::diag::Diag;
use cwtools_error_codes::ErrorCode;
use cwtools_validation::ErrorSeverity;
use std::path::{Path, PathBuf};

/// `--report-type`. The three original spellings render exactly as they did
/// before; `github` and `sarif` are the CI additions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReportType {
    Cli,
    Csv,
    Json,
    Github,
    Sarif,
}

impl ReportType {
    /// The spelling the flag uses, for the "Wrote … report to …" line.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ReportType::Cli => "cli",
            ReportType::Csv => "csv",
            ReportType::Json => "json",
            ReportType::Github => "github",
            ReportType::Sarif => "sarif",
        }
    }
}

/// Parse a `--report-type` value, for clap's `value_parser`. An unrecognised
/// format is an error: silently falling back to the text report would leave a
/// typo'd CI job publishing nothing.
pub(crate) fn parse_report_type(s: &str) -> Result<ReportType, String> {
    match s {
        "cli" => Ok(ReportType::Cli),
        "csv" => Ok(ReportType::Csv),
        "json" => Ok(ReportType::Json),
        "github" => Ok(ReportType::Github),
        "sarif" => Ok(ReportType::Sarif),
        _ => Err(format!(
            "invalid report type '{s}': valid values are cli, csv, json, github, sarif"
        )),
    }
}

/// The directory CI paths are reported relative to. Both formats are resolved
/// against the checkout root by the service consuming them, and a step with a
/// `working-directory:` doesn't move that root — so prefer `GITHUB_WORKSPACE`
/// when the runner set it, and fall back to the process CWD.
pub(crate) fn report_root() -> PathBuf {
    match std::env::var_os("GITHUB_WORKSPACE") {
        Some(w) if !w.is_empty() => PathBuf::from(w),
        _ => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

/// A diagnostic's file as a CI report should write it: relative to `base` when
/// it sits underneath (so an annotation lands on the PR diff), else the
/// absolute path. The bool says which one came back.
fn locate(file: &str, base: &Path) -> (String, bool) {
    let abs = std::path::absolute(file).unwrap_or_else(|_| PathBuf::from(file));
    match abs.strip_prefix(base) {
        Ok(rel) => (slashed(rel), true),
        Err(_) => (slashed(&abs), false),
    }
}

/// Forward slashes regardless of host: both formats are consumed by services
/// that treat `\` as a literal character, not a separator.
fn slashed(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

// ── GitHub Actions ───────────────────────────────────────────────────────────

fn github_level(s: ErrorSeverity) -> &'static str {
    match s {
        ErrorSeverity::Error => "error",
        ErrorSeverity::Warning => "warning",
        ErrorSeverity::Information | ErrorSeverity::Hint => "notice",
    }
}

/// Escape a workflow-command message. The runner percent-decodes `%25`/`%0D`/
/// `%0A`, so an unescaped newline would end the command and dump the rest of
/// the message into the log as plain text.
fn escape_data(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' => out.push_str("%25"),
            '\r' => out.push_str("%0D"),
            '\n' => out.push_str("%0A"),
            c => out.push(c),
        }
    }
    out
}

/// Escape a workflow-command property value. `:` and `,` separate the command's
/// own fields, so a file name containing either has to be encoded as well.
fn escape_property(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in escape_data(s).chars() {
        match c {
            ':' => out.push_str("%3A"),
            ',' => out.push_str("%2C"),
            c => out.push(c),
        }
    }
    out
}

/// One `::error file=…,line=…,col=…::message` line (trailing newline included).
/// Whole-file diagnostics carry line 0; they're clamped to 1 because an
/// annotation without a real line attaches to the run instead of the file.
pub(crate) fn github_row(d: &Diag, base: &Path) -> String {
    let (file, _) = locate(&d.file, base);
    let mut props = format!(
        "file={},line={},col={}",
        escape_property(&file),
        d.line.max(1),
        d.col.max(1)
    );
    if !d.code.is_empty() {
        props.push_str(",title=");
        props.push_str(&escape_property(d.code));
    }
    format!(
        "::{} {}::{}\n",
        github_level(d.severity),
        props,
        escape_data(&d.message)
    )
}

/// One `::notice::message` line for something that belongs to the run rather
/// than to a file, so it lands in the job log instead of on a random source line.
pub(crate) fn github_notice(message: &str) -> String {
    format!("::notice::{}\n", escape_data(message))
}

// ── SARIF 2.1.0 ──────────────────────────────────────────────────────────────

const SARIF_SCHEMA: &str = "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/Schemata/sarif-schema-2.1.0.json";
const INFORMATION_URI: &str = "https://github.com/MillenniumDawn/cwtools";

/// Notification descriptor for the run-level "these checks did not run" message.
const VANILLA_NOTIFICATION_ID: &str = "cwtools/vanilla-gated-checks";

fn sarif_level(s: ErrorSeverity) -> &'static str {
    match s {
        ErrorSeverity::Error => "error",
        ErrorSeverity::Warning => "warning",
        ErrorSeverity::Information | ErrorSeverity::Hint => "note",
    }
}

/// Percent-encode a path for a SARIF URI. Paradox installs live in paths like
/// `Hearts of Iron IV`, and a raw space is not a legal URI character.
/// `allow_colon` is for absolute URIs only: a bare `:` in the first segment of
/// a relative reference reads as a scheme.
fn uri_encode(path: &str, allow_colon: bool) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char);
            }
            b':' if allow_colon => out.push(':'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn file_uri(path: &str) -> String {
    let encoded = uri_encode(path, true);
    if encoded.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    }
}

/// The SARIF 2.1.0 document for `diags` (trailing newline included). `base` is
/// the run's source root: locations under it are emitted relative to
/// `%SRCROOT%`, so the report resolves in any checkout of the same tree.
///
/// `tool.driver.rules` is generated from the shared emitted error-code
/// catalog entries, and carries only the codes this run actually reported so
/// every `ruleIndex` resolves. `notice` is a run-level message with no location
/// (the checks a missing base-game index disabled); it becomes a
/// `toolConfigurationNotifications` entry rather than a result, and the whole
/// `invocations` array is omitted when there is nothing to say.
pub(crate) fn sarif_report(diags: &[&Diag], base: &Path, notice: Option<&str>) -> String {
    let mut rules: Vec<&'static (&'static str, ErrorCode)> = diags
        .iter()
        .filter_map(|d| codes::emitted_entry(d.code))
        .collect();
    rules.sort_unstable_by_key(|(_, c)| c.id);
    rules.dedup_by_key(|(_, c)| c.id);

    let rule_index: std::collections::HashMap<String, usize> = rules
        .iter()
        .enumerate()
        .map(|(i, (_, c))| (c.id.to_ascii_lowercase(), i))
        .collect();

    let sarif_rules: Vec<SarifRule> = rules.iter().map(sarif_rule_from).collect();
    let sarif_results: Vec<SarifResult> = diags
        .iter()
        .map(|d| sarif_result_from(d, base, &rule_index))
        .collect();

    // A base URI must end in exactly one '/'. Trimming them all would turn the
    // filesystem root's `file:///` into `file:/`.
    let root = file_uri(&slashed(base));
    let root = match root.strip_suffix('/') {
        Some(_) => root,
        None => format!("{root}/"),
    };

    let doc = SarifDocument {
        schema: SARIF_SCHEMA.to_string(),
        version: "2.1.0".to_string(),
        runs: vec![SarifRun {
            tool: SarifTool {
                driver: SarifDriver {
                    name: "cwtools".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    information_uri: INFORMATION_URI.to_string(),
                    rules: sarif_rules,
                },
            },
            invocations: notice
                .map(|text| SarifInvocation {
                    execution_successful: true,
                    tool_configuration_notifications: vec![SarifNotification {
                        descriptor: SarifDescriptorReference {
                            id: VANILLA_NOTIFICATION_ID.to_string(),
                        },
                        level: "note".to_string(),
                        message: SarifMessage {
                            text: text.to_string(),
                        },
                    }],
                })
                .into_iter()
                .collect(),
            original_uri_base_ids: {
                let mut m = std::collections::BTreeMap::new();
                m.insert("SRCROOT".to_string(), SarifUriBase { uri: root });
                m
            },
            column_kind: "unicodeCodePoints".to_string(),
            results: sarif_results,
        }],
    };
    let mut out = serde_json::to_string_pretty(&doc).unwrap();
    out.push('\n');
    out
}

#[derive(serde::Serialize)]
struct SarifDocument {
    #[serde(rename = "$schema")]
    schema: String,
    version: String,
    runs: Vec<SarifRun>,
}

#[derive(serde::Serialize)]
struct SarifRun {
    tool: SarifTool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    invocations: Vec<SarifInvocation>,
    #[serde(rename = "originalUriBaseIds")]
    original_uri_base_ids: std::collections::BTreeMap<String, SarifUriBase>,
    #[serde(rename = "columnKind")]
    column_kind: String,
    results: Vec<SarifResult>,
}

#[derive(serde::Serialize)]
struct SarifInvocation {
    #[serde(rename = "executionSuccessful")]
    execution_successful: bool,
    #[serde(rename = "toolConfigurationNotifications")]
    tool_configuration_notifications: Vec<SarifNotification>,
}

#[derive(serde::Serialize)]
struct SarifNotification {
    descriptor: SarifDescriptorReference,
    level: String,
    message: SarifMessage,
}

#[derive(serde::Serialize)]
struct SarifDescriptorReference {
    id: String,
}

#[derive(serde::Serialize)]
struct SarifUriBase {
    uri: String,
}

#[derive(serde::Serialize)]
struct SarifTool {
    driver: SarifDriver,
}

#[derive(serde::Serialize)]
struct SarifDriver {
    name: String,
    version: String,
    #[serde(rename = "informationUri")]
    information_uri: String,
    rules: Vec<SarifRule>,
}

#[derive(serde::Serialize)]
struct SarifRule {
    id: String,
    name: String,
    #[serde(rename = "shortDescription")]
    short_description: SarifMessage,
    #[serde(rename = "defaultConfiguration")]
    default_configuration: SarifLevel,
    #[serde(rename = "helpUri")]
    help_uri: String,
}

#[derive(serde::Serialize)]
struct SarifLevel {
    level: String,
}

#[derive(serde::Serialize)]
struct SarifMessage {
    text: String,
}

#[derive(serde::Serialize)]
struct SarifResult {
    #[serde(rename = "ruleId", skip_serializing_if = "String::is_empty")]
    rule_id: String,
    #[serde(rename = "ruleIndex", skip_serializing_if = "Option::is_none")]
    rule_index: Option<usize>,
    level: String,
    message: SarifMessage,
    locations: Vec<SarifLocation>,
    /// The secondary spans the message points at (the `if` a stray `else` is
    /// missing, the case a path is indexed under). Omitted when there are none,
    /// rather than serialized as an empty array.
    #[serde(rename = "relatedLocations", skip_serializing_if = "Vec::is_empty")]
    related_locations: Vec<SarifLocation>,
    #[serde(rename = "partialFingerprints")]
    partial_fingerprints: std::collections::BTreeMap<String, String>,
}

#[derive(serde::Serialize)]
struct SarifLocation {
    #[serde(rename = "physicalLocation")]
    physical_location: SarifPhysicalLocation,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<SarifMessage>,
}

#[derive(serde::Serialize)]
struct SarifPhysicalLocation {
    #[serde(rename = "artifactLocation")]
    artifact_location: SarifArtifactLocation,
    region: SarifRegion,
}

#[derive(serde::Serialize)]
struct SarifArtifactLocation {
    uri: String,
    #[serde(rename = "uriBaseId", skip_serializing_if = "Option::is_none")]
    uri_base_id: Option<String>,
}

#[derive(serde::Serialize)]
struct SarifRegion {
    #[serde(rename = "startLine")]
    start_line: u32,
    #[serde(rename = "startColumn")]
    start_column: u32,
    /// Both are exclusive ends in SARIF's convention, and both are omitted for
    /// a diagnostic the emit site gave no range for: a region with only a start
    /// is a valid point, while a wrong end draws a squiggle over the wrong text.
    #[serde(rename = "endLine", skip_serializing_if = "Option::is_none")]
    end_line: Option<u32>,
    #[serde(rename = "endColumn", skip_serializing_if = "Option::is_none")]
    end_column: Option<u32>,
}

fn sarif_rule_from((const_name, code): &&'static (&'static str, ErrorCode)) -> SarifRule {
    SarifRule {
        id: code.id.to_string(),
        name: codes::rule_name(const_name),
        short_description: SarifMessage {
            text: codes::short_description(const_name, code),
        },
        default_configuration: SarifLevel {
            level: sarif_level(code.severity).to_string(),
        },
        help_uri: cwtools_error_codes::doc_url(code.id),
    }
}

fn sarif_result_from(
    d: &Diag,
    base: &Path,
    rule_index: &std::collections::HashMap<String, usize>,
) -> SarifResult {
    let (path, relative) = locate(&d.file, base);
    let (uri, uri_base_id) = if relative {
        (uri_encode(&path, false), Some("SRCROOT".to_string()))
    } else {
        (file_uri(&path), None)
    };
    let rule_idx = if d.code.is_empty() {
        None
    } else {
        rule_index.get(&d.code.to_ascii_lowercase()).copied()
    };
    // Whole-file diagnostics report line 0 and get clamped to 1; carrying an
    // end that predates the clamped start would invert the region.
    let start_line = d.line.max(1);
    let start_column = d.col.max(1);
    let end = d
        .end
        .filter(|(line, col)| (*line, *col) >= (start_line, start_column));
    let location =
        |uri: String, uri_base_id: Option<String>, region: SarifRegion, message| SarifLocation {
            physical_location: SarifPhysicalLocation {
                artifact_location: SarifArtifactLocation { uri, uri_base_id },
                region,
            },
            message,
        };
    SarifResult {
        rule_id: d.code.to_string(),
        rule_index: rule_idx,
        level: sarif_level(d.severity).to_string(),
        message: SarifMessage {
            text: d.message.clone(),
        },
        locations: vec![location(
            uri.clone(),
            uri_base_id.clone(),
            SarifRegion {
                start_line,
                start_column,
                end_line: end.map(|(line, _)| line),
                end_column: end.map(|(_, col)| col),
            },
            None,
        )],
        // Every secondary span is inside the diagnostic's own file, so they all
        // share its artifact location.
        related_locations: d
            .related
            .iter()
            .map(|r| {
                location(
                    uri.clone(),
                    uri_base_id.clone(),
                    SarifRegion {
                        start_line: r.line.max(1),
                        start_column: r.col.max(1),
                        end_line: Some(r.end.0),
                        end_column: Some(r.end.1),
                    },
                    Some(SarifMessage {
                        text: r.message.clone(),
                    }),
                )
            })
            .collect(),
        partial_fingerprints: {
            let mut m = std::collections::BTreeMap::new();
            m.insert("cwtoolsDiagHash/v1".to_string(), d.hash.clone());
            m
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(file: &str, line: u32, col: u32, code: &'static str, message: &str) -> Diag {
        Diag {
            file: file.into(),
            severity: ErrorSeverity::Error,
            code,
            message: message.to_string(),
            line,
            col,
            hash: "0123456789abcdef".to_string(),
            legacy_hash: "fedcba9876543210".to_string(),
            end: None,
            related: Vec::new(),
        }
    }

    /// An absolute path spelled the host's way. On Windows a leading `/` names
    /// the current drive's root, so a base and a file that both start with one
    /// still relate to each other, but neither renders without the drive.
    fn abs(tail: &str) -> String {
        if cfg!(windows) {
            format!("C:/{tail}")
        } else {
            format!("/{tail}")
        }
    }

    /// `file_uri` of an `abs` path, up to and including the first slash.
    const URI: &str = if cfg!(windows) {
        "file:///C:/"
    } else {
        "file:///"
    };

    #[test]
    fn report_type_round_trips_its_spelling() {
        for name in ["cli", "csv", "json", "github", "sarif"] {
            assert_eq!(parse_report_type(name).unwrap().as_str(), name);
        }
    }

    #[test]
    fn unknown_report_type_is_rejected() {
        let e = parse_report_type("sarrif").unwrap_err();
        assert!(e.contains("sarrif") && e.contains("sarif"), "got: {e}");
    }

    #[test]
    fn github_row_renders_the_workflow_command() {
        let base = PathBuf::from(abs("repo"));
        let d = diag(
            &abs("repo/common/x.txt"),
            12,
            5,
            "CW282",
            "redundant default",
        );
        assert_eq!(
            github_row(&d, &base),
            "::error file=common/x.txt,line=12,col=5,title=CW282::redundant default\n"
        );
    }

    #[test]
    fn github_row_maps_severity_to_the_three_annotation_levels() {
        let base = Path::new("/repo");
        let mut d = diag("/repo/x.txt", 1, 1, "CW100", "m");
        for (sev, want) in [
            (ErrorSeverity::Error, "::error "),
            (ErrorSeverity::Warning, "::warning "),
            (ErrorSeverity::Information, "::notice "),
            (ErrorSeverity::Hint, "::notice "),
        ] {
            d.severity = sev;
            assert!(github_row(&d, base).starts_with(want), "{sev:?}");
        }
    }

    /// A raw newline would terminate the command and swallow the rest.
    #[test]
    fn github_row_encodes_newlines_and_percent_in_the_message() {
        let base = Path::new("/repo");
        let d = diag("/repo/x.txt", 1, 1, "CW100", "one\r\ntwo 50% off");
        let row = github_row(&d, base);
        assert!(row.contains("::one%0D%0Atwo 50%25 off\n"), "got: {row}");
        assert_eq!(row.matches('\n').count(), 1, "one physical line: {row}");
    }

    #[test]
    fn github_row_encodes_separators_in_the_file_property() {
        let base = PathBuf::from(abs("repo"));
        let d = diag(&abs("repo/od,d:name.txt"), 1, 1, "", "m");
        let row = github_row(&d, &base);
        assert!(row.contains("file=od%2Cd%3Aname.txt,line=1"), "got: {row}");
        assert!(!row.contains("title="), "no code, no title: {row}");
    }

    /// Whole-file diagnostics report line 0, which GitHub can't anchor.
    #[test]
    fn github_row_clamps_line_zero() {
        let base = Path::new("/repo");
        let d = diag("/repo/x.yml", 0, 0, "", "bad file");
        assert!(github_row(&d, base).contains("line=1,col=1"));
    }

    #[test]
    fn paths_outside_the_root_stay_absolute() {
        let base = PathBuf::from(abs("repo"));
        let file = abs("elsewhere/x.txt");
        let d = diag(&file, 1, 1, "CW100", "m");
        let row = github_row(&d, &base);
        assert!(
            row.contains(&format!("file={},line=", escape_property(&file))),
            "got: {row}"
        );
    }

    #[test]
    fn sarif_has_the_2_1_0_envelope() {
        let out = sarif_report(&[], Path::new("/repo"), None);
        assert!(out.contains("\"version\": \"2.1.0\""));
        assert!(out.contains("sarif-schema-2.1.0.json"));
        assert!(out.contains("\"name\": \"cwtools\""));
        assert!(out.contains(&format!("\"version\": \"{}\"", env!("CARGO_PKG_VERSION"))));
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["runs"][0]["tool"]["driver"]["rules"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(v["runs"][0]["results"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn sarif_rules_come_from_the_error_code_catalog() {
        let d = diag("/repo/x.txt", 3, 2, "CW113", "missing file");
        let out = sarif_report(&[&d], Path::new("/repo"), None);
        assert!(out.contains("\"id\": \"CW113\""), "got: {out}");
        assert!(out.contains("\"name\": \"MissingFile\""), "got: {out}");
        // shortDescription is the catalog's message template.
        assert!(out.contains("\"shortDescription\""), "got: {out}");
        assert!(out.contains("\"defaultConfiguration\""), "got: {out}");
        assert!(out.contains("\"ruleIndex\": 0"), "got: {out}");
    }

    /// Pass-through templates ("{}") would render as a useless description.
    #[test]
    fn sarif_describes_pass_through_templates_by_name() {
        let d = diag("/repo/x.txt", 1, 1, "CW240", "value is wrong");
        let out = sarif_report(&[&d], Path::new("/repo"), None);
        assert!(out.contains("\"text\": \"Unexpected value\""), "got: {out}");
    }

    #[test]
    fn sarif_rule_indexes_are_sorted_and_shared() {
        let ds = [
            diag("/repo/a.txt", 1, 1, "CW282", "b"),
            diag("/repo/b.txt", 2, 1, "CW113", "a"),
            diag("/repo/c.txt", 3, 1, "CW282", "c"),
        ];
        let out = sarif_report(&ds.iter().collect::<Vec<_>>(), Path::new("/repo"), None);
        // CW113 sorts first, so it is rule 0 and CW282 is rule 1.
        assert_eq!(out.matches("\"ruleIndex\": 0").count(), 1);
        assert_eq!(out.matches("\"ruleIndex\": 1").count(), 2);
        assert_eq!(out.matches("\"id\": \"CW113\"").count(), 1);
    }

    /// The catalog resolves a code case-insensitively, so a diagnostic carrying
    /// a non-canonical spelling still contributes a rule — and must still find
    /// its index. Keying the lookup on the raw spelling would drop the
    /// `ruleIndex` for exactly these rows.
    #[test]
    fn sarif_rule_index_resolves_a_non_canonical_code_spelling() {
        let d = diag("/repo/x.txt", 3, 2, "cw113", "missing file");
        let out = sarif_report(&[&d], Path::new("/repo"), None);
        assert!(out.contains("\"id\": \"CW113\""), "got: {out}");
        assert!(out.contains("\"ruleIndex\": 0"), "got: {out}");
    }

    #[test]
    fn sarif_locations_are_relative_to_the_source_root() {
        let d = diag(&abs("repo/common/x.txt"), 7, 3, "CW100", "m");
        let out = sarif_report(&[&d], Path::new(&abs("repo")), None);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let loc = &v["runs"][0]["results"][0]["locations"][0]["physicalLocation"];
        assert_eq!(loc["artifactLocation"]["uri"], "common/x.txt");
        assert_eq!(loc["artifactLocation"]["uriBaseId"], "SRCROOT");
        assert_eq!(
            v["runs"][0]["originalUriBaseIds"]["SRCROOT"]["uri"],
            format!("{URI}repo/")
        );
        assert_eq!(loc["region"]["startLine"], 7);
        assert_eq!(loc["region"]["startColumn"], 3);
    }

    /// A base URI keeps exactly one trailing slash; the filesystem root is the
    /// case where trimming them all leaves the scheme mangled as `file:/`.
    #[test]
    fn sarif_root_uri_survives_the_filesystem_root() {
        let out = sarif_report(&[], Path::new("/"), None);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["runs"][0]["originalUriBaseIds"]["SRCROOT"]["uri"],
            "file:///"
        );
    }

    /// `{}` are substitution points, not text a reader wants in a description.
    #[test]
    fn sarif_description_replaces_template_placeholders() {
        let d = diag("/repo/x.txt", 1, 1, "CW100", "m");
        let out = sarif_report(&[&d], Path::new("/repo"), None);
        assert!(
            out.contains("\"text\": \"Localisation key … is not defined for …\""),
            "got: {out}"
        );
        assert!(!out.contains("{}"), "no raw placeholders: {out}");
    }

    #[test]
    fn sarif_encodes_spaces_in_uris() {
        let d = diag(&abs("games/Hearts of Iron IV/x.txt"), 1, 1, "CW100", "m");
        let out = sarif_report(&[&d], Path::new(&abs("repo")), None);
        assert!(
            out.contains(&format!(
                "\"uri\": \"{URI}games/Hearts%20of%20Iron%20IV/x.txt\""
            )),
            "got: {out}"
        );
        assert!(!out.contains("uriBaseId"), "outside the root: {out}");
    }

    #[test]
    fn sarif_omits_the_rule_id_when_a_diagnostic_has_no_code() {
        let d = diag("/repo/x.yml", 0, 0, "", "could not parse");
        let out = sarif_report(&[&d], Path::new("/repo"), None);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v["runs"][0]["results"][0].get("ruleId").is_none());
        assert_eq!(
            v["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"]["startLine"],
            1
        );
        assert_eq!(
            v["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"]["startColumn"],
            1
        );
    }

    /// An end position turns a point into a span, which is what a code-scanning
    /// UI underlines. Both ends are SARIF-exclusive, matching the parser's own
    /// exclusive end.
    #[test]
    fn sarif_regions_carry_the_end_position_when_there_is_one() {
        let mut d = diag("/repo/x.txt", 7, 3, "CW282", "redundant default");
        d.end = Some((7, 19));
        let out = sarif_report(&[&d], Path::new("/repo"), None);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let region = &v["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"];
        assert_eq!(region["startLine"], 7);
        assert_eq!(region["startColumn"], 3);
        assert_eq!(region["endLine"], 7);
        assert_eq!(region["endColumn"], 19);
    }

    /// A point diagnostic must stay a point: an invented end would underline
    /// text the check never looked at.
    #[test]
    fn sarif_omits_the_end_when_the_emit_site_gave_no_range() {
        let d = diag("/repo/x.txt", 7, 3, "CW282", "m");
        let out = sarif_report(&[&d], Path::new("/repo"), None);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let region = &v["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"];
        assert!(region.get("endLine").is_none(), "got: {out}");
        assert!(region.get("endColumn").is_none(), "got: {out}");
    }

    /// A whole-file diagnostic's start is clamped to 1:1, so an end recorded
    /// before that would invert the region.
    #[test]
    fn sarif_drops_an_end_that_precedes_the_clamped_start() {
        let mut d = diag("/repo/x.yml", 0, 0, "", "bad file");
        d.end = Some((0, 4));
        let out = sarif_report(&[&d], Path::new("/repo"), None);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let region = &v["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"];
        assert_eq!(region["startLine"], 1);
        assert!(region.get("endLine").is_none(), "got: {out}");
    }

    #[test]
    fn sarif_publishes_related_spans_against_the_same_file() {
        let mut d = diag(&abs("repo/common/x.txt"), 12, 5, "CW238", "stray else");
        d.related = vec![crate::diag::Related {
            message: "the if this else belongs to".to_string(),
            line: 4,
            col: 3,
            end: (4, 9),
        }];
        let out = sarif_report(&[&d], Path::new(&abs("repo")), None);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let related = &v["runs"][0]["results"][0]["relatedLocations"][0];
        assert_eq!(related["message"]["text"], "the if this else belongs to");
        let loc = &related["physicalLocation"];
        // Same artifact as the diagnostic itself: secondary spans are in-file.
        assert_eq!(loc["artifactLocation"]["uri"], "common/x.txt");
        assert_eq!(loc["artifactLocation"]["uriBaseId"], "SRCROOT");
        assert_eq!(loc["region"]["startLine"], 4);
        assert_eq!(loc["region"]["startColumn"], 3);
        assert_eq!(loc["region"]["endColumn"], 9);
        // The primary location stays unlabelled; the message is the result's.
        assert!(
            v["runs"][0]["results"][0]["locations"][0]
                .get("message")
                .is_none(),
            "got: {out}"
        );
    }

    #[test]
    fn sarif_omits_related_locations_when_there_are_none() {
        let d = diag("/repo/x.txt", 1, 1, "CW100", "m");
        let out = sarif_report(&[&d], Path::new("/repo"), None);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(
            v["runs"][0]["results"][0].get("relatedLocations").is_none(),
            "got: {out}"
        );
    }

    #[test]
    fn sarif_carries_the_diagnostic_hash_as_a_fingerprint() {
        let d = diag("/repo/x.txt", 1, 1, "CW100", "m");
        let out = sarif_report(&[&d], Path::new("/repo"), None);
        assert!(
            out.contains("\"cwtoolsDiagHash/v1\": \"0123456789abcdef\""),
            "got: {out}"
        );
    }

    #[test]
    fn github_notice_is_a_run_level_annotation() {
        assert_eq!(
            github_notice("CW113, CW500 report nothing"),
            "::notice::CW113, CW500 report nothing\n"
        );
        assert!(github_notice("a\nb").contains("a%0Ab"));
    }

    #[test]
    fn sarif_carries_a_run_notice_outside_the_results() {
        let out = sarif_report(&[], Path::new("/repo"), Some("CW113, CW500 report nothing"));
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let notification = &v["runs"][0]["invocations"][0]["toolConfigurationNotifications"][0];
        assert_eq!(notification["level"], "note");
        assert_eq!(notification["descriptor"]["id"], VANILLA_NOTIFICATION_ID);
        assert_eq!(
            notification["message"]["text"],
            "CW113, CW500 report nothing"
        );
        assert_eq!(v["runs"][0]["invocations"][0]["executionSuccessful"], true);
        // A notice is not a finding: nothing joins the results.
        assert_eq!(v["runs"][0]["results"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn sarif_omits_invocations_without_a_notice() {
        let out = sarif_report(&[], Path::new("/repo"), None);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v["runs"][0].get("invocations").is_none(), "got: {out}");
    }

    #[test]
    fn sarif_escapes_json_in_messages() {
        let d = diag("/repo/x.txt", 1, 1, "CW100", "he said \"no\"\nthen left");
        let out = sarif_report(&[&d], Path::new("/repo"), None);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["runs"][0]["results"][0]["message"]["text"],
            "he said \"no\"\nthen left"
        );
        // Also verify it round-trips as valid JSON (escaping was correct).
        assert!(out.contains("\\\"no\\\""));
    }
}
