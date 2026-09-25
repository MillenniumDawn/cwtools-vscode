use tower_lsp::lsp_types::*;

use super::sort_for_kind;
use crate::paths::{encoded_position_len, line_prefix_with_encoding};
use cwtools_game::constants::Game;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocCompletionContext {
    Key,
    DataFunction,
    Reference,
}

pub(crate) fn loc_completion_context(
    text: &str,
    pos: Position,
    encoding: &PositionEncodingKind,
) -> LocCompletionContext {
    let prefix = line_prefix_with_encoding(text, pos.line, pos.character, encoding);
    if prefix.matches('$').count() % 2 == 1 {
        LocCompletionContext::Reference
    } else if prefix.rfind('[') > prefix.rfind(']') {
        LocCompletionContext::DataFunction
    } else {
        LocCompletionContext::Key
    }
}

pub(crate) fn loc_completion_range(
    text: &str,
    pos: Position,
    context: LocCompletionContext,
    encoding: &PositionEncodingKind,
) -> Range {
    let prefix = line_prefix_with_encoding(text, pos.line, pos.character, encoding);
    let start_byte = prefix
        .char_indices()
        .rev()
        .find_map(|(byte, ch)| {
            let part_of_token = ch.is_alphanumeric()
                || ch == '_'
                || (context != LocCompletionContext::DataFunction && ch == '.');
            (!part_of_token).then_some(byte + ch.len_utf8())
        })
        .unwrap_or(0);
    Range::new(
        Position::new(
            pos.line,
            encoded_position_len(&prefix[..start_byte], encoding),
        ),
        Position::new(pos.line, encoded_position_len(prefix, encoding)),
    )
}

/// Whether the cursor is in the field-key position of a scripted
/// localisation's outer `defined_text` block.
///
/// Scripted localisation files are script files rather than YAML localisation
/// files. Their `defined_text` wrapper is not a game type key, so the normal
/// position resolver cannot descend into the `scripted_loc` rules there. Keep
/// this narrow: values and nested blocks under `text` must continue through
/// the regular script completion path (which is where variables are useful).
pub(crate) fn in_scripted_loc_key_context(
    text: &str,
    pos: Position,
    encoding: &PositionEncodingKind,
) -> bool {
    let current_prefix = line_prefix_with_encoding(text, pos.line, pos.character, encoding);

    let mut prefix = String::new();
    for (line, content) in text.lines().enumerate() {
        if line >= pos.line as usize {
            break;
        }
        prefix.push_str(content);
        prefix.push('\n');
    }
    prefix.push_str(current_prefix);

    let mut blocks = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut in_comment = false;
    for (byte, ch) in prefix.char_indices() {
        if in_comment {
            if ch == '\n' {
                in_comment = false;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '#' => in_comment = true,
            '"' => in_string = true,
            '{' => blocks.push(assignment_name_before(prefix.as_str(), byte)),
            '}' => {
                blocks.pop();
            }
            _ => {}
        }
    }

    let has_defined_text = blocks
        .iter()
        .flatten()
        .any(|name| name.eq_ignore_ascii_case("defined_text"));
    let inside_text = blocks
        .iter()
        .flatten()
        .any(|name| name.eq_ignore_ascii_case("text"));
    if !has_defined_text || inside_text {
        return false;
    }

    // `name = ...` and `text = ...` are value positions, not field-key
    // positions. An assignment followed by `{` is an opener, so it remains a
    // key position until the block-specific check above handles the `text`
    // block itself.
    let current_line = current_prefix.split('#').next().unwrap_or(current_prefix);
    let last_open = current_line.rfind('{').unwrap_or(0);
    current_line
        .rfind('=')
        .is_none_or(|assignment| assignment <= last_open)
}

fn assignment_name_before(source: &str, open_byte: usize) -> Option<String> {
    let before = source[..open_byte].trim_end();
    let lhs = before.strip_suffix('=')?.trim_end();
    let start = lhs
        .char_indices()
        .rev()
        .find_map(|(byte, ch)| {
            (!ch.is_ascii_alphanumeric() && ch != '_').then_some(byte + ch.len_utf8())
        })
        .unwrap_or(0);
    let name = &lhs[start..];
    (!name.is_empty()).then(|| name.to_string())
}

pub(crate) fn loc_completions(
    loc_keys: &std::collections::HashSet<String>,
    language: &str,
    registry: Option<&cwtools_game::scope_registry::ScopeRegistry>,
    context: LocCompletionContext,
) -> Vec<CompletionItem> {
    match context {
        LocCompletionContext::Key | LocCompletionContext::Reference => loc_keys
            .iter()
            .map(|k| CompletionItem {
                label: k.clone(),
                kind: Some(if context == LocCompletionContext::Reference {
                    CompletionItemKind::REFERENCE
                } else {
                    CompletionItemKind::TEXT
                }),
                detail: Some("loc key".to_string()),
                sort_text: sort_for_kind(Some(CompletionItemKind::TEXT), k),
                ..Default::default()
            })
            .collect(),
        LocCompletionContext::DataFunction => scope_completion_names(language, registry)
            .into_iter()
            .map(|name| {
                let sort_text = sort_for_kind(Some(CompletionItemKind::FUNCTION), &name);
                CompletionItem {
                    label: name,
                    kind: Some(CompletionItemKind::FUNCTION),
                    detail: Some("scope command".to_string()),
                    sort_text,
                    ..Default::default()
                }
            })
            .collect(),
    }
}

fn normalized_game(language: &str) -> Option<Game> {
    Game::from_str(language)
}

fn scope_prelude(language: &str) -> &'static [&'static str] {
    match normalized_game(language) {
        Some(Game::Hoi4 | Game::Eu4) => &["THIS", "ROOT", "PREV", "FROM"],
        _ => &["this", "root", "prev", "from"],
    }
}

