//! The CW diagnostic catalog, as the CLI sees it.
//!
//! The code list itself is `cwtools_error_codes::CATALOG`; `--ignore-code` /
//! `--only-code` validate against it (a typo is an error, not a silent no-op).
//! Not every listed code is currently emitted; pending codes are marked in
//! `PENDING_CODES`. The SARIF report only turns emitted codes into
//! `tool.driver.rules`.
//!
//! The long form of what a code means lives in `docs/ERROR_CODES.md` and is
//! read from there rather than copied: [`doc_row`] parses the row `explain`
//! prints, so the reference stays the one place the prose is written.

use cwtools_error_codes::{CATALOG, ErrorCode};

// Codes defined in `error_codes` but not yet wired. Kept in step with the
// reference's Status column by `pending_codes_match_the_reference_status`: a
// code listed here after its check lands loses its SARIF rule metadata.
const PENDING_CODES: &[&str] = &[
    "CW220", "CW221", "CW228", "CW230", "CW233", "CW269", "CW273",
];

/// Whether the check behind `id` is still unwired. `list-codes` says so, since
/// a code that never fires is otherwise indistinguishable from a clean run.
pub(crate) fn is_pending_code(id: &str) -> bool {
    PENDING_CODES.iter().any(|p| p.eq_ignore_ascii_case(id))
}

/// The catalog entry for `id` (case-insensitive), or `None` when no CLI parse-time
/// validation entry exists for that code.
pub(crate) fn entry(id: &str) -> Option<&'static (&'static str, ErrorCode)> {
    CATALOG.iter().find(|(_, c)| c.id.eq_ignore_ascii_case(id))
}

/// Every catalog entry, in declaration order (which is code order).
pub(crate) fn all() -> impl Iterator<Item = &'static (&'static str, ErrorCode)> {
    CATALOG.iter()
}

/// The entry for emitted codes only, for SARIF rule metadata.
pub(crate) fn emitted_entry(id: &str) -> Option<&'static (&'static str, ErrorCode)> {
    entry(id).filter(|(_, c)| !is_pending_code(c.id))
}

/// Parse an `--ignore-code` / `--only-code` value, normalising it to the
/// catalog's spelling so the filter compares case-insensitively. For clap's
/// `value_parser`: an unknown code fails the run instead of quietly matching
/// nothing.
pub(crate) fn parse_code(s: &str) -> Result<String, String> {
    entry(s).map(|(_, c)| c.id.to_string()).ok_or_else(|| {
        format!(
            "unknown diagnostic code '{s}': expected a CWxxx code the validator emits \
             (e.g. CW100, CW113, CW225); docs/ERROR_CODES.md lists them all"
        )
    })
}

/// Whether `code` survives an `--only-code` / `--ignore-code` policy. Both empty
/// (the default) keeps everything, so today's runs are unaffected.
pub(crate) fn wanted(code: &str, only: &[String], ignored: &[String]) -> bool {
    (only.is_empty() || only.iter().any(|c| c == code)) && !ignored.iter().any(|c| c == code)
}

/// The words of a catalog const name, minus its `CW###_` prefix.
fn words(const_name: &str) -> impl Iterator<Item = &str> {
    const_name
        .split_once('_')
        .map_or(const_name, |(_, rest)| rest)
        .split('_')
}

/// SARIF `rule.name`: `CW100_MISSING_LOCALISATION` → `MissingLocalisation`.
pub(crate) fn rule_name(const_name: &str) -> String {
    words(const_name).fold(String::new(), |mut out, w| {
        let mut cs = w.chars();
        if let Some(first) = cs.next() {
            out.extend(first.to_uppercase());
            out.extend(cs.flat_map(char::to_lowercase));
        }
        out
    })
}

/// One-line description of a code: its message template with the substitution
/// points blanked out, or the prose form of its const name when the template is
/// a bare pass-through and would render as nothing. Shared by `list-codes` and
/// the SARIF `shortDescription` so the two can't drift.
pub(crate) fn short_description(const_name: &str, code: &ErrorCode) -> String {
    let description = code.message_template.replace("{}", "…");
    if description.trim_matches(['…', ' ']).is_empty() {
        rule_summary(const_name)
    } else {
        description
    }
}

