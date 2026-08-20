//! The report row shared by `validate` and `loc`: mapping a diagnostic from
//! either source onto a common shape, hashing it for `--ignore-hashes`
//! baselines, and rendering the csv/json/cli columns. The `github` and `sarif`
//! renderers live in `report`.

use cwtools_validation::{ErrorSeverity, ValidationError};
use std::borrow::Cow;
use std::path::Path;

/// FNV-1a-64 hex digest of `parts`, joined by `|`. FNV rather than std's
/// `DefaultHasher` because the seed there is randomized per process: a baseline
/// file has to mean the same thing on every run and every machine.
fn fnv1a_digest(parts: [&str; 4]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut mix = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    };
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            mix(b'|');
        }
        for b in part.bytes() {
            mix(b);
        }
    }
    format!("{:016x}", h)
}

/// `file` relative to `root` and `/`-separated, for hashing: a baseline must
/// mean the same thing whether `file` came out absolute or mod-relative.
/// Falls back to `file` (still `/`-separated) when it isn't under `root` — a
/// vanilla install path reported alongside mod files, say — rather than
/// panicking. Lexical only, no filesystem access, so a file that was never
/// written to disk (as in tests) still hashes. The strip is done via
/// `Path::strip_prefix`, with a drive-prefix prepended to `file` on Windows
/// when it has none: a leading-`/`-no-drive path is root-relative there
/// while a `C:/...` path is drive-absolute, and the two prefix kinds don't
/// match under `strip_prefix`. We don't want the same logical path spelled
/// two ways to fall through to the fallback and hash differently.
fn relative_file(file: &str, root: &Path) -> String {
    let mut file = file.replace('\\', "/");
    let root_str = root.to_string_lossy().replace('\\', "/");
    if let Some(drive) = drive_letter(&root_str)
        && !file.contains(':')
        && let Some(rest) = file.strip_prefix('/')
    {
        file = format!("{drive}:/{rest}");
    }
    match Path::new(&file).strip_prefix(Path::new(&root_str)) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => file,
    }
}

/// The drive letter `path` starts with (`C` in `C:/repo/mod`), or `None`. A
/// colon anywhere else is part of a directory name, which is legal on Unix,
/// and reading it as a drive would rewrite paths that have nothing to do with
/// Windows.
fn drive_letter(path: &str) -> Option<char> {
    let mut chars = path.chars();
    let drive = chars.next().filter(char::is_ascii_alphabetic)?;
    (chars.next() == Some(':')).then_some(drive)
}

/// Stable digest of a diagnostic, for baseline/ignore matching. Keyed on the
/// trimmed text of the offending source line rather than its line number, so
/// inserting a line above a baselined diagnostic doesn't resurface it as new.
/// Two identical diagnostics on two identical lines of one file collapse to one
/// digest, which is the intended trade: baselines track content, not position.
/// `file` is relativized against `root` first, so the digest doesn't depend on
/// whether this invocation happened to see an absolute or relative path.
fn diag_hash(root: &Path, file: &str, code: &str, message: &str, line_text: &str) -> String {
    fnv1a_digest([relative_file(file, root).as_str(), code, message, line_text])
}

/// The previous digest, keyed on the line number. Still accepted when matching
/// `--ignore-hashes` so existing baselines don't all invalidate at once; never
/// emitted. Remove once the migration window closes.
fn legacy_diag_hash(root: &Path, file: &str, code: &str, message: &str, line: u32) -> String {
    fnv1a_digest([
        relative_file(file, root).as_str(),
        code,
        message,
        &line.to_string(),
    ])
}

/// One-slot memo of a file's trimmed lines, feeding [`diag_hash`]. Diagnostics
/// arrive grouped by file from both the validation and loc passes, so a single
/// slot keeps each file to one read and only one file resident.
#[derive(Default)]
pub(crate) struct SourceLines {
    file: String,
    lines: Vec<String>,
    inline_ignored: cwtools_validation::inline_ignore::InlineIgnoreMap,
}

