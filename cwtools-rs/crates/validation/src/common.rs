//! Shared leaf helpers and the [`ValidationError`] type used across the
//! validation submodules.

use cwtools_game::scope_engine::ScopeContext;
use cwtools_parser::ast::{Child, Leaf, ParsedFile, SourcePos, SourceRange, Value};
use cwtools_parser::fix::SuggestedFix;
use cwtools_rules::rules_types::*;
use cwtools_string_table::string_table::{StringTable, StringTokens};

use cwtools_error_codes::ErrorCode;
pub use cwtools_error_codes::ErrorSeverity;

/// The file path a run's diagnostics are tagged with. Built once per file and
/// cloned into every diagnostic.
pub type FilePath = std::sync::Arc<str>;

#[derive(Debug, Clone, PartialEq)]
pub struct ValidationError {
    pub message: String,
    pub severity: ErrorSeverity,
    pub line: u32,
    pub col: u16,
    /// Path of the file the diagnostic is in. Shared rather than owned: every
    /// diagnostic from one file names the same path, and the candidate errors
    /// that `pick_best_candidate` and the alias disjunction build and throw away
    /// far outnumber the ones that survive.
    pub file: std::sync::Arc<str>,
    /// CW### error code, e.g. "CW262" for an unexpected property node. The id is
    /// `&'static` (the catalog `ErrorCode.id`), so no per-error allocation.
    pub code: Option<&'static str>,
    /// Optional machine-applicable fix. Pure metadata: the report/hash path
    /// never reads it (see `error_hash` and the CLI `Diag`), so a diagnostic
    /// hashes and renders identically with or without one. The CLI `fix`
    /// subcommand and the LSP code-action provider consume it.
    pub fix: Option<SuggestedFix>,
    /// Optional end position of the diagnostic's span (exclusive, matching the
    /// `SourceRange` convention: 1-based line, 0-based col). Populated where the
    /// emit site holds the node's range (`leaf.pos` / `block.range`); left `None`
    /// where no clean range is in hand. Pure metadata like `fix`: the report/hash
    /// path never reads it (see `error_hash` and the CLI `Diag`), so a diagnostic
    /// hashes and renders identically with or without one. The LSP publishes a
    /// precise squiggle when set, and falls back to the whole line when `None`.
    pub end: Option<(u32, u16)>,
    /// Secondary spans the message is about: the `if` an `else` is missing, the
    /// spelling a path is indexed under. Empty for most codes. Pure metadata
    /// like `fix` and `end` (the report/hash path never reads it); the LSP
    /// publishes them as `relatedInformation`.
    pub related: Vec<RelatedSpan>,
}

/// A secondary span attached to a diagnostic, always in the same file as the
/// diagnostic it hangs off. Use it where the message names something the reader
/// has to go and look at; the `message` says what that place is, so it reads on
/// its own in the editor's related-information list.
#[derive(Debug, Clone, PartialEq)]
pub struct RelatedSpan {
    pub message: String,
    /// 1-based line, matching [`ValidationError::line`].
    pub line: u32,
    /// 0-based column, matching [`ValidationError::col`].
    pub col: u16,
    /// Exclusive end of the span, in the same convention as the start.
    pub end: (u32, u16),
}

impl ValidationError {
    /// Build a diagnostic from a catalog [`ErrorCode`]: pulls severity and id
    /// from the code and formats its template with `args`. Centralizes the
    /// code→severity mapping so call sites don't restate it.
    pub(crate) fn from_code(
        code: &ErrorCode,
        file: &FilePath,
        line: u32,
        col: u16,
        args: &[&str],
    ) -> Self {
        ValidationError {
            message: code.format(args),
            severity: code.severity,
            line,
            col,
            file: std::sync::Arc::clone(file),
            code: Some(code.id),
            fix: None,
            end: None,
            related: Vec::new(),
        }
    }

    /// Like [`from_code`](Self::from_code) but with an explicit `severity` and a
    /// pre-built `message`, for the sites whose severity is decided at runtime
    /// (cardinality) or whose message is assembled from a match arm. Still tags
    /// the diagnostic with the catalog `code.id`.
    pub(crate) fn from_code_with(
        code: &ErrorCode,
        severity: ErrorSeverity,
        file: &FilePath,
        line: u32,
        col: u16,
        message: String,
    ) -> Self {
        ValidationError {
            message,
            severity,
            line,
            col,
            file: std::sync::Arc::clone(file),
            code: Some(code.id),
            fix: None,
            end: None,
            related: Vec::new(),
        }
    }

