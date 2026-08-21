//! Localization string command parser.
//!
//! Parses `$ref$` references and `[command]` blocks inside loc strings.
//! Supports:
//! * `$ref_key$`                   – reference to another loc key
//! * `[command]`                   – simple command
//! * `[command|format]`            – command with format specifier
//! * `[Scope.Owner.GetName]`       – Jomini command chains (CK3/VIC3)
//! * `[function(param1, param2)]`  – Jomini function calls
//! * `[?variable]`                 – event_target / saved variable reference
//! * `[event_target:foo]`          – named event target reference
//!
//! Handles both the original Paradox syntax (`[GetName]`) and the newer
//! Jomini syntax.

/// Parsed element inside a loc string.
///
/// `Chars`/`Ref`/`Command` borrow directly from the source string (they are
/// always substrings of the input), so parsing a loc value allocates nothing
/// for plain-text runs. `JominiCommand` keeps owned `String`s because the
/// Jomini parser rebuilds segment text character-by-character.
#[derive(Debug, Clone, PartialEq)]
pub enum LocElement<'a> {
    /// Plain text characters.
    Chars(&'a str),
    /// `$ref$` reference to another loc key.
    Ref(&'a str),
    /// `[command]` block (non-Jomini).
    Command(&'a str),
    /// `[Scope.Owner.GetName]` Jomini command chain or function call.
    JominiCommand(Vec<JominiCommand>),
}

/// A single Jomini command / function call.
#[derive(Debug, Clone, PartialEq)]
pub struct JominiCommand {
    pub key: String,
    pub params: Vec<JominiParam>,
}

/// Parameter to a Jomini function.
#[derive(Debug, Clone, PartialEq)]
pub enum JominiParam {
    /// A string literal, e.g. `'foo'`.
    Literal(String),
    /// A nested command chain, e.g. `Scope.Owner`.
    Commands(Vec<JominiCommand>),
}

/// Parse a loc string and return all elements.
///
/// This is a tolerant hand-written parser that handles:
/// * unescaped `$` inside text
/// * nested brackets
/// * Jomini function chains
///
/// # Arguments
/// * `s` – the raw description string (may include surrounding quotes)
pub fn parse_loc_elements(s: &str) -> Vec<LocElement<'_>> {
    let mut elements = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0; // byte offset; always lands on a char boundary
    // Start of the literal run being accumulated. Everything that is not a ref
    // or a command joins it, so a line of stray brackets costs one element
    // rather than one per bracket.
    let mut literal_start: Option<usize> = None;
    // Offsets of every `[` that never closes, descending, built the first time a
    // bracket fails to parse. Retrying those is what made a run of `[`
    // quadratic: each attempt scans to the end of the string before giving up.
    // A value with balanced brackets never builds this and never allocates.
    let mut unmatched: Option<Vec<usize>> = None;

    while i < bytes.len() {
        let parsed = match bytes[i] {
            b'$' => parse_ref(s, i),
            b'[' if unmatched.as_ref().is_none_or(|u| u.last() != Some(&i)) => parse_bracket(s, i),
            _ => None,
        };

        match parsed {
            Some((elem, new_i)) => {
                if let Some(start) = literal_start.take() {
                    elements.push(LocElement::Chars(&s[start..i]));
                }
                elements.push(elem);
                i = new_i;
            }
            None => {
                if bytes[i] == b'[' {
                    let u = unmatched.get_or_insert_with(|| unmatched_opens(bytes));
                    while u.last().is_some_and(|&p| p <= i) {
                        u.pop();
                    }
                }
                literal_start.get_or_insert(i);
                // `next_special` only ever stops on an ASCII byte, so its result
                // is a char boundary even when `i + 1` lands mid-sequence, and
                // it is always past `i` — the loop cannot stall on a lone `]`.
                i = next_special(s, i + 1);
            }
        }
    }

    if let Some(start) = literal_start {
        elements.push(LocElement::Chars(&s[start..]));
    }

    elements
}

/// Byte offsets of every `[` with no matching `]`, in descending order.
///
/// One reverse pass: a `]` makes a closer available and a `[` takes one when it
/// can, which pairs brackets exactly the way [`parse_bracket`]'s depth count
/// does. So an offset listed here is one where `parse_bracket` would scan to the
/// end of the string and fail. Descending order lets the caller pop from the
/// back as it advances.
fn unmatched_opens(bytes: &[u8]) -> Vec<usize> {
    let mut unmatched = Vec::new();
    let mut closers = 0usize;

    for (i, &b) in bytes.iter().enumerate().rev() {
        match b {
            b']' => closers += 1,
            b'[' if closers > 0 => closers -= 1,
            b'[' => unmatched.push(i),
            _ => {}
        }
    }

    unmatched
}

/// Return the byte offset of the next `$`, `[`, or `]` at or after `start`,
/// or `s.len()` if none.  Safe because `$`/`[`/`]` are ASCII and can never
/// appear as a continuation byte of a multi-byte UTF-8 sequence.
fn next_special(s: &str, start: usize) -> usize {
    s.as_bytes()[start..]
        .iter()
        .position(|&b| matches!(b, b'$' | b'[' | b']'))
        .map(|off| start + off)
        .unwrap_or(s.len())
}

/// Parse a `$ref$` starting at `s[start]` where `s.as_bytes()[start] == b'$'`.
///
/// Mirrors F# `dollarColour`: the ref name ends at `|` or `$`.
/// So `$MY_KEY|Y$` yields `Ref("MY_KEY")`.
fn parse_ref(s: &str, start: usize) -> Option<(LocElement<'_>, usize)> {
    let bytes = s.as_bytes();
    let content_start = start + 1; // skip opening '$'

    // Find end of key: '|' or '$'
    let key_end = bytes[content_start..]
        .iter()
        .position(|&b| b == b'$' || b == b'|')
        .map(|off| content_start + off)?;

    let key = &s[content_start..key_end];

    // A literal `$` (e.g. a currency sign) followed by non-identifier text is
    // not a ref. Loc keys, modifier names and idea names are all `[A-Za-z0-9_.]`,
    // so reject anything else: `$[?var|-3]`, `$§Y[?VAR|0]§!`, `$5 today$`.
    // The caller then treats the `$` as literal text.
    if !is_loc_ref_key(key) {
        return None;
    }

    if bytes[key_end] == b'|' {
        // Skip colour suffix up to and including the closing '$'
        let after_pipe = key_end + 1;
        let close = bytes[after_pipe..]
            .iter()
            .position(|&b| b == b'$')
            .map(|off| after_pipe + off)?;
        Some((LocElement::Ref(key), close + 1))
    } else {
        // bytes[key_end] == b'$' — consume it
        Some((LocElement::Ref(key), key_end + 1))
    }
}

/// Whether `key` is a plausible `$ref$` name: non-empty and made only of
/// loc-key identifier characters (`[A-Za-z0-9_.]`). Loc keys, modifier names
/// and idea names all fit this; literal-`$` constructs (currency, colour codes,
/// `[?...]` brackets) do not.
fn is_loc_ref_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
}