impl SourceLines {
    /// Trimmed text of 1-based `line` in `file`; `""` when the file can't be
    /// read or the line doesn't exist (whole-file diagnostics report line 0).
    pub(crate) fn trimmed(&mut self, file: &str, line: u32) -> &str {
        if self.file != file {
            self.lines = std::fs::read_to_string(file)
                .map(|text| text.lines().map(|l| l.trim().to_string()).collect())
                .unwrap_or_default();
            self.file = file.to_string();
            // Built from the same trimmed lines the hash path reads, so a
            // directive costs nothing extra; trimming cannot hide one (it
            // only strips leading/trailing whitespace around the line).
            self.inline_ignored.clear();
            for (i, line) in self.lines.iter().enumerate() {
                if let Some(codes) = cwtools_validation::inline_ignore::inline_directive_codes(line)
                {
                    self.inline_ignored
                        .insert(i as u32 + 1, codes.into_iter().collect());
                }
            }
        }
        line.checked_sub(1)
            .and_then(|i| self.lines.get(i as usize))
            .map_or("", String::as_str)
    }

    /// Whether a `# cwtools-ignore` directive in `file` suppresses a
    /// diagnostic with 1-based `line` and `code`. Reuses the same memo the
    /// trimmed-line lookup maintains, so per file it is one read at most.
    pub(crate) fn inline_suppressed(&mut self, file: &str, line: u32, code: &str) -> bool {
        self.trimmed(file, line);
        cwtools_validation::inline_ignore::inline_suppressed(&self.inline_ignored, line, code)
    }
}

/// Escape a field for CSV output.
fn csv_escape(s: &str) -> Cow<'_, str> {
    if s.contains([',', '"', '\n']) {
        Cow::Owned(format!("\"{}\"", s.replace('"', "\"\"")))
    } else {
        Cow::Borrowed(s)
    }
}

/// One rendered diagnostic row for the `validate` report. Reads only
/// file/severity/code/message/line/hash — never a diagnostic's `fix`, so a
/// `SuggestedFix` payload is inert here (locked in by `fix_payload_is_inert_in_report`).
pub(crate) struct Diag {
    pub(crate) file: cwtools_validation::FilePath,
    pub(crate) severity: cwtools_validation::ErrorSeverity,
    /// The catalog id (`""` for a diagnostic with no code). Both sources hand
    /// one out as `&'static str`, so the row never owns a copy.
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) line: u32,
    /// 1-based column, normalised from the emitting subsystem's convention.
    /// Only the `github` and `sarif` reports read it, so it stays out of the
    /// hash and the cli/csv/json rows.
    pub(crate) col: u32,
    pub(crate) hash: String,
    /// The previous line-number digest, for matching older baselines only.
    /// Empty unless an `--ignore-hashes` baseline was loaded — nothing else
    /// reads it, and computing one costs another relativize-and-digest per
    /// diagnostic.
    pub(crate) legacy_hash: String,
    /// Exclusive end of the diagnostic's span, 1-based line and column, when
    /// the emit site had a range to give. Only the `sarif` report reads it, so
    /// it stays out of the hash and the cli/csv/json rows.
    pub(crate) end: Option<(u32, u32)>,
    /// Secondary spans the message refers to, always inside this row's own
    /// file. Same contract as `end`: `sarif` only.
    pub(crate) related: Vec<Related>,
}

/// A secondary span on a report row, normalised to the 1-based columns the CI
/// formats use. `end` is exclusive, like [`Diag::end`].
pub(crate) struct Related {
    pub(crate) message: String,
    pub(crate) line: u32,
    pub(crate) col: u32,
    pub(crate) end: (u32, u32),
}

/// Whether a `--ignore-hashes` baseline suppresses `d`. Both digests are
/// accepted for the migration window: baselines written before the digest
/// became content-derived keep matching, while only the new one is ever
/// emitted, so a rewritten baseline converts itself.
pub(crate) fn is_ignored(ignored: &std::collections::HashSet<String>, d: &Diag) -> bool {
    ignored.contains(&d.hash) || ignored.contains(&d.legacy_hash)
}