    /// Attach a machine-applicable fix at the emit site, where the AST node's
    /// span is in scope. Chains onto a `from_code` / `from_code_with` call so the
    /// existing constructor signatures stay unchanged.
    pub(crate) fn with_fix(mut self, fix: SuggestedFix) -> Self {
        self.fix = Some(fix);
        self
    }

    /// Attach the diagnostic's end position (exclusive) from the emit site's node
    /// range, so the LSP can publish a precise squiggle instead of the whole line.
    /// Chains onto a `from_code` / `from_code_with` call; the convention matches
    /// `SourceRange` (1-based line, 0-based col — pass `leaf.pos.end` / `range.end`).
    /// Left unset (whole-line squiggle) where no clean range is in hand.
    pub(crate) fn with_end(mut self, end: SourcePos) -> Self {
        self.end = Some((end.line, end.col));
        self
    }

    /// Point at a second place in the same file that the reader needs to see to
    /// make sense of the message (the `if` a stray `else` is missing, the case
    /// a path is actually indexed under). Chains like [`with_end`](Self::with_end);
    /// `range` follows the same convention (`leaf.pos` / `key_token_range(…)`).
    pub(crate) fn with_related(mut self, message: impl Into<String>, range: SourceRange) -> Self {
        self.related.push(RelatedSpan {
            message: message.into(),
            line: range.start.line,
            col: range.start.col,
            end: (range.end.line, range.end.col),
        });
        self
    }
}

/// End of `leaf`'s raw key token (quotes included, from the interned source
/// string), for diagnostics that advise about the key: the squiggle covers the
/// key instead of the whole leaf/block span. Falls back to the caller's `key`
/// when the interned string is unavailable.
pub(crate) fn key_token_end(leaf: &Leaf, key: &str, table: &StringTable) -> SourcePos {
    let raw_len = table
        .with_string(leaf.key.normal, |s| s.chars().count())
        .unwrap_or_else(|| key.chars().count());
    cwtools_parser::fix::key_token_range(leaf.pos.start, raw_len).end
}

/// Number of significant decimal places in a numeric string; trailing zeros do
/// not count (`0.1230` has 3). Used for the CW270 32-bit precision check.
pub(crate) fn decimal_places(s: &str) -> usize {
    match s.split_once('.') {
        Some((_, frac)) => frac.trim_end_matches('0').len(),
        None => 0,
    }
}

/// Whether `key` names a scope (keyword, scope link, or iterator) rather than a
/// variable. A `variable_field` value naming a scope must not be flagged as an
/// unset variable (CW246).
pub(crate) fn resolves_as_scope_key(ctx: &ScopeContext, key: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "this",
        "root",
        "prev",
        "prevprev",
        "prevprevprev",
        "from",
        "fromfrom",
        "fromfromfrom",
        "fromfromfromfrom",
    ];
    if KEYWORDS.iter().any(|kw| key.eq_ignore_ascii_case(kw)) {
        return true;
    }
    // Only the registry lookups need a lowercased key; allocate at most once.
    let k = key.to_ascii_lowercase();
    ctx.registry.id_of(&k).is_some() || ctx.registry.links.contains_key(&k)
}

/// True when a leaf value is numerically zero (`0`, `0.0`, `"0"`, …). Used by
/// the CW235 zero-modifier check.
pub(crate) fn value_is_zero(value: &Value) -> bool {
    match value {
        Value::Int(n) => *n == 0,
        Value::Float(f) => *f == 0.0,
        Value::String(_) | Value::QString(_) => false,
        _ => false,
    }
}

/// Whole-segment path containment (prevents `events` from matching
/// `.../my_events_backup/x.txt`). One shared implementation with the indexer so
/// a file is indexed by the same type that validates it.
pub(crate) use cwtools_index::path_contains_segment;

