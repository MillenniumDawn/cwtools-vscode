//! Parsing of the `#`/`##`/`###` comment directives that precede rules: rule
//! `Options` (cardinality, scope, severity, ...), `replace_scope(s)`, required
//! scopes, and the documentation lines used for hover tooltips.

use super::*;
use crate::ruleset_loader::RuleParseError;
use cwtools_error_codes::CW603_RULES_INVALID_DIRECTIVE;
use std::path::Path;

/// Extract description from ### comments (## are options).
pub(crate) fn extract_description_from_comments(comments: &[String]) -> Option<String> {
    // Only `###` lines are documentation. `##` lines are rule options
    // (cardinality, scope, severity, ...) and must NOT leak into the hover
    // tooltip. This intentionally diverges from F# (RulesParser.fs collects
    // every `##` line), which polluted every tooltip with option text.
    let desc_lines: Vec<String> = comments
        .iter()
        .filter(|s| s.starts_with("###"))
        .map(|s| s.trim_start_matches('#').trim().to_string())
        .collect();
    match desc_lines.len() {
        0 => None,
        1 => Some(desc_lines[0].clone()),
        _ => Some(desc_lines.join("\n")),
    }
}

/// Iterate the bodies of exactly-`##` directive lines, newest-first.
///
/// Yields each comment line that begins with exactly `##` (not `#` or `###`),
/// with the `##` prefix stripped and leading whitespace trimmed. Iterating in
/// reverse gives newest-match-wins to the first caller that returns.
fn directive_bodies(comments: &[String]) -> impl Iterator<Item = &str> {
    comments.iter().rev().filter_map(|c| {
        let rest = c.strip_prefix("##")?;
        // Exclude `###` lines — those are documentation.
        if rest.starts_with('#') {
            return None;
        }
        Some(rest.trim_start())
    })
}

/// True when a bare `## key` or `## key = ...` directive appears in exactly-`##` comment lines.
///
/// Used for boolean flags (`## required`, `## optional`, `## primary`) that may
/// appear without an `= value` suffix.
pub(crate) fn has_directive(comments: &[String], key: &str) -> bool {
    directive_bodies(comments).any(|rest| {
        // Matches bare `## key` or `## key = ...` or `## key_something` — use
        // word-boundary logic: key must be followed by end-of-string, '=', or whitespace.
        rest.strip_prefix(key).is_some_and(|after| {
            after.is_empty()
                || after.starts_with('=')
                || after.starts_with(|c: char| c.is_whitespace())
        })
    })
}

/// Return the value of a `key = value` directive from exactly-`##` comment lines.
///
/// Rules:
/// - The line must start with exactly `##` (two hashes), not `#` or `###`.
/// - After stripping `##` and whitespace, the line must begin with `key`.
/// - Newest match wins (scan from end).
///
/// Returns the RHS string (trimmed, with one balanced pair of surrounding
/// quotes removed) when found, `None` otherwise, so `## scope = "country"`
/// reads the same as `## scope = country` (#273).
pub(crate) fn find_directive<'a>(comments: &'a [String], key: &str) -> Option<&'a str> {
    directive_bodies(comments).find_map(|rest| {
        let after_key = rest.strip_prefix(key)?.trim_start();
        let rhs = after_key.strip_prefix('=')?;
        Some(strip_quotes(rhs.trim()))
    })
}

/// Collect every `## key = value` directive into a map in a single pass.
///
/// `key` is the token before the first `=` (trimmed); `value` is the trimmed
/// RHS, unquoted the same way `find_directive` unquotes it. Forward iteration
/// with overwrite yields newest-match-wins, so for any `key` `map.get(key)`
/// returns exactly what `find_directive(comments, key)` would.
/// `###` documentation lines and plain `#` comments are skipped.
fn collect_directives(comments: &[String]) -> std::collections::HashMap<&str, &str> {
    let mut map = std::collections::HashMap::new();
    for c in comments {
        let Some(rest) = c.strip_prefix("##") else {
            continue;
        };
        if rest.starts_with('#') {
            continue;
        }
        let rest = rest.trim_start();
        let Some((key, rhs)) = rest.split_once('=') else {
            continue;
        };
        map.insert(key.trim_end(), strip_quotes(rhs.trim()));
    }
    map
}