/// The legacy line-number digest, or an empty string when no baseline is loaded
/// and nothing will ever compare against it.
fn legacy_hash_if_wanted(
    wanted: bool,
    root: &Path,
    file: &str,
    code: &str,
    message: &str,
    line: u32,
) -> String {
    if wanted {
        legacy_diag_hash(root, file, code, message, line)
    } else {
        String::new()
    }
}

/// Map a `ValidationError` to a report `Diag`, computing its hash from the
/// trimmed source line. Consumes the error (moves the message and the shared
/// file path). The `fix` field is deliberately dropped. `root` is the mod root
/// the hash is relativized against; the emitted `file` column is untouched.
/// `legacy` requests the older line-number digest, needed only when matching an
/// `--ignore-hashes` baseline.
pub(crate) fn validation_to_diag(
    root: &Path,
    err: ValidationError,
    line_text: &str,
    legacy: bool,
) -> Diag {
    let code = err.code.unwrap_or_default();
    let hash = diag_hash(root, &err.file, code, &err.message, line_text);
    let legacy_hash = legacy_hash_if_wanted(legacy, root, &err.file, code, &err.message, err.line);
    Diag {
        file: err.file,
        severity: err.severity,
        code,
        message: err.message,
        line: err.line,
        // Parser columns are 0-based; both CI formats are 1-based.
        col: err.col as u32 + 1,
        hash,
        legacy_hash,
        // The parser's exclusive 0-based end column lands on SARIF's exclusive
        // 1-based `endColumn` by the same +1; the line is 1-based already.
        end: err.end.map(|(line, col)| (line, col as u32 + 1)),
        related: err
            .related
            .into_iter()
            .map(|r| Related {
                message: r.message,
                line: r.line,
                col: r.col as u32 + 1,
                end: (r.end.0, r.end.1 as u32 + 1),
            })
            .collect(),
    }
}

/// Map a `LocDiagnostic` to a report `Diag`, computing its hash. Consumes the
/// diagnostic (moves file/message). The `fix` field is deliberately dropped,
/// same as `validation_to_diag`. `root` is the mod root the hash is
/// relativized against.
pub(crate) fn loc_diagnostic_to_diag(
    root: &Path,
    d: cwtools_localization::LocDiagnostic,
    line_text: &str,
    legacy: bool,
) -> Diag {
    let line = d.line as u32;
    let hash = diag_hash(root, &d.file, d.code, &d.message, line_text);
    let legacy_hash = legacy_hash_if_wanted(legacy, root, &d.file, d.code, &d.message, line);
    Diag {
        file: d.file.as_str().into(),
        severity: d.severity,
        code: d.code,
        message: d.message,
        line,
        // Loc diagnostics already count columns from 1.
        col: (d.col as u32).max(1),
        hash,
        legacy_hash,
        // A loc diagnostic carries a point, not a range.
        end: None,
        related: Vec::new(),
    }
}

/// Map a rules-config `RuleParseError` to a report `Diag`, computing its hash
/// from the trimmed source line. Consumes the error (moves the message). `root`
/// is the rules root the hash is relativized against, so a baseline survives the
/// ruleset being checked out somewhere else.
pub(crate) fn rule_error_to_diag(
    root: &Path,
    err: cwtools_rules::ruleset_loader::RuleParseError,
    line_text: &str,
    legacy: bool,
) -> Diag {
    let file = err.file.to_string_lossy().into_owned();
    let hash = diag_hash(root, &file, err.code, &err.message, line_text);
    let legacy_hash = legacy_hash_if_wanted(legacy, root, &file, err.code, &err.message, err.line);
    Diag {
        file: file.as_str().into(),
        severity: err.severity,
        code: err.code,
        message: err.message,
        line: err.line,
        // Rules columns are 0-based; both CI formats are 1-based.
        col: err.col as u32 + 1,
        hash,
        legacy_hash,
        // A rules-config problem carries a point, not a range.
        end: None,
        related: Vec::new(),
    }
}