/// Parse a `[...]` block starting at `s[start]` where `s.as_bytes()[start] == b'['`.
fn parse_bracket(s: &str, start: usize) -> Option<(LocElement<'_>, usize)> {
    let bytes = s.as_bytes();
    let mut depth = 1usize;
    let mut i = start + 1;

    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'[' => depth += 1,
            b']' => depth -= 1,
            _ => {}
        }
        i += 1;
    }

    if depth != 0 {
        return None; // unmatched bracket
    }

    // i points one past the closing ']'; content is s[start+1..i-1]
    let content = &s[start + 1..i - 1];

    if (content.contains('.') || content.contains('('))
        && let Ok(commands) = parse_jomini(content)
    {
        return Some((LocElement::JominiCommand(commands), i));
    }

    let command = content.find('|').map(|p| &content[..p]).unwrap_or(content);

    Some((LocElement::Command(command), i))
}

#[derive(Debug, Clone, PartialEq)]
pub enum JominiParseError {
    UnclosedParen,
}

impl std::fmt::Display for JominiParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnclosedParen => f.write_str("unclosed parenthesis in Jomini function"),
        }
    }
}

impl std::error::Error for JominiParseError {}

/// Parse Jomini command chain / function call.
///
/// Examples:
/// * `Scope.Owner.GetName`
/// * `GetName('param')`
/// * `GetName(Scope.Owner.GetAge)`
fn parse_jomini(input: &str) -> Result<Vec<JominiCommand>, JominiParseError> {
    let mut commands = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '.' => {
                if !current.is_empty() {
                    commands.push(JominiCommand {
                        key: std::mem::take(&mut current),
                        params: Vec::new(),
                    });
                }
            }
            '(' => {
                let key = std::mem::take(&mut current);
                let params = parse_jomini_params(&mut chars)?;
                commands.push(JominiCommand { key, params });
            }
            ' ' | ',' => {}
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        commands.push(JominiCommand {
            key: current,
            params: Vec::new(),
        });
    }

    Ok(commands)
}