/// Parse Options from comment lines preceding a rule.
/// CRITICAL: when NO cardinality comment is present, use min=1, max=1, strict_min=true (F# default).
pub(crate) fn options_from_comments(comments: &[String], is_comparison: bool) -> Options {
    // Collect every `## key = value` directive once (newest-wins via overwrite),
    // then look up each option below — avoids re-scanning `comments` per directive.
    // `directives.get(key)` is exactly equivalent to `find_directive(comments, key)`:
    // both key on the token before `=` and return the trimmed RHS.
    let directives = collect_directives(comments);

    // Cardinality: from exactly-## lines only, newest-match-wins.
    let (min, max, strict_min) = if let Some(spec) = directives.get("cardinality").copied() {
        if let Some((min_s, max_s)) = spec.split_once("..") {
            let min_s = min_s.trim();
            let (min_s, strict) = match min_s.strip_prefix('~') {
                Some(rest) => (rest, false),
                None => (min_s, true),
            };
            let min = min_s.parse::<i32>().unwrap_or(1);
            let max = if max_s.trim() == "inf" {
                i32::MAX
            } else {
                max_s.trim().parse::<i32>().unwrap_or(1)
            };
            (min, max, strict)
        } else {
            (1, 1, true)
        }
    } else {
        // No cardinality comment -> F# default: 1..1, strict
        (1, 1, true)
    };

    // Description: all ## lines joined
    let description = extract_description_from_comments(comments);

    // push_scope: from exactly-## lines only, newest-match-wins.
    let push_scope = directives
        .get("push_scope")
        .copied()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // replace_scopes / replace_scope: hand-parse key=value pairs
    let replace_scopes = parse_replace_scopes_from_comments(comments);

    // severity: from exactly-## lines only.
    let severity = directives
        .get("severity")
        .copied()
        .and_then(|sev| match sev {
            "error" => Some(Severity::Error),
            "warning" => Some(Severity::Warning),
            "info" | "information" => Some(Severity::Information),
            "hint" => Some(Severity::Hint),
            _ => None,
        });

    // required_scopes: ## scope = X or ## scope = { A B }
    let required_scopes = parse_required_scopes(comments);

    // reference_details: from exactly-## lines.
    let reference_details = directives
        .get("outgoingReferenceLabel")
        .map(|v| ReferenceDetail::Outgoing(v.to_string()))
        .or_else(|| {
            directives
                .get("incomingReferenceLabel")
                .map(|v| ReferenceDetail::Incoming(v.to_string()))
        });

    // error_if_only_match: from exactly-## lines.
    let error_if_only_match = directives
        .get("error_if_only_match")
        .copied()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // default_bool: `## default_bool = yes|no` marks the field's engine default
    // so setting it to that value emits an info hint (CW282).
    let default_bool = directives.get("default_bool").copied().and_then(|v| {
        match v.to_ascii_lowercase().as_str() {
            "yes" | "true" => Some(true),
            "no" | "false" => Some(false),
            _ => None,
        }
    });

    Options {
        min,
        max,
        strict_min,
        leafvalue: false,
        description,
        push_scope,
        replace_scopes: replace_scopes.map(Box::new),
        severity,
        required_scopes,
        comparison: is_comparison,
        reference_details: reference_details.map(Box::new),
        error_if_only_match,
        default_bool,
    }
}