/// Map a `LocService` fatal parse error (a file that couldn't even be
/// lenient-parsed, so there's no line number) to a report `Diag`. Always
/// Error-severity; `line` is 0 like other whole-file diagnostics, so there's no
/// source line to key the hash on. `root` is the mod root the hash is
/// relativized against.
pub(crate) fn loc_parse_error_to_diag(
    root: &Path,
    file: String,
    message: String,
    legacy: bool,
) -> Diag {
    let hash = diag_hash(root, &file, "", &message, "");
    let legacy_hash = legacy_hash_if_wanted(legacy, root, &file, "", &message, 0);
    Diag {
        file: file.as_str().into(),
        severity: ErrorSeverity::Error,
        code: "",
        message,
        line: 0,
        col: 1,
        hash,
        legacy_hash,
        end: None,
        related: Vec::new(),
    }
}

/// One CSV report row (trailing newline included).
pub(crate) fn csv_row(d: &Diag) -> String {
    format!(
        "{},{},{:?},{},{},{}\n",
        csv_escape(&d.file),
        d.line,
        d.severity,
        csv_escape(d.code),
        csv_escape(&d.message),
        d.hash
    )
}

/// One JSON report row (trailing newline included); `last` suppresses the comma.
pub(crate) fn json_row(d: &Diag, last: bool) -> String {
    #[derive(serde::Serialize)]
    struct JsonRow<'a> {
        file: &'a str,
        line: u32,
        severity: String,
        code: &'a str,
        message: &'a str,
        hash: &'a str,
    }
    let row = JsonRow {
        file: &d.file,
        line: d.line,
        severity: format!("{:?}", d.severity),
        code: d.code,
        message: &d.message,
        hash: &d.hash,
    };
    let mut s = String::from("  ");
    s.push_str(&serde_json::to_string(&row).unwrap());
    if !last {
        s.push(',');
    }
    s.push('\n');
    s
}

/// The ANSI color of a severity tag, matching the three levels a reader
/// actually triages by: red stops the build, yellow probably should, and the
/// two advisory levels recede.
fn severity_color(s: ErrorSeverity) -> &'static str {
    match s {
        ErrorSeverity::Error => "\x1b[31m",
        ErrorSeverity::Warning => "\x1b[33m",
        ErrorSeverity::Information | ErrorSeverity::Hint => "\x1b[90m",
    }
}

/// One grouped-CLI report row (the per-diagnostic line, not the file header).
/// `color` paints the severity tag; it is off for everything but a report going
/// to a terminal, so a redirected run is unchanged (see `run::color_enabled`).
pub(crate) fn cli_row(d: &Diag, color: bool) -> String {
    let code_part = if d.code.is_empty() {
        String::new()
    } else {
        format!("[{}] ", d.code)
    };
    let severity = if color {
        format!("{}[{:?}]\x1b[0m", severity_color(d.severity), d.severity)
    } else {
        format!("[{:?}]", d.severity)
    };
    format!(
        "    {} {}{} (line {})\n",
        severity, code_part, d.message, d.line
    )
}