/// without one, only the prelude keywords are offered (#373 — scope names
pub(crate) fn scope_completion_names(
    language: &str,
    registry: Option<&cwtools_game::scope_registry::ScopeRegistry>,
) -> Vec<String> {
    let mut names: Vec<String> = scope_prelude(language)
        .iter()
        .map(|s| s.to_string())
        .collect();

    if let Some(reg) = registry {
        names.extend(reg.by_id.values().map(|d| d.name.clone()));
        names.extend(reg.links.keys().cloned());
    }

    let prelude_len = scope_prelude(language).len();
    names[prelude_len..].sort_unstable();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_loc_key_context_stays_outside_text_values() {
        let encoding = &PositionEncodingKind::UTF16;
        let outer = "defined_text = {\n    n\n}\n";
        assert!(in_scripted_loc_key_context(
            outer,
            Position::new(1, 5),
            encoding
        ));

        let value = "defined_text = {\n    name = GetName\n}\n";
        assert!(!in_scripted_loc_key_context(
            value,
            Position::new(1, 17),
            encoding
        ));

        let nested = "defined_text = {\n    text = {\n        $my_var\n    }\n}\n";
        assert!(!in_scripted_loc_key_context(
            nested,
            Position::new(2, 16),
            encoding
        ));
    }

    #[test]
    fn localisation_context_is_cursor_sensitive() {
        let encoding = &PositionEncodingKind::UTF16;
        assert_eq!(
            loc_completion_context("key:0 text", Position::new(0, 10), encoding),
            LocCompletionContext::Key
        );
        assert_eq!(
            loc_completion_context("key:0 [Get", Position::new(0, 10), encoding),
            LocCompletionContext::DataFunction
        );
        assert_eq!(
            loc_completion_context("key:0 $OTHER", Position::new(0, 13), encoding),
            LocCompletionContext::Reference
        );
        assert_eq!(
            loc_completion_context("key:0 [GetName]", Position::new(0, 16), encoding),
            LocCompletionContext::Key
        );
    }

    #[test]
    fn localisation_items_follow_context() {
        let loc_keys = std::collections::HashSet::from(["KNOWN_KEY".to_string()]);
        let keys = loc_completions(&loc_keys, "hoi4", None, LocCompletionContext::Key);
        assert!(keys.iter().any(|item| item.label == "KNOWN_KEY"));
        assert!(!keys.iter().any(|item| item.label == "THIS"));
        let functions =
            loc_completions(&loc_keys, "hoi4", None, LocCompletionContext::DataFunction);
        assert!(functions.iter().any(|item| item.label == "THIS"));
        assert!(!functions.iter().any(|item| item.label == "KNOWN_KEY"));
        let references = loc_completions(&loc_keys, "hoi4", None, LocCompletionContext::Reference);
        assert!(references.iter().any(|item| item.label == "KNOWN_KEY"));
    }

    #[test]
    fn localisation_ranges_exclude_syntax_delimiters() {
        let reference = "key:0 $foo.bar";
        let pos = Position::new(0, reference.chars().count() as u32);
        assert_eq!(
            loc_completion_range(
                reference,
                pos,
                LocCompletionContext::Reference,
                &PositionEncodingKind::UTF16,
            ),
            Range::new(Position::new(0, 7), pos)
        );

        let function = "key:0 [GetName";
        let pos = Position::new(0, function.chars().count() as u32);
        assert_eq!(
            loc_completion_range(
                function,
                pos,
                LocCompletionContext::DataFunction,
                &PositionEncodingKind::UTF16,
            ),
            Range::new(Position::new(0, 7), pos)
        );
    }

    #[test]
    fn localisation_context_and_range_use_utf16_columns() {
        let reference = "😀 key:0 $OTHER";
        let pos = Position::new(0, crate::paths::utf16_len(reference));
        assert_eq!(
            loc_completion_context(reference, pos, &PositionEncodingKind::UTF16),
            LocCompletionContext::Reference
        );
        assert_eq!(
            loc_completion_range(
                reference,
                pos,
                LocCompletionContext::Reference,
                &PositionEncodingKind::UTF16,
            ),
            Range::new(Position::new(0, 10), pos)
        );

        let function = "😀 key:0 [Get";
        let pos = Position::new(0, crate::paths::utf16_len(function));
        assert_eq!(
            loc_completion_context(function, pos, &PositionEncodingKind::UTF16),
            LocCompletionContext::DataFunction
        );
        assert_eq!(
            loc_completion_range(
                function,
                pos,
                LocCompletionContext::DataFunction,
                &PositionEncodingKind::UTF16,
            ),
            Range::new(Position::new(0, 10), pos)
        );
    }

    #[test]
    fn localisation_context_and_range_use_utf32_columns() {
        let reference = "😀 key:0 $OTHER";
        let pos = Position::new(0, reference.chars().count() as u32);
        assert_eq!(
            loc_completion_context(reference, pos, &PositionEncodingKind::UTF32),
            LocCompletionContext::Reference
        );
        assert_eq!(
            loc_completion_range(
                reference,
                pos,
                LocCompletionContext::Reference,
                &PositionEncodingKind::UTF32,
            ),
            Range::new(Position::new(0, 9), pos)
        );
    }

    #[test]
    fn scope_completion_names_without_registry_keeps_prelude() {
        assert!(
            scope_completion_names("HOI4", None)
                .iter()
                .any(|s| s == "THIS")
        );
    }
}
