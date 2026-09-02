#[derive(Debug, Clone, PartialEq)]
pub enum LocElement<'a> {
    Chars(&'a str),
    Ref(&'a str),
    Command(&'a str),
    JominiCommand(Vec<JominiCommand>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct JominiCommand {
    pub key: String,
    pub params: Vec<JominiParam>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum JominiParam {
    Literal(String),
    Commands(Vec<JominiCommand>),
}

pub fn parse_loc_elements(s: &str) -> Vec<LocElement<'_>> {
    let mut elements = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0; // byte offset; always lands on a char boundary
    let mut literal_start: Option<usize> = None;
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
                i = next_special(s, i + 1);
            }
        }
    }

    if let Some(start) = literal_start {
        elements.push(LocElement::Chars(&s[start..]));
    }

    elements
}

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

fn next_special(s: &str, start: usize) -> usize {
    s.as_bytes()[start..]
        .iter()
        .position(|&b| matches!(b, b'$' | b'[' | b']'))
        .map(|off| start + off)
        .unwrap_or(s.len())
}

fn parse_ref(s: &str, start: usize) -> Option<(LocElement<'_>, usize)> {
    let bytes = s.as_bytes();
    let content_start = start + 1; // skip opening '$'

    let key_end = bytes[content_start..]
        .iter()
        .position(|&b| b == b'$' || b == b'|')
        .map(|off| content_start + off)?;

    let key = &s[content_start..key_end];

    if !is_loc_ref_key(key) {
        return None;
    }

    if bytes[key_end] == b'|' {
        let after_pipe = key_end + 1;
        let close = bytes[after_pipe..]
            .iter()
            .position(|&b| b == b'$')
            .map(|off| after_pipe + off)?;
        Some((LocElement::Ref(key), close + 1))
    } else {
        Some((LocElement::Ref(key), key_end + 1))
    }
}

fn is_loc_ref_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
}

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
        let elems = parse_loc_elements("[Scope.GetName(]");
        assert_eq!(elems, vec![LocElement::Command("Scope.GetName(")]);
    }

    #[test]
    fn test_jomini_lone_quote_param() {
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
        let elems = parse_loc_elements("$MY_KEY|Y$");
        assert_eq!(elems, vec![LocElement::Ref("MY_KEY")]);
    }

    #[test]
    fn test_ref_no_colour_suffix() {
        let elems = parse_loc_elements("$MY_KEY$");
        assert_eq!(elems, vec![LocElement::Ref("MY_KEY")]);
    }

    #[test]
    fn test_stray_closing_bracket_terminates() {
        let elems = parse_loc_elements("[USA.GetName]], rest");
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
        let elems = parse_loc_elements("]");
        assert_eq!(elems, vec![LocElement::Chars("]")]);
    }

    #[test]
    fn test_multibyte_text_with_stray_bracket() {
        let elems = parse_loc_elements("мнения[USA.GetName]], потому");
        assert!(!elems.is_empty());
    }

    #[test]
    fn test_ref_colour_in_mixed_string() {
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
        let s = "$[?united_nations_esco_mandatory_funding|-3]\n$[?united_nations_esco_optional_funding|-3]";
        assert!(
            refs(s).is_empty(),
            "no bogus ref: {:?}",
            parse_loc_elements(s)
        );
        assert!(parse_loc_elements(s).iter().any(|e| matches!(e, LocElement::Command(c) if c.starts_with("?united_nations_esco_mandatory"))));
    }

    #[test]
    fn test_colour_code_prefix_not_a_ref() {
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
        let elems = parse_loc_elements("a]]b[[c$$d");
        assert_eq!(elems, vec![LocElement::Chars("a]]b[[c$$d")]);
    }

    #[test]
    fn test_unmatched_bracket_run_parses_in_linear_time() {
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