/// Ordinal rank for `--min-severity` filtering: higher is more severe.
pub(crate) fn severity_rank(s: ErrorSeverity) -> u8 {
    match s {
        ErrorSeverity::Error => 3,
        ErrorSeverity::Warning => 2,
        ErrorSeverity::Information => 1,
        ErrorSeverity::Hint => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_parser::ast::{SourcePos, SourceRange};
    use cwtools_parser::fix::SuggestedFix;
    use std::path::PathBuf;

    /// Absolute path spelled the host's way. On Windows a leading `/` is not
    /// absolute (no drive), so tests must not hardcode it.
    fn abs(tail: &str) -> String {
        if cfg!(windows) {
            format!("C:/{tail}")
        } else {
            format!("/{tail}")
        }
    }

    fn err_base() -> ValidationError {
        ValidationError {
            message: "redundant default, remove it".to_string(),
            severity: ErrorSeverity::Information,
            line: 12,
            col: 4,
            file: "common/ideas/x.txt".into(),
            code: Some("CW282"),
            fix: None,
            end: None,
            related: Vec::new(),
        }
    }

    /// Bug: the digest used to be keyed on whatever path string an invocation
    /// happened to produce. The same file under the mod root, spelled
    /// absolute in one run, relative in another, `./`-prefixed in a third and
    /// backslash-separated in a fourth (as a Windows run would spell it),
    /// must all collapse to one digest.
    #[test]
    fn diag_hash_is_stable_across_path_spellings_of_the_same_file() {
        let root = PathBuf::from(abs("repo/mod"));
        let spellings = [
            abs("repo/mod/common/x.txt"),
            "common/x.txt".to_string(),
            abs("repo/mod/./common/x.txt"),
            r"\repo\mod\common\x.txt".to_string(),
        ];
        let hashes: Vec<String> = spellings
            .iter()
            .map(|f| diag_hash(&root, f, "CW282", "m", "cost = 150"))
            .collect();
        for (spelling, hash) in spellings.iter().zip(&hashes) {
            assert_eq!(
                *hash, hashes[0],
                "{spelling:?} must hash the same as {:?}",
                spellings[0]
            );
        }
    }

    /// A file outside the root (e.g. a vanilla install path reported
    /// alongside mod files) still produces a stable digest, not a panic.
    #[test]
    fn diag_hash_is_stable_for_a_file_outside_the_root() {
        let root = PathBuf::from(abs("repo/mod"));
        let a = diag_hash(
            &root,
            &abs("vanilla/common/x.txt"),
            "CW282",
            "m",
            "cost = 150",
        );
        let b = diag_hash(
            &root,
            &abs("vanilla/common/x.txt"),
            "CW282",
            "m",
            "cost = 150",
        );
        assert_eq!(a, b);
    }

    /// `diag_hash` strips the mod root, regardless of how the file spelling
    /// and the root spelling disagree.
    #[test]
    fn relative_file_strips_the_root_regardless_of_spelling() {
        let root = PathBuf::from(abs("repo/mod"));
        let want = "common/x.txt";
        assert_eq!(relative_file(&abs("repo/mod/common/x.txt"), &root), want);
        assert_eq!(
            relative_file("common/x.txt", &root),
            want,
            "already relative"
        );
        assert_eq!(
            relative_file(&abs("repo/mod/./common/x.txt"), &root),
            want,
            "a `./` component"
        );
        let root_trailing = PathBuf::from(abs("repo/mod/"));
        assert_eq!(
            relative_file(&abs("repo/mod/common/x.txt"), &root_trailing),
            want,
            "trailing separator on the root"
        );
        assert_eq!(
            relative_file(r"\repo\mod\common\x.txt", &root),
            want,
            "backslash-separated spelling"
        );
    }

    /// A file genuinely outside the root (e.g. a vanilla install path reported
    /// alongside mod files) falls back to the file string rather than panicking,
    /// and does so the same way every time.
    #[test]
    fn relative_file_falls_back_and_does_not_panic_when_outside_the_root() {
        let root = PathBuf::from(abs("repo/mod"));
        let outside = abs("vanilla/common/x.txt");
        assert_eq!(relative_file(&outside, &root), outside);
        assert_eq!(
            relative_file(&outside, &root),
            relative_file(&outside, &root)
        );
    }

    /// A colon in the root names a directory, which is legal on Unix; only a
    /// leading `X:` is a drive. Reading any colon as one rewrote every file
    /// outside the root before hashing it, so a vanilla path picked up a
    /// prefix built from the mod's own name.
    #[test]
    fn relative_file_reads_only_a_leading_drive_letter() {
        let root = PathBuf::from(abs("repo/my:mod"));
        let outside = abs("vanilla/common/x.txt");
        assert_eq!(relative_file(&outside, &root), outside, "outside the root");
        assert_eq!(
            relative_file(&abs("repo/my:mod/common/x.txt"), &root),
            "common/x.txt",
            "under the root"
        );
    }

    #[test]
    fn relative_file_handles_windows_drive_and_backslash() {
        // Windows drive-letter path and backslashes must still relativize.
        let root = PathBuf::from(abs("repo/mod"));
        let win = r"C:\repo\mod\common\x.txt";
        // On Unix the drive prefix is just part of the string and won't match,
        // so it falls back — that's correct for the host. On Windows it strips.
        let got = relative_file(win, &root);
        if cfg!(windows) {
            assert_eq!(got, "common/x.txt");
        } else {
            assert_eq!(got, win.replace('\\', "/"));
        }
        // Backslash-separated spelling of the same file must still hash the same
        // as the forward-slash absolute on the host's filesystem.
        let root2 = PathBuf::from(abs("repo/mod"));
        let fwd = abs("repo/mod/common/x.txt");
        let bwd = r"\repo\mod\common\x.txt";
        assert_eq!(
            relative_file(&fwd, &root2),
            relative_file(bwd, &root2),
            "forward vs backslash must relativize identically"
        );
    }

    /// Same diagnostic on the same source text, moved down two lines.
    #[test]
    fn hash_survives_line_motion() {
        let mut moved = err_base();
        moved.line += 2;
        let root = Path::new(".");
        let before = validation_to_diag(root, err_base(), "cost = 150", true);
        let after = validation_to_diag(root, moved, "cost = 150", true);
        assert_eq!(
            before.hash, after.hash,
            "inserting a line above a diagnostic must not change its digest"
        );
        assert_ne!(
            before.legacy_hash, after.legacy_hash,
            "the legacy digest is the one that moved"
        );
    }

    /// Editing the offending line is a real change: the baseline entry should
    /// stop matching so the diagnostic is re-triaged.
    #[test]
    fn hash_changes_when_the_source_line_changes() {
        let root = Path::new(".");
        let a = validation_to_diag(root, err_base(), "cost = 150", true);
        let b = validation_to_diag(root, err_base(), "cost = 200", true);
        assert_ne!(a.hash, b.hash);
    }

    /// Migration: a baseline written with the old line-number digest still
    /// suppresses its diagnostic, and only the new digest is emitted.
    #[test]
    fn legacy_hashes_still_match_but_are_not_emitted() {
        let root = Path::new(".");
        let d = validation_to_diag(root, err_base(), "cost = 150", true);
        let legacy = legacy_diag_hash(
            root,
            "common/ideas/x.txt",
            "CW282",
            "redundant default, remove it",
            12,
        );
        assert_eq!(d.legacy_hash, legacy);
        assert_ne!(
            d.hash, legacy,
            "the emitted digest is the content-derived one"
        );

        let baseline: std::collections::HashSet<String> = [legacy].into_iter().collect();
        assert!(
            is_ignored(&baseline, &d),
            "old baselines must keep matching"
        );

        let fresh: std::collections::HashSet<String> = [d.hash.clone()].into_iter().collect();
        assert!(is_ignored(&fresh, &d), "new baselines match too");
        // The report and --output-hashes only ever see the new digest.
        assert!(csv_row(&d).contains(&d.hash));
        assert!(!csv_row(&d).contains(&d.legacy_hash));
    }

    /// The legacy digest exists only to match an `--ignore-hashes` baseline, so
    /// a run without one does not compute it. Everything else about the row is
    /// unchanged, including the emitted digest.
    #[test]
    fn legacy_hash_is_skipped_without_a_baseline() {
        let root = Path::new(".");
        let with_baseline = validation_to_diag(root, err_base(), "cost = 150", true);
        let without = validation_to_diag(root, err_base(), "cost = 150", false);

        assert!(
            without.legacy_hash.is_empty(),
            "no baseline means no legacy digest, got {:?}",
            without.legacy_hash
        );
        assert!(!with_baseline.legacy_hash.is_empty());
        assert_eq!(
            with_baseline.hash, without.hash,
            "the emitted digest must not depend on whether a baseline was loaded"
        );
        assert_eq!(csv_row(&with_baseline), csv_row(&without));
    }

    /// Skipping the legacy digest must not change which diagnostics a baseline
    /// suppresses. The new digest still matches, and an unrelated baseline still
    /// does not — an empty `legacy_hash` must never be treated as a match.
    #[test]
    fn skipping_the_legacy_hash_does_not_change_suppression() {
        let root = Path::new(".");
        let d = validation_to_diag(root, err_base(), "cost = 150", false);

        let fresh: std::collections::HashSet<String> = [d.hash.clone()].into_iter().collect();
        assert!(
            is_ignored(&fresh, &d),
            "a baseline holding the new digest still suppresses"
        );

        let unrelated: std::collections::HashSet<String> =
            ["0000000000000000".to_string()].into_iter().collect();
        assert!(
            !is_ignored(&unrelated, &d),
            "an unrelated baseline must not suppress"
        );
        assert!(
            !is_ignored(&std::collections::HashSet::new(), &d),
            "an empty baseline must not suppress"
        );
    }

    /// The legacy digest is a compatibility contract with baselines already on
    /// disk, so its bytes are frozen, not just its shape. This value is FNV-1a-64
    /// over `common/ideas/x.txt|CW282|redundant default|12`, exactly what the
    /// old line-number `diag_hash` emitted.
    #[test]
    fn legacy_hash_digest_is_frozen() {
        assert_eq!(
            legacy_diag_hash(
                Path::new("."),
                "common/ideas/x.txt",
                "CW282",
                "redundant default",
                12
            ),
            "8e7fd969bd9ea463"
        );
    }

    /// Whole-file diagnostics (line 0) have no source line; the digest still
    /// distinguishes them by file/code/message.
    #[test]
    fn parse_error_hashes_are_distinct_without_a_source_line() {
        let root = Path::new(".");
        let a = loc_parse_error_to_diag(root, "l_english.yml".into(), "bad yaml".into(), true);
        let b = loc_parse_error_to_diag(root, "l_english.yml".into(), "worse yaml".into(), true);
        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn source_lines_trims_and_handles_missing_input() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("x.txt");
        std::fs::write(&file, "first\n    indented = yes\n").unwrap();
        let path = file.to_str().unwrap();

        let mut sources = SourceLines::default();
        assert_eq!(sources.trimmed(path, 2), "indented = yes");
        assert_eq!(sources.trimmed(path, 1), "first");
        assert_eq!(sources.trimmed(path, 0), "", "line 0 has no source line");
        assert_eq!(sources.trimmed(path, 99), "", "past the end of the file");
        assert_eq!(sources.trimmed("/no/such/file.txt", 1), "");
        // The slot re-fills when the file changes back.
        assert_eq!(sources.trimmed(path, 1), "first");
    }

    #[test]
    fn json_row_escapes_special_characters_via_serde() {
        let root = Path::new(".");
        let mut err = err_base();
        err.message = "say \"hi\"\\bye\n\r\t\u{0001}end".to_string();
        err.file = "a/b\"c.txt".into();
        let d = validation_to_diag(root, err, "x", false);
        let row = json_row(&d, true);
        // Must be valid JSON and round-trip.
        let v: serde_json::Value = serde_json::from_str(&row).unwrap();
        assert_eq!(v["message"], "say \"hi\"\\bye\n\r\t\u{0001}end");
        assert_eq!(v["file"], "a/b\"c.txt");
        // Raw bytes must not contain a literal newline inside the string value.
        assert!(row.contains("\\n"));
        assert!(row.contains("\\r"));
        assert!(row.contains("\\t"));
        assert!(row.contains("\\\""));
        assert!(row.contains("\\\\"));
    }

    #[test]
    fn json_report_rows_form_a_valid_json_array() {
        let root = Path::new(".");
        let d0 = validation_to_diag(root, err_base(), "x", false);
        let mut e1 = err_base();
        e1.message = "other".to_string();
        let d1 = validation_to_diag(root, e1, "y", false);
        let mut out = String::from("[\n");
        out.push_str(&json_row(&d0, false));
        out.push_str(&json_row(&d1, true));
        out.push_str("]\n");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    // Inertness guard: a fix payload, an end position and a related span must not
    // change the digest or any of the three text rows. `sarif` reads the end and
    // the related spans on purpose (see `report`); nothing else may, or an
    // existing `--ignore-hashes` baseline would stop matching the moment an emit
    // site started recording a range.
    #[test]
    fn fix_payload_is_inert_in_report() {
        let base = err_base();
        let mut with_extras = base.clone();
        with_extras.fix = Some(SuggestedFix::delete(
            "Remove redundant default",
            SourceRange {
                start: SourcePos { line: 12, col: 4 },
                end: SourcePos { line: 13, col: 0 },
            },
        ));
        with_extras.end = Some((12, 30));
        with_extras.related = vec![cwtools_validation::RelatedSpan {
            message: "defined here".to_string(),
            line: 4,
            col: 2,
            end: (4, 8),
        }];

        let root = Path::new(".");
        let d0 = validation_to_diag(root, base, "cost = 150", true);
        let d1 = validation_to_diag(root, with_extras, "cost = 150", true);

        assert_eq!(d0.hash, d1.hash, "hash must ignore the extras");
        assert_eq!(d0.legacy_hash, d1.legacy_hash);
        assert_eq!(csv_row(&d0), csv_row(&d1), "csv row must ignore the extras");
        assert_eq!(json_row(&d0, true), json_row(&d1, true));
        assert_eq!(
            cli_row(&d0, false),
            cli_row(&d1, false),
            "cli row must ignore the extras"
        );
        // The sarif report is the one that does read them.
        assert_eq!(
            d1.end,
            Some((12, 31)),
            "0-based exclusive col becomes 1-based"
        );
        assert_eq!(d1.related.len(), 1);
        assert_eq!(d1.related[0].col, 3);
        assert_eq!(d1.related[0].end, (4, 9));
    }

    fn row_diag(severity: ErrorSeverity) -> Diag {
        Diag {
            file: "x.txt".into(),
            severity,
            code: "CW100",
            message: "m".to_string(),
            line: 3,
            col: 1,
            hash: String::new(),
            legacy_hash: String::new(),
            end: None,
            related: Vec::new(),
        }
    }

    /// The uncolored row is the shape every existing baseline and CI log holds.
    #[test]
    fn cli_row_without_color_is_plain_text() {
        assert_eq!(
            cli_row(&row_diag(ErrorSeverity::Error), false),
            "    [Error] [CW100] m (line 3)\n"
        );
    }

    #[test]
    fn cli_row_with_color_wraps_only_the_severity_tag() {
        let row = cli_row(&row_diag(ErrorSeverity::Warning), true);
        assert_eq!(row, "    \x1b[33m[Warning]\x1b[0m [CW100] m (line 3)\n");
        // One escape pair: the message and the code stay unpainted.
        assert_eq!(row.matches('\x1b').count(), 2);
    }

    #[test]
    fn cli_row_colors_the_three_triage_levels_apart() {
        let color_of = |s| {
            cli_row(&row_diag(s), true)
                .split_once('[')
                .map(|(_, rest)| format!("[{}", rest.split('m').next().unwrap()))
                .unwrap()
        };
        assert_eq!(color_of(ErrorSeverity::Error), "[31");
        assert_eq!(color_of(ErrorSeverity::Warning), "[33");
        assert_eq!(color_of(ErrorSeverity::Information), "[90");
        assert_eq!(color_of(ErrorSeverity::Hint), "[90");
    }
}