/// Start (line, col) of a child node, for locating block-level diagnostics.
pub(crate) fn child_start_pos(child: &Child, ast: &ParsedFile) -> Option<(u32, u16)> {
    match child {
        Child::Leaf(i) => {
            let l = &ast.arena.leaves[*i as usize];
            Some((l.pos.start.line, l.pos.start.col))
        }
        Child::LeafValue(i) => {
            let lv = &ast.arena.leaf_values[*i as usize];
            Some((lv.pos.start.line, lv.pos.start.col))
        }
        _ => None,
    }
}

pub(crate) fn child_key_matches(
    child: &Child,
    ast: &ParsedFile,
    table: &StringTable,
    filter_key: &str,
) -> bool {
    match child {
        Child::Leaf(idx) => {
            let leaf = &ast.arena.leaves[*idx as usize];
            table
                .with_string(leaf.key.normal, |s| {
                    unquote_key(s).eq_ignore_ascii_case(unquote_key(filter_key))
                })
                .unwrap_or(false)
        }
        _ => false,
    }
}

/// A block key that isn't a known scope command but resolves to a scope via the
/// game data: a numeric state/province id, a 2-4 character upper-case
/// alphanumeric country/state tag, or a `prefix:data` reference. Plain
/// effect/trigger names are excluded.
pub(crate) fn looks_like_data_ref(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    key.contains(':')
        || key.bytes().all(|b| b.is_ascii_digit())
        || ((2..=4).contains(&key.len())
            && key
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            && key.bytes().any(|b| b.is_ascii_uppercase()))
}

/// Check that a string has the YYYY.MM.DD shape for a CW date field.
pub(crate) fn is_date_shape(s: &str) -> bool {
    // Exactly YYYY.MM.DD — three numeric parts separated by dots.
    let mut parts = s.splitn(4, '.');
    let (Some(y), Some(m), Some(d), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    y.parse::<i32>().is_ok() && m.parse::<u32>().is_ok() && d.parse::<u32>().is_ok()
}

/// Check that a string has the YYYY.MM.DD or YYYY.MM.DD.HH shape for a CW
/// datetime field. Mirrors F# `IsValidDateTime` which accepts both 3 and 4
/// dot-separated numeric parts (3-part dates are valid for datetime fields).
pub(crate) fn is_datetime_shape(s: &str) -> bool {
    let mut parts = s.splitn(5, '.');
    match (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) {
        (Some(_), Some(_), Some(_), None, None) => is_date_shape(s),
        (Some(y), Some(m), Some(d), Some(h), None) => {
            y.parse::<i32>().is_ok()
                && m.parse::<u32>().is_ok()
                && d.parse::<u32>().is_ok()
                && h.parse::<u32>().is_ok()
        }
        _ => false,
    }
}

/// Size heuristic shared by every enum-membership check: a populated enum is
/// treated as authoritative only when it is small (≤ 5 members). Larger enums
/// are likely incomplete game-data catalogues (equipment_categories, tech
/// folders, idea tokens, …) that the CWT rules rarely enumerate in full, so an
/// unlisted value is accepted rather than flagged. Keep this in one place so the
/// `enum_contains` / `parsed_pattern_matches` / `field_matches_key` sites stay
/// in agreement.
pub(crate) fn enum_is_authoritative(def: &EnumDefinition) -> bool {
    def.values.len() > 5
}

/// Enum membership test. An absent or empty enum (members come from game data
/// that isn't statically loaded — provinces, ship_units, ...) is permissive.
///
/// For populated enums, we use a size heuristic: small enums (≤ 5 members) are
/// treated as authoritative — an unlisted value is a genuine error.  Larger
/// enums are likely incomplete game-data catalogues (equipment_categories,
/// tech folders, idea tokens, …) and are treated as advisory — any non-empty
/// value is accepted, because the CWT rules rarely enumerate every member.
pub(crate) fn enum_contains(
    ruleset: &cwtools_rules::rules_types::RuleSet,
    enum_name: &str,
    value: &str,
) -> bool {
    match ruleset.enum_by_name().get(enum_name) {
        Some(&idx) if !ruleset.enums[idx].values.is_empty() => {
            // Enum membership is case-insensitive (F# lowercases both the enum
            // values and the checked key — FieldValidators.fs `getLowerKey` +
            // RuleValidationService.fs `.lower`). e.g. `containerOrientations`
            // is authored UPPER_LEFT/CENTER but files use upper_left/center.
            if ruleset.enum_values_contains_ci(idx, value) {
                return true;
            }
            // An enum whose members are `@`-prefixed scripted constants (e.g.
            // `enum[command_cap_increase] = { @tier1_cp_cap_increase ... }`) accepts
            // the resolved literal value too (`command_cap_increase = 10`), which we
            // can't resolve statically — be permissive.
            if ruleset.enum_has_at_constant(idx) {
                return true;
            }
            // Large enums are likely incomplete game-data catalogues — accept any
            // non-empty value rather than flag every unlisted member.
            // Small enums (≤ 5 members) are authoritative; an unlisted value is
            // a genuine error.
            if enum_is_authoritative(&ruleset.enums[idx]) {
                return !value.is_empty();
            }
            false
        }
        _ => true,
    }
}

/// Zero-copy variant of `match_text`: borrows the string from the table,
/// strips surrounding quotes via a slice (no allocation), and passes the
/// resulting `&str` to `f`.  Returns `f`'s value, or the default if the id
/// is out of range.
pub(crate) fn with_match_text<R: Default>(
    table: &StringTable,
    t: &StringTokens,
    f: impl FnOnce(&str) -> R,
) -> R {
    table
        .with_string(t.normal, |s| {
            let unquoted = if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
                &s[1..s.len() - 1]
            } else {
                s
            };
            f(unquoted)
        })
        .unwrap_or_default()
}