fn parse_jomini_params(
    chars: &mut std::iter::Peekable<std::str::Chars>,
) -> Result<Vec<JominiParam>, JominiParseError> {
    let mut params = Vec::new();
    let mut current = String::new();

    for ch in chars.by_ref() {
        match ch {
            ')' => {
                if !current.trim().is_empty() {
                    params.push(parse_jomini_param(&current)?);
                }
                return Ok(params);
            }
            ',' => {
                if !current.trim().is_empty() {
                    params.push(parse_jomini_param(&current)?);
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    Err(JominiParseError::UnclosedParen)
}

fn parse_jomini_param(s: &str) -> Result<JominiParam, JominiParseError> {
    let trimmed = s.trim();
    // A lone `'` satisfies both starts_with and ends_with; it takes two quotes to
    // be a quoted literal, not one character playing both roles.
    if trimmed.len() >= 2 && trimmed.starts_with('\'') && trimmed.ends_with('\'') {
        Ok(JominiParam::Literal(
            trimmed[1..trimmed.len() - 1].to_string(),
        ))
    } else if trimmed.contains('.') {
        let commands = parse_jomini(trimmed)?;
        Ok(JominiParam::Commands(commands))
    } else {
        Ok(JominiParam::Literal(trimmed.to_string()))
    }
}

/* ======================================================================== */
/* Tests                                                                   */
/* ======================================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_ref() {
        let elems = parse_loc_elements("$FOO$");
        assert_eq!(elems, vec![LocElement::Ref("FOO")]);
    }

    #[test]
    fn test_simple_command() {
        let elems = parse_loc_elements("[GetName]");
        assert_eq!(elems, vec![LocElement::Command("GetName")]);
    }

    #[test]
    fn test_command_with_format() {
        let elems = parse_loc_elements("[GetName|Y]");
        assert_eq!(elems, vec![LocElement::Command("GetName")]);
    }

    #[test]
    fn test_mixed_text_and_commands() {
        let elems = parse_loc_elements("Hello [GetName], welcome!");
        assert_eq!(elems.len(), 3);
        assert_eq!(elems[0], LocElement::Chars("Hello "));
        assert_eq!(elems[1], LocElement::Command("GetName"));
        assert_eq!(elems[2], LocElement::Chars(", welcome!"));
    }

    #[test]
    fn test_ref_and_command() {
        let elems = parse_loc_elements("$TITLE$ [GetName]");
        assert_eq!(elems.len(), 3);
        assert_eq!(elems[0], LocElement::Ref("TITLE"));
        assert_eq!(elems[1], LocElement::Chars(" "));
        assert_eq!(elems[2], LocElement::Command("GetName"));
    }

    #[test]
    fn test_jomini_chain() {
        let elems = parse_loc_elements("[Scope.Owner.GetName]");
        assert_eq!(elems.len(), 1);
        if let LocElement::JominiCommand(cmds) = &elems[0] {
            assert_eq!(cmds.len(), 3);
            assert_eq!(cmds[0].key, "Scope");
            assert_eq!(cmds[1].key, "Owner");
            assert_eq!(cmds[2].key, "GetName");
        } else {
            panic!("expected JominiCommand");
        }
    }

    #[test]
    fn test_jomini_function() {
        let elems = parse_loc_elements("[GetName('foo')]");
        assert_eq!(elems.len(), 1);
        if let LocElement::JominiCommand(cmds) = &elems[0] {
            assert_eq!(cmds.len(), 1);
            assert_eq!(cmds[0].key, "GetName");
            assert_eq!(cmds[0].params.len(), 1);
            assert_eq!(cmds[0].params[0], JominiParam::Literal("foo".to_string()));
        } else {
            panic!("expected JominiCommand");
        }
    }

    #[test]
    fn jomini_parse_error_display() {
        assert_eq!(
            JominiParseError::UnclosedParen.to_string(),
            "unclosed parenthesis in Jomini function"
        );
        // The error is produced by an unterminated '(' chain; parse_bracket
        // then falls back to a plain Command so the value still parses.
        let elems = parse_loc_elements("[Scope.GetName(]");
        assert_eq!(elems, vec![LocElement::Command("Scope.GetName(")]);
    }

    #[test]
    fn test_jomini_lone_quote_param() {
        // `[GetName(')]` — a single `'` satisfies both starts_with and ends_with,
        // so stripping "the quotes" sliced [1..0] and panicked the process.
        let elems = parse_loc_elements("[GetName(')]");
        assert_eq!(elems.len(), 1, "{elems:?}");
        let LocElement::JominiCommand(cmds) = &elems[0] else {
            panic!("expected JominiCommand, got {elems:?}");
        };
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].key, "GetName");
        assert_eq!(cmds[0].params, vec![JominiParam::Literal("'".to_string())]);
    }

    #[test]
    fn test_jomini_empty_quoted_param() {
        // `''` is a well-formed empty literal: two quotes, not one doing both jobs.
        let elems = parse_loc_elements("[GetName('')]");
        let LocElement::JominiCommand(cmds) = &elems[0] else {
            panic!("expected JominiCommand, got {elems:?}");
        };
        assert_eq!(cmds[0].params, vec![JominiParam::Literal(String::new())]);
    }

    #[test]
    fn test_event_target() {
        let elems = parse_loc_elements("[event_target:foo]");
        assert_eq!(elems.len(), 1);
        assert_eq!(elems[0], LocElement::Command("event_target:foo"));
    }

    #[test]
    fn test_question_variable() {
        let elems = parse_loc_elements("[?var_name]");
        assert_eq!(elems.len(), 1);
        assert_eq!(elems[0], LocElement::Command("?var_name"));
    }

    #[test]
    fn test_scripted_gui_callback() {
        let elems = parse_loc_elements("[!topbar_icon_click]");
        assert_eq!(elems, vec![LocElement::Command("!topbar_icon_click")]);
    }

    #[test]
    fn test_scripted_gui_callback_as_jomini_tail() {
        let elems = parse_loc_elements("[Root.!topbar_icon_click]");
        let LocElement::JominiCommand(commands) = &elems[0] else {
            panic!("expected JominiCommand, got {elems:?}");
        };
        assert_eq!(commands[0].key, "Root");
        assert_eq!(commands[1].key, "!topbar_icon_click");
    }

    #[test]
    fn test_ref_colour_suffix_stripped() {
        // $MY_KEY|Y$ should yield Ref("MY_KEY"), not Ref("MY_KEY|Y")
        let elems = parse_loc_elements("$MY_KEY|Y$");
        assert_eq!(elems, vec![LocElement::Ref("MY_KEY")]);
    }

    #[test]
    fn test_ref_no_colour_suffix() {
        // Plain ref without colour suffix still works
        let elems = parse_loc_elements("$MY_KEY$");
        assert_eq!(elems, vec![LocElement::Ref("MY_KEY")]);
    }

    #[test]
    fn test_stray_closing_bracket_terminates() {
        // Regression: `[cmd]]` has an extra `]`. A lone `]` is special, so the
        // old `_` arm called next_special(s, i) == i and looped forever pushing
        // empty Chars (OOM). It must now terminate and treat `]` as literal text.
        let elems = parse_loc_elements("[USA.GetName]], rest");
        // Last elements include the stray `]` and the trailing text.
        let joined: String = elems
            .iter()
            .map(|e| match e {
                LocElement::Chars(c) => (*c).to_string(),
                _ => String::new(),
            })
            .collect();
        assert!(
            joined.contains(']'),
            "stray bracket kept as text: {elems:?}"
        );
        assert!(joined.contains(", rest"), "trailing text parsed: {elems:?}");
    }

    #[test]
    fn test_only_closing_bracket() {
        // A bare `]` must not loop.
        let elems = parse_loc_elements("]");
        assert_eq!(elems, vec![LocElement::Chars("]")]);
    }

    #[test]
    fn test_multibyte_text_with_stray_bracket() {
        // Cyrillic text (multi-byte) around a stray `]` — must not panic on a
        // non-char-boundary index and must terminate.
        let elems = parse_loc_elements("мнения[USA.GetName]], потому");
        assert!(!elems.is_empty());
    }

    #[test]
    fn test_ref_colour_in_mixed_string() {
        // Colour-suffixed ref inside mixed text
        let elems = parse_loc_elements("Hello $NAME|G$ world");
        assert_eq!(elems[0], LocElement::Chars("Hello "));
        assert_eq!(elems[1], LocElement::Ref("NAME"));
        assert_eq!(elems[2], LocElement::Chars(" world"));
    }

    fn refs(s: &str) -> Vec<String> {
        parse_loc_elements(s)
            .into_iter()
            .filter_map(|e| match e {
                LocElement::Ref(r) => Some(r.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn test_currency_dollar_before_bracket_is_literal() {
        // `$[?var|-3]` — the `$` is a literal currency sign, the `[?..]` a command.
        // Two adjacent constructs (as in MD loc) used to let the second `$` close
        // the first, yielding a bogus Ref("[?...mandatory_funding").
        let s = "$[?united_nations_esco_mandatory_funding|-3]\n$[?united_nations_esco_optional_funding|-3]";
        assert!(
            refs(s).is_empty(),
            "no bogus ref: {:?}",
            parse_loc_elements(s)
        );
        // The bracket still parses as a command.
        assert!(parse_loc_elements(s).iter().any(|e| matches!(e, LocElement::Command(c) if c.starts_with("?united_nations_esco_mandatory"))));
    }

    #[test]
    fn test_colour_code_prefix_not_a_ref() {
        // `$§Y[?GDPVAR|0]§!` — colour-code + bracket after a literal `$`. Must not
        // yield a Ref (the all-caps body would otherwise dodge the lowercase heuristic).
        let s = "$§Y[?GDPVAR|0]§!$x$";
        assert!(
            !refs(s).iter().any(|r| r.contains('[') || r.contains('§')),
            "no bogus ref with bracket/colour chars: {:?}",
            refs(s)
        );
    }

    #[test]
    fn test_stray_currency_dollars_not_refs() {
        assert!(refs("$5 and $10").is_empty(), "{:?}", refs("$5 and $10"));
        assert!(refs("costs 100$ total").is_empty());
    }

    #[test]
    fn test_unmatched_open_still_finds_the_command_after_it() {
        // The `[` at 0 never closes, so it is text; the bracket at 2 does close
        // and must still parse. Skipping a known-unmatched open must not skip
        // the matched one that follows it.
        let elems = parse_loc_elements("[[[GetName] tail");
        assert_eq!(
            elems,
            vec![
                LocElement::Chars("[["),
                LocElement::Command("GetName"),
                LocElement::Chars(" tail"),
            ]
        );
    }

    #[test]
    fn test_literal_run_is_one_element() {
        // Stray brackets and dollars are text. They used to be split into one
        // element per byte, so a value of them cost 32 bytes of `LocElement` per
        // character; they now join the run around them.
        let elems = parse_loc_elements("a]]b[[c$$d");
        assert_eq!(elems, vec![LocElement::Chars("a]]b[[c$$d")]);
    }

    #[test]
    fn test_unmatched_bracket_run_parses_in_linear_time() {
        // One mebibyte of `[`. Every one of them fails to parse, and each
        // failure used to scan to the end of the string first: ~5.5e11 byte
        // reads, minutes of CPU. Linear parsing is two passes over the bytes, a
        // few milliseconds even unoptimised, so a five-second ceiling separates
        // the two by ~100x on either side rather than racing the machine.
        let s = "[".repeat(1 << 20);
        let start = std::time::Instant::now();
        let elems = parse_loc_elements(&s);
        let elapsed = start.elapsed();

        assert_eq!(
            elems,
            vec![LocElement::Chars(s.as_str())],
            "the whole run is one literal"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "parsing {} unmatched brackets took {elapsed:?}",
            s.len()
        );
    }

    #[test]
    fn test_legit_refs_still_parse() {
        assert_eq!(refs("$MY_KEY$"), vec!["MY_KEY".to_string()]);
        assert_eq!(refs("$MY_KEY|Y$"), vec!["MY_KEY".to_string()]);
        assert_eq!(
            refs("$military_industrial_organization_funds_gain$"),
            vec!["military_industrial_organization_funds_gain".to_string()]
        );
    }
}