// ── The published reference ──────────────────────────────────────────────────

/// `docs/ERROR_CODES.md`, embedded so `explain` answers from the binary alone.
/// 30 KB of the reference the diagnostics already link to.
const REFERENCE: &str = include_str!("../../../docs/ERROR_CODES.md");

/// A code's row in the reference, as `explain` prints it. The severity and
/// message columns are left to the catalog, which is where the emit sites read
/// them from; `catalog_matches_the_reference` holds the two to each other.
pub(crate) struct DocRow {
    pub(crate) meaning: String,
    pub(crate) status: String,
}

/// The reference row for `id`, or `None` when the doc carries no row for it.
/// Rows are found by the per-code anchor rather than by position, so a heading
/// or a table split above one doesn't move it.
pub(crate) fn doc_row(id: &str) -> Option<DocRow> {
    let cells = row_cells(doc_line(id)?);
    // Leading and trailing `|` bracket the row, so the id lands at 1.
    let cell = |i: usize| cells.get(i).filter(|c| !c.is_empty()).cloned();
    Some(DocRow {
        meaning: cell(4)?,
        status: cell(5)?,
    })
}

/// The raw table line carrying `id`'s anchor.
fn doc_line(id: &str) -> Option<&'static str> {
    let anchor = format!("<a id=\"{}\"></a>", id.to_ascii_lowercase());
    REFERENCE.lines().find(|l| l.contains(&anchor))
}

/// Split a markdown table row into its cells. `\|` is an escaped pipe inside a
/// cell rather than a separator — CW226's row reads `[?ROOT.war_support\|1]`,
/// and splitting there would shift every column after it.
fn row_cells(row: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = row.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => cell.push(chars.next().unwrap_or('\\')),
            '|' => cells.push(std::mem::take(&mut cell).trim().to_string()),
            c => cell.push(c),
        }
    }
    cells.push(cell.trim().to_string());
    cells
}

