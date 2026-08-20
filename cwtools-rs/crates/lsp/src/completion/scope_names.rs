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

/// Build best-effort localisation completions for the syntax at the cursor.
/// Scope tracking within localisation remains intentionally best-effort.
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

/// Chain-keyword prelude for scope completions. HOI4 and EU4 configs conventionally
/// spell these uppercase; other supported games use lowercase.
fn scope_prelude(language: &str) -> &'static [&'static str] {
    match normalized_game(language) {
        Some(Game::Hoi4 | Game::Eu4) => &["THIS", "ROOT", "PREV", "FROM"],
        _ => &["this", "root", "prev", "from"],
    }
}

/// Derive scope completion names from the loaded registry when available, with
/// game scope definitions and a small link fallback when no registry is loaded.
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
    } else {
        names.extend(scope_names_for_game(language));
    }

    let prelude_len = scope_prelude(language).len();
    names[prelude_len..].sort_unstable();
    names.dedup();
    names
}

pub(crate) fn scope_names_for_game(language: &str) -> Vec<String> {
    let mut names: Vec<String> = normalized_game(language)
        .map(|game| {
            game.scope_defs()
                .iter()
                .flat_map(|scope| {
                    std::iter::once(scope.name.to_ascii_lowercase())
                        .chain(scope.aliases.iter().map(|alias| alias.to_ascii_lowercase()))
                })
                .collect()
        })
        .unwrap_or_default();

    let links: &[&str] = match normalized_game(language) {
        Some(Game::Hoi4) => &["OVERLORD", "FACTION_LEADER", "capital_scope", "owner"],
        Some(Game::Stellaris) => &[
            "owner",
            "controller",
            "space_owner",
            "space_controller",
            "solar_system",
        ],
        Some(Game::Eu4) => &["EMPEROR", "capital_scope", "owner", "controller"],
        Some(Game::Ck2 | Game::Ck3) => &["liege", "employer", "host"],
        Some(Game::Ir) => &["owner", "controller", "capital_scope"],
        _ => &[],
    };
    names.extend(links.iter().map(|s| s.to_string()));
    names
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn game_aliases_share_scope_fallback() {
        assert_eq!(
            scope_names_for_game("stellaris"),
            scope_names_for_game("stl")
        );
        assert_eq!(
            scope_names_for_game("ir"),
            scope_names_for_game("imperator")
        );
        assert!(
            scope_completion_names("HOI4", None)
                .iter()
                .any(|s| s == "THIS")
        );
    }
}