/// `s` starts with `prefix`, ASCII-case-insensitively. Byte-wise, so it matches
/// what `to_ascii_lowercase().starts_with(..)` decides without the two owned
/// copies that spelling allocates.
pub(crate) fn starts_with_ci(s: &str, prefix: &str) -> bool {
    let (s, prefix) = (s.as_bytes(), prefix.as_bytes());
    s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// `s` ends with `suffix`, ASCII-case-insensitively. See [`starts_with_ci`].
pub(crate) fn ends_with_ci(s: &str, suffix: &str) -> bool {
    let (s, suffix) = (s.as_bytes(), suffix.as_bytes());
    s.len() >= suffix.len() && s[s.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

/// Strip a balanced pair of surrounding double-quotes from a child key.
pub(crate) fn unquote_key(s: &str) -> &str {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

pub(crate) fn leaf_value_to_string(value: &Value, table: &StringTable) -> String {
    match value {
        Value::String(t) | Value::QString(t) => table.get_string(t.normal).unwrap_or_default(),
        Value::Float(f) => f.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Clause(_) => "{...}".to_string(),
    }
}

/// Run `f` on the leaf value's string form without a heap String for the
/// dominant String/QString case. The bytes are copied into a stack buffer
/// while the table's read guard is held, then `f` runs after the guard drops
/// (holding the guard through `f` risks lock nesting, see rule_core).
pub(crate) fn with_leaf_value_str<R>(
    value: &Value,
    table: &StringTable,
    f: impl FnOnce(&str) -> R,
) -> R {
    match value {
        Value::String(t) | Value::QString(t) => {
            let mut buf: smallvec::SmallVec<[u8; 64]> = smallvec::SmallVec::new();
            table.with_string(t.normal, |s| buf.extend_from_slice(s.as_bytes()));
            f(std::str::from_utf8(&buf).unwrap_or_default())
        }
        other => f(&leaf_value_to_string(other, table)),
    }
}

pub(crate) fn severity_to_error(sev: &Severity) -> ErrorSeverity {
    sev.into()
}

pub fn error_hash(error: &ValidationError) -> String {
    let sev_str = match error.severity {
        ErrorSeverity::Error => "error",
        ErrorSeverity::Warning => "warning",
        ErrorSeverity::Information => "information",
        ErrorSeverity::Hint => "hint",
    };
    format!(
        "{}|{}|{}|{}",
        sev_str, error.file, error.line, error.message
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference spelling these two replace. Every case below asserts the
    /// pair agrees with it, so the helpers can't drift from the semantics the
    /// filepath checks were written against.
    fn starts_ref(s: &str, prefix: &str) -> bool {
        s.to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase())
    }
    fn ends_ref(s: &str, suffix: &str) -> bool {
        s.to_ascii_lowercase()
            .ends_with(&suffix.to_ascii_lowercase())
    }

    #[test]
    fn starts_with_ci_folds_ascii_case() {
        assert!(starts_with_ci("GFX/interface/x.dds", "gfx/"));
        assert!(starts_with_ci("gfx/interface/x.dds", "GFX/"));
        assert!(starts_with_ci("gfx/interface/x.dds", "gfx/"));
        assert!(!starts_with_ci("sound/x.wav", "gfx/"));
    }

    #[test]
    fn ends_with_ci_folds_ascii_case() {
        assert!(ends_with_ci("gfx/x.DDS", ".dds"));
        assert!(ends_with_ci("gfx/x.dds", ".DDS"));
        assert!(ends_with_ci("sound/zom_vo.ASSET", ".asset"));
        assert!(!ends_with_ci("gfx/x.dds", ".tga"));
    }

    /// A needle longer than the haystack must answer false, not panic on the
    /// slice. This is the boundary the byte-slice form has to guard itself.
    #[test]
    fn needle_longer_than_haystack_is_false() {
        assert!(!starts_with_ci("a", "abc"));
        assert!(!ends_with_ci("a", "abc"));
        assert!(!starts_with_ci("", "x"));
        assert!(!ends_with_ci("", "x"));
    }

    /// An empty needle matches anything, the same as `str::starts_with("")`.
    #[test]
    fn empty_needle_always_matches() {
        assert!(starts_with_ci("anything", ""));
        assert!(ends_with_ci("anything", ""));
        assert!(starts_with_ci("", ""));
        assert!(ends_with_ci("", ""));
    }

    /// Non-ASCII bytes are compared verbatim (ASCII folding leaves them alone)
    /// and slicing by byte offset must not split one. `é` is two bytes, so a
    /// needle whose length crosses it would land mid-character if the offset
    /// were computed in characters instead of bytes.
    #[test]
    fn non_ascii_is_compared_bytewise_without_splitting() {
        // Identical non-ASCII bytes match, and the ASCII tail still folds.
        assert!(ends_with_ci("gfx/café.DDS", ".dds"));
        assert!(starts_with_ci("café/x.dds", "CAFé/"));
        // ASCII folding does not reach non-ASCII case: É (0xC9) is not é (0xE9).
        assert!(!starts_with_ci("café/x.dds", "CAFÉ/"));
        assert!(!ends_with_ci("x.dés", ".des"));
        // A needle exactly as long as a trailing multi-byte character.
        assert!(!ends_with_ci("x.dé", "dé!"));
        assert!(ends_with_ci("x.dé", "é"));
    }

    /// Both helpers must decide exactly what the allocating spelling decided.
    #[test]
    fn helpers_agree_with_the_lowercasing_spelling() {
        let cases = [
            ("gfx/interface/Button.DDS", ".dds"),
            ("gfx/interface/Button.DDS", ".tga"),
            ("gfx/interface/Button.DDS", "gfx/"),
            ("gfx/interface/Button.DDS", "GFX/INTERFACE/"),
            ("", ""),
            ("", ".dds"),
            (".dds", ".dds"),
            ("café.DDS", ".dds"),
            ("ΑΣ.dds", ".DDS"),
        ];
        for (s, needle) in cases {
            assert_eq!(
                starts_with_ci(s, needle),
                starts_ref(s, needle),
                "starts_with_ci({s:?}, {needle:?})"
            );
            assert_eq!(
                ends_with_ci(s, needle),
                ends_ref(s, needle),
                "ends_with_ci({s:?}, {needle:?})"
            );
        }
    }

    #[test]
    fn data_ref_requires_a_complete_country_tag_shape() {
        for key in ["GER", "D01", "G1R2", "42", "event_target:foo"] {
            assert!(looks_like_data_ref(key), "{key:?} should be a data ref");
        }
        for key in ["A", "country_Block", "uSA", "GER_tag", "TOOLONG"] {
            assert!(
                !looks_like_data_ref(key),
                "{key:?} should not be a data ref"
            );
        }
    }
}