/// Prose form of a catalog const name, for the SARIF description of the codes
/// whose message template is a bare pass-through.
pub(crate) fn rule_summary(const_name: &str) -> String {
    let mut out = String::new();
    for w in words(const_name) {
        if out.is_empty() {
            out.push_str(&rule_name(w));
        } else {
            out.push(' ');
            out.extend(w.chars().flat_map(char::to_lowercase));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every diagnostic links to `docs/ERROR_CODES.md#cw###`, so a code with no
    /// anchor there lands the reader at the top of the file instead of on its
    /// row. Checked against the doc itself, not a second list to keep in step.
    #[test]
    fn every_code_has_a_documentation_anchor() {
        let doc = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/ERROR_CODES.md"),
        )
        .expect("the error-code reference is checked in beside the crates");

        let missing: Vec<&str> = CATALOG
            .iter()
            .map(|(_, c)| c.id)
            .filter(|id| !doc.contains(&format!("<a id=\"{}\">", id.to_ascii_lowercase())))
            .collect();
        assert!(
            missing.is_empty(),
            "docs/ERROR_CODES.md needs an <a id=\"cw###\"> anchor for: {missing:?}"
        );
    }

    /// `explain` prints the reference's own prose, so a code whose row the
    /// parser can't reach would print a heading with nothing under it.
    #[test]
    fn every_code_has_a_readable_reference_row() {
        let missing: Vec<&str> = CATALOG
            .iter()
            .map(|(_, c)| c.id)
            .filter(|id| doc_row(id).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "docs/ERROR_CODES.md needs a `| <a id=…></a>CW### | Severity | Message | Meaning | \
             Status |` row for: {missing:?}"
        );
    }

    /// The reference restates the severity and the message template the emit
    /// sites actually use. Two copies of a fact drift; this is the check that
    /// says so at test time rather than in someone's CI log.
    #[test]
    fn catalog_matches_the_reference() {
        for (_, code) in CATALOG {
            let cells = row_cells(doc_line(code.id).expect("row exists"));
            assert_eq!(
                cells[2],
                format!("{:?}", code.severity),
                "{} severity disagrees with docs/ERROR_CODES.md",
                code.id
            );
            assert_eq!(
                cells[3], code.message_template,
                "{} message disagrees with docs/ERROR_CODES.md",
                code.id
            );
        }
    }

    /// `PENDING_CODES` decides whether a code reaches `tool.driver.rules`, so a
    /// stale entry silently strips a real diagnostic's SARIF rule metadata —
    /// which is exactly what it did to CW274 between its check landing and this
    /// test. The reference's Status column is the same fact written down.
    #[test]
    fn pending_codes_match_the_reference_status() {
        let mut documented: Vec<&str> = CATALOG
            .iter()
            .map(|(_, c)| c.id)
            .filter(|id| {
                !doc_row(id)
                    .expect("row exists")
                    .status
                    .starts_with("Emitted")
            })
            .collect();
        documented.sort_unstable();
        let mut listed = PENDING_CODES.to_vec();
        listed.sort_unstable();
        assert_eq!(
            documented, listed,
            "PENDING_CODES disagrees with the Status column in docs/ERROR_CODES.md"
        );
    }

    /// CW226's meaning cell contains `[?ROOT.war_support\|1]`; splitting on that
    /// pipe would shift the status column into it.
    #[test]
    fn row_cells_keeps_an_escaped_pipe_inside_its_cell() {
        let cells = row_cells(r"| a | b\|c | d |");
        assert_eq!(cells, ["", "a", "b|c", "d", ""]);
        let cw226 = doc_row("CW226").expect("CW226 is documented");
        assert!(cw226.meaning.contains("war_support|1"), "{}", cw226.meaning);
        assert!(cw226.status.starts_with("Emitted"), "{}", cw226.status);
    }

    #[test]
    fn short_description_falls_back_to_the_const_name() {
        let (_, cw240) = entry("CW240").unwrap();
        assert_eq!(
            short_description("CW240_UNEXPECTED_VALUE", cw240),
            "Unexpected value"
        );
        let (_, cw110) = entry("CW110").unwrap();
        assert_eq!(
            short_description("CW110_TECH_CAT_MISSING", cw110),
            cw110.message_template
        );
    }

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<&str> = CATALOG.iter().map(|(_, c)| c.id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate CW id in the catalog");
    }

    #[test]
    fn parse_code_is_case_insensitive_and_normalises() {
        assert_eq!(parse_code("cw100").unwrap(), "CW100");
        assert_eq!(parse_code("CW100").unwrap(), "CW100");
    }

    #[test]
    fn parse_code_rejects_an_unknown_code() {
        let err = parse_code("CW999").unwrap_err();
        assert!(err.contains("CW999"), "got: {err}");
        assert!(err.contains("ERROR_CODES.md"), "got: {err}");
    }

    #[test]
    fn parse_code_rejects_retired_code() {
        let err = parse_code("CW258").unwrap_err();
        assert!(err.contains("CW258"), "got: {err}");
    }

    #[test]
    fn parse_code_accepts_pending_codes() {
        assert_eq!(parse_code("CW220").unwrap(), "CW220");
        assert_eq!(parse_code("CW269").unwrap(), "CW269");
    }

    #[test]
    fn emitted_entry_skips_pending_codes() {
        assert!(entry("CW220").is_some(), "pending code is still parseable");
        assert!(
            emitted_entry("CW220").is_none(),
            "pending code stays out of SARIF rules"
        );
        assert!(emitted_entry("CW113").is_some());
    }

    #[test]
    fn wanted_defaults_to_keeping_everything() {
        assert!(wanted("CW100", &[], &[]));
    }

    #[test]
    fn wanted_applies_only_then_ignore() {
        let only = vec!["CW100".to_string(), "CW113".to_string()];
        let ignored = vec!["CW113".to_string()];
        assert!(wanted("CW100", &only, &[]));
        assert!(!wanted("CW225", &only, &[]));
        assert!(!wanted("CW113", &only, &ignored), "ignore wins over only");
        assert!(!wanted("CW113", &[], &ignored));
    }

    #[test]
    fn rule_names_drop_the_code_prefix() {
        assert_eq!(
            rule_name("CW100_MISSING_LOCALISATION"),
            "MissingLocalisation"
        );
        assert_eq!(
            rule_summary("CW100_MISSING_LOCALISATION"),
            "Missing localisation"
        );
        assert_eq!(rule_name("CW001_PARSE_ERROR"), "ParseError");
    }
}