pub(crate) fn parse_replace_scopes_from_comments(comments: &[String]) -> Option<ReplaceScopes> {
    // Use find_directive so only exactly-## lines are considered.
    // "replace_scopes" is longer so try it first to avoid "replace_scope" matching a prefix.
    let rhs = find_directive(comments, "replace_scopes")
        .or_else(|| find_directive(comments, "replace_scope"))?;

    // `rhs` is either `{ key = value ... }` or a bare value. Parse the inner pairs.
    let pairs_str = if rhs.starts_with('{') {
        let close = rhs.find('}')?;
        &rhs[1..close]
    } else {
        rhs
    };

    let mut root = None;
    let mut this = None;
    let mut froms = Vec::new();
    let mut prevs = Vec::new();

    // Parse space-separated `key = value` triples in a single streaming pass.
    // Peek the next two tokens without collecting: when `key =` is followed by a
    // value, consume all three; otherwise drop the current token and re-sync on
    // the next. Mirrors the prior `tokens[ti+1] == "="` / advance-by-3-or-1 logic.
    let mut tokens = pairs_str.split_whitespace().peekable();
    while let Some(key) = tokens.next() {
        if tokens.peek() != Some(&"=") {
            continue;
        }
        tokens.next(); // consume "="
        let Some(value) = tokens.next() else {
            break;
        };
        let value = strip_quotes(value);
        // Keys are case-insensitive: HOI4 config writes them uppercase
        // (`THIS = state ROOT = state`, operations.cwt) while other rules use
        // lowercase (`this = state`, state-history).
        match key.to_ascii_lowercase().as_str() {
            "this" => this = Some(value.to_string()),
            "root" => root = Some(value.to_string()),
            "from" => {
                if froms.is_empty() {
                    froms.push(value.to_string());
                } else {
                    froms[0] = value.to_string();
                }
            }
            "fromfrom" => {
                while froms.len() < 2 {
                    froms.push(String::new());
                }
                froms[1] = value.to_string();
            }
            "fromfromfrom" => {
                while froms.len() < 3 {
                    froms.push(String::new());
                }
                froms[2] = value.to_string();
            }
            "fromfromfromfrom" => {
                while froms.len() < 4 {
                    froms.push(String::new());
                }
                froms[3] = value.to_string();
            }
            "prev" => {
                if prevs.is_empty() {
                    prevs.push(value.to_string());
                } else {
                    prevs[0] = value.to_string();
                }
            }
            "prevprev" => {
                while prevs.len() < 2 {
                    prevs.push(String::new());
                }
                prevs[1] = value.to_string();
            }
            "prevprevprev" => {
                while prevs.len() < 3 {
                    prevs.push(String::new());
                }
                prevs[2] = value.to_string();
            }
            "prevprevprevprev" => {
                while prevs.len() < 4 {
                    prevs.push(String::new());
                }
                prevs[3] = value.to_string();
            }
            _ => {}
        }
    }

    if root.is_none() && this.is_none() && froms.is_empty() && prevs.is_empty() {
        return None;
    }

    Some(ReplaceScopes {
        root,
        this,
        froms,
        prevs,
    })
}

pub(crate) fn parse_required_scopes(comments: &[String]) -> Vec<String> {
    // Use find_directive so only exactly-## lines are considered and
    // newest-match-wins (scanning from the end).
    if let Some(rhs) = find_directive(comments, "scope") {
        if rhs.starts_with('{') && rhs.ends_with('}') {
            return rhs[1..rhs.len() - 1]
                .split_whitespace()
                .map(|s| strip_quotes(s).to_string())
                .collect();
        } else if !rhs.is_empty() {
            return vec![rhs.to_string()];
        }
    }
    Vec::new()
}

/// Whether a `## cardinality = <spec>` RHS is one `options_from_comments`
/// can't parse: it has a `min..max` shape, but `min` (after an optional `~`)
/// or `max` (unless it's `inf`) isn't an integer. `options_from_comments`
/// silently `unwrap_or(1)`s such a bound instead of erroring, so this is the
/// only way malformed input is ever surfaced.
fn cardinality_spec_is_malformed(spec: &str) -> bool {
    let Some((min_s, max_s)) = spec.split_once("..") else {
        // No `..` at all falls back to the documented 1..1 default rather
        // than an unwrap_or, so it isn't flagged here.
        return false;
    };
    let min_s = min_s.trim().strip_prefix('~').unwrap_or(min_s.trim());
    let max_s = max_s.trim();
    min_s.parse::<i32>().is_err() || (max_s != "inf" && max_s.parse::<i32>().is_err())
}

