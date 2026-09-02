use cwtools_game::scope_engine::ScopeContext;
use cwtools_parser::ast::{Child, Leaf, ParsedFile, SourcePos, SourceRange, Value};
use cwtools_parser::fix::SuggestedFix;
use cwtools_rules::rules_types::*;
use cwtools_string_table::string_table::{StringTable, StringTokens};

use cwtools_error_codes::ErrorCode;
pub use cwtools_error_codes::ErrorSeverity;

pub type FilePath = std::sync::Arc<str>;

#[derive(Debug, Clone, PartialEq)]
pub struct ValidationError {
    pub message: String,
    pub severity: ErrorSeverity,
    pub line: u32,
    pub col: u16,
    pub file: std::sync::Arc<str>,
    pub code: Option<&'static str>,
    pub fix: Option<SuggestedFix>,
    pub end: Option<(u32, u16)>,
    pub related: Vec<RelatedSpan>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RelatedSpan {
    pub message: String,
    pub line: u32,
    pub col: u16,
    pub end: (u32, u16),
}

impl ValidationError {
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

    pub(crate) fn with_fix(mut self, fix: SuggestedFix) -> Self {
        self.fix = Some(fix);
        self
    }

    pub(crate) fn with_end(mut self, end: SourcePos) -> Self {
        self.end = Some((end.line, end.col));
        self
    }

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

pub(crate) fn key_token_end(leaf: &Leaf, key: &str, table: &StringTable) -> SourcePos {
    let raw_len = table
        .with_string(leaf.key.normal, |s| s.chars().count())
        .unwrap_or_else(|| key.chars().count());
    cwtools_parser::fix::key_token_range(leaf.pos.start, raw_len).end
}

pub(crate) fn decimal_places(s: &str) -> usize {
    match s.split_once('.') {
        Some((_, frac)) => frac.trim_end_matches('0').len(),
        None => 0,
    }
}

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
    let k = key.to_ascii_lowercase();
    ctx.registry.id_of(&k).is_some() || ctx.registry.links.contains_key(&k)
}

pub(crate) fn value_is_zero(value: &Value) -> bool {
    match value {
        Value::Int(n) => *n == 0,
        Value::Float(f) => *f == 0.0,
        Value::String(_) | Value::QString(_) => false,
        _ => false,
    }
}

pub(crate) use cwtools_index::path_contains_segment;

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

pub(crate) fn is_date_shape(s: &str) -> bool {
    let mut parts = s.splitn(4, '.');
    let (Some(y), Some(m), Some(d), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    y.parse::<i32>().is_ok() && m.parse::<u32>().is_ok() && d.parse::<u32>().is_ok()
}

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

pub(crate) fn enum_is_authoritative(def: &EnumDefinition) -> bool {
    def.values.len() > 5
}

pub(crate) fn enum_contains(
    ruleset: &cwtools_rules::rules_types::RuleSet,
    enum_name: &str,
    value: &str,
) -> bool {
    match ruleset.enum_by_name().get(enum_name) {
        Some(&idx) if !ruleset.enums[idx].values.is_empty() => {
            if ruleset.enum_values_contains_ci(idx, value) {
                return true;
            }
            if ruleset.enum_has_at_constant(idx) {
                return true;
            }
            if enum_is_authoritative(&ruleset.enums[idx]) {
                return !value.is_empty();
            }
            false
        }
        _ => true,
    }
}

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

pub(crate) fn starts_with_ci(s: &str, prefix: &str) -> bool {
    let (s, prefix) = (s.as_bytes(), prefix.as_bytes());
    s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

pub(crate) fn ends_with_ci(s: &str, suffix: &str) -> bool {
    let (s, suffix) = (s.as_bytes(), suffix.as_bytes());
    s.len() >= suffix.len() && s[s.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

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

    #[test]
    fn needle_longer_than_haystack_is_false() {
        assert!(!starts_with_ci("a", "abc"));
        assert!(!ends_with_ci("a", "abc"));
        assert!(!starts_with_ci("", "x"));
        assert!(!ends_with_ci("", "x"));
    }

    #[test]
    fn empty_needle_always_matches() {
        assert!(starts_with_ci("anything", ""));
        assert!(ends_with_ci("anything", ""));
        assert!(starts_with_ci("", ""));
        assert!(ends_with_ci("", ""));
    }

    #[test]
    fn non_ascii_is_compared_bytewise_without_splitting() {
        assert!(ends_with_ci("gfx/café.DDS", ".dds"));
        assert!(starts_with_ci("café/x.dds", "CAFé/"));
        assert!(!starts_with_ci("café/x.dds", "CAFÉ/"));
        assert!(!ends_with_ci("x.dés", ".des"));
        assert!(!ends_with_ci("x.dé", "dé!"));
        assert!(ends_with_ci("x.dé", "é"));
    }

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