/// Whether a `## severity = <value>` RHS is not one of the values
/// `options_from_comments` recognises (it otherwise silently falls to `None`).
fn severity_value_is_unrecognized(value: &str) -> bool {
    !matches!(value, "error" | "warning" | "info" | "information" | "hint")
}

/// Scan every `##` directive comment in `ast` for a `cardinality` or `severity`
/// value `options_from_comments` can't parse, and report one `RuleParseError`
/// per bad line. Purely diagnostic — doesn't change what `options_from_comments`
/// resolves the directive to, so well-formed config is unaffected.
pub(crate) fn validate_comment_directives(ast: &ParsedFile, path: &Path) -> Vec<RuleParseError> {
    let mut errors = Vec::new();
    for comment in &ast.arena.comments {
        let text = comment.text.trim();
        let Some(rest) = text.strip_prefix("##") else {
            continue;
        };
        if rest.starts_with('#') {
            continue; // ### documentation line
        }
        let rest = rest.trim_start();
        let Some((key, rhs)) = rest.split_once('=') else {
            continue;
        };
        let key = key.trim_end();
        let rhs = strip_quotes(rhs.trim());
        let message = match key {
            "cardinality" if cardinality_spec_is_malformed(rhs) => {
                Some(format!("malformed `cardinality` bound `{rhs}`"))
            }
            "severity" if severity_value_is_unrecognized(rhs) => {
                Some(format!("unrecognized `severity` value `{rhs}`"))
            }
            _ => None,
        };
        if let Some(message) = message {
            errors.push(RuleParseError::new(
                &CW603_RULES_INVALID_DIRECTIVE,
                path.to_path_buf(),
                comment.pos.start.line,
                comment.pos.start.col,
                message,
            ));
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // ── find_directive ────────────────────────────────────────────────────────

    #[test]
    fn find_directive_basic() {
        assert_eq!(
            find_directive(&s(&["## cardinality = 0..1"]), "cardinality"),
            Some("0..1")
        );
    }

    #[test]
    fn find_directive_ignores_single_hash() {
        // # cardinality = 0..1 is a plain comment — must be ignored
        assert_eq!(
            find_directive(&s(&["# cardinality = 0..1"]), "cardinality"),
            None
        );
    }

    #[test]
    fn find_directive_ignores_triple_hash() {
        // ### cardinality = 0..1 is a doc line — must be ignored
        assert_eq!(
            find_directive(&s(&["### cardinality = 0..1"]), "cardinality"),
            None
        );
    }

    #[test]
    fn find_directive_newest_wins() {
        // Two ## cardinality lines: last one (by position) wins
        let comments = s(&["## cardinality = 1..1", "## cardinality = 0..inf"]);
        assert_eq!(find_directive(&comments, "cardinality"), Some("0..inf"));
    }

    #[test]
    fn find_directive_scope_not_from_doc() {
        // A ### doc line mentioning "scope" must not be picked up as ## scope
        let comments = s(&["### scope = the owning country", "## scope = country"]);
        assert_eq!(find_directive(&comments, "scope"), Some("country"));
    }

    #[test]
    fn find_directive_plain_comment_cardinality_ignored() {
        // A # plain comment mentioning cardinality must not be parsed
        let comments = s(&["# this has cardinality = 1..2 inside a sentence"]);
        assert_eq!(find_directive(&comments, "cardinality"), None);
    }

    // ── parse_replace_scopes_from_comments ────────────────────────────────────

    #[test]
    fn replace_scope_keys_are_case_insensitive() {
        // HOI4 config writes replace_scope keys uppercase (operations.cwt etc.).
        // They must parse the same as lowercase so ROOT/THIS/PREV apply and the
        // hover scope table shows them (cwtools-vscode#35).
        let comments = s(&["## replace_scope = { THIS = state ROOT = country PREV = state }"]);
        let rs = parse_replace_scopes_from_comments(&comments).expect("should parse");
        assert_eq!(rs.this.as_deref(), Some("state"));
        assert_eq!(rs.root.as_deref(), Some("country"));
        assert_eq!(rs.prevs.first().map(String::as_str), Some("state"));
    }

    // ── parse_required_scopes ─────────────────────────────────────────────────

    #[test]
    fn required_scopes_single() {
        assert_eq!(
            parse_required_scopes(&s(&["## scope = country"])),
            vec!["country"]
        );
    }

    #[test]
    fn required_scopes_block() {
        assert_eq!(
            parse_required_scopes(&s(&["## scope = { country state }"])),
            vec!["country", "state"]
        );
    }

    #[test]
    fn required_scopes_single_hash_ignored() {
        assert!(parse_required_scopes(&s(&["# scope = country"])).is_empty());
    }

    #[test]
    fn required_scopes_triple_hash_ignored() {
        assert!(parse_required_scopes(&s(&["### scope = the owning country"])).is_empty());
    }

    #[test]
    fn extract_description_crlf_line_endings() {
        // Windows CRLF: the parser keeps the trailing `\r` in comment text, so
        // `### doc\r` must still extract as documentation (precompute_comments
        // trims it in the real pipeline; this guards the extractor directly).
        let comments_with_crlf = vec![
            "### First doc line\r".to_string(),
            "## cardinality = 0..1\r".to_string(),
            "### Second doc line\r".to_string(),
        ];
        let desc = extract_description_from_comments(&comments_with_crlf).unwrap();
        assert_eq!(
            desc, "First doc line\nSecond doc line",
            "CRLF comments should extract as documentation"
        );
    }

    // ── default_bool (#26) ────────────────────────────────────────────────────

    #[test]
    fn default_bool_yes_parses() {
        let opts = options_from_comments(&s(&["## default_bool = yes"]), false);
        assert_eq!(opts.default_bool, Some(true));
    }

    #[test]
    fn default_bool_no_parses() {
        let opts = options_from_comments(&s(&["## default_bool = no"]), false);
        assert_eq!(opts.default_bool, Some(false));
    }

    #[test]
    fn default_bool_absent_is_none() {
        let opts = options_from_comments(&s(&["## cardinality = 0..1"]), false);
        assert_eq!(opts.default_bool, None);
    }

    // ── quoted directive values (#273) ────────────────────────────────────────

    #[test]
    fn find_directive_strips_surrounding_quotes() {
        assert_eq!(
            find_directive(&s(&[r#"## cardinality = "0..1""#]), "cardinality"),
            Some("0..1")
        );
    }

    #[test]
    fn quoted_cardinality_parses_like_unquoted() {
        let opts = options_from_comments(&s(&[r#"## cardinality = "~0..inf""#]), false);
        assert_eq!((opts.min, opts.max, opts.strict_min), (0, i32::MAX, false));
    }

    #[test]
    fn quoted_severity_parses_like_unquoted() {
        let opts = options_from_comments(&s(&[r#"## severity = "warning""#]), false);
        assert_eq!(opts.severity, Some(Severity::Warning));
    }

    #[test]
    fn quoted_push_scope_parses_like_unquoted() {
        let opts = options_from_comments(&s(&[r#"## push_scope = "country""#]), false);
        assert_eq!(opts.push_scope.as_deref(), Some("country"));
    }

    #[test]
    fn quoted_scope_parses_like_unquoted() {
        assert_eq!(
            parse_required_scopes(&s(&[r#"## scope = "country""#])),
            vec!["country"]
        );
    }

    #[test]
    fn quoted_scope_list_is_unquoted_whole_or_per_member() {
        assert_eq!(
            parse_required_scopes(&s(&[r#"## scope = "{ country state }""#])),
            vec!["country", "state"]
        );
        assert_eq!(
            parse_required_scopes(&s(&[r#"## scope = { "country" "state" }"#])),
            vec!["country", "state"]
        );
    }

    #[test]
    fn quoted_replace_scope_values_are_unquoted() {
        let comments = s(&[r#"## replace_scope = { THIS = "state" ROOT = "country" }"#]);
        let rs = parse_replace_scopes_from_comments(&comments).expect("should parse");
        assert_eq!(rs.this.as_deref(), Some("state"));
        assert_eq!(rs.root.as_deref(), Some("country"));
    }

    #[test]
    fn quoted_replace_scopes_block_is_unquoted() {
        let comments = s(&[r#"## replace_scopes = "{ THIS = state }""#]);
        let rs = parse_replace_scopes_from_comments(&comments).expect("should parse");
        assert_eq!(rs.this.as_deref(), Some("state"));
    }

    #[test]
    fn quoted_remaining_options_parse_like_unquoted() {
        let comments = s(&[
            r#"## default_bool = "yes""#,
            r#"## error_if_only_match = "scripted_effect""#,
            r#"## outgoingReferenceLabel = "triggers""#,
        ]);
        let opts = options_from_comments(&comments, false);
        assert_eq!(opts.default_bool, Some(true));
        assert_eq!(opts.error_if_only_match.as_deref(), Some("scripted_effect"));
        assert_eq!(
            opts.reference_details.as_deref(),
            Some(&ReferenceDetail::Outgoing("triggers".to_string()))
        );
    }

    // ── validate_comment_directives (R8) ──────────────────────────────────────

    fn parse(src: &str) -> ParsedFile {
        let table = cwtools_string_table::string_table::StringTable::new();
        cwtools_parser::parser::parse_string(src, &table)
    }

    #[test]
    fn malformed_cardinality_bound_is_flagged() {
        // "n" is neither an integer nor "inf" — options_from_comments would
        // otherwise silently unwrap_or(1) this into max=1.
        let ast = parse("## cardinality = 0..n\nfoo = bar\n");
        let errors = validate_comment_directives(&ast, std::path::Path::new("t.cwt"));
        assert_eq!(errors.len(), 1, "got: {:?}", errors);
        assert!(errors[0].message.contains("cardinality"));
        assert!(errors[0].message.contains("0..n"));
    }

    #[test]
    fn invalid_severity_value_is_flagged() {
        // "warn" is not a recognised severity — the correct spelling is "warning".
        let ast = parse("## severity = warn\nfoo = bar\n");
        let errors = validate_comment_directives(&ast, std::path::Path::new("t.cwt"));
        assert_eq!(errors.len(), 1, "got: {:?}", errors);
        assert!(errors[0].message.contains("severity"));
        assert!(errors[0].message.contains("warn"));
    }

    #[test]
    fn well_formed_cardinality_and_severity_are_not_flagged() {
        let ast = parse(
            "## cardinality = 0..inf\n## severity = warning\nfoo = bar\n\
             ## cardinality = ~1..5\nbaz = qux\n",
        );
        let errors = validate_comment_directives(&ast, std::path::Path::new("t.cwt"));
        assert!(errors.is_empty(), "got: {:?}", errors);
    }

    #[test]
    fn quoted_well_formed_directives_are_not_flagged() {
        // The readers unquote these (#273), so CW603 must not fire on them.
        let ast = parse("## cardinality = \"0..inf\"\n## severity = \"warning\"\nfoo = bar\n");
        let errors = validate_comment_directives(&ast, std::path::Path::new("t.cwt"));
        assert!(errors.is_empty(), "got: {:?}", errors);
    }

    #[test]
    fn quoted_malformed_cardinality_is_still_flagged() {
        let ast = parse("## cardinality = \"0..n\"\nfoo = bar\n");
        let errors = validate_comment_directives(&ast, std::path::Path::new("t.cwt"));
        assert_eq!(errors.len(), 1, "got: {:?}", errors);
        assert!(errors[0].message.contains("0..n"));
    }

    #[test]
    fn malformed_cardinality_diagnostic_does_not_change_parsed_rule() {
        // The diagnostic is additive: options_from_comments must still resolve
        // the malformed bound exactly as it did before (min=0, max=1 via the
        // existing unwrap_or(1) fallback, strict since there's no `~`).
        let opts = options_from_comments(&s(&["## cardinality = 0..n"]), false);
        assert_eq!((opts.min, opts.max, opts.strict_min), (0, 1, true));
    }

    #[test]
    fn invalid_severity_diagnostic_does_not_change_parsed_rule() {
        // options_from_comments must still fall through to None for an
        // unrecognized severity, same as before the diagnostic was added.
        let opts = options_from_comments(&s(&["## severity = warn"]), false);
        assert_eq!(opts.severity, None);
    }
}
