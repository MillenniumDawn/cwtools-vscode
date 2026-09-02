use tower_lsp::lsp_types::Url;

pub(crate) fn uri_to_path_str(uri: &str) -> String {
    if let Ok(url) = Url::parse(uri)
        && let Ok(path) = url.to_file_path()
        && let Some(s) = path.to_str()
    {
        return s.to_string();
    }
    uri.strip_prefix("file://").unwrap_or(uri).to_string()
}

/// Convert a filesystem path to a `file://` URI, percent-encoding special
pub(crate) fn path_to_uri(path: &std::path::Path) -> String {
    Url::from_file_path(path)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| format!("file://{}", path.display()))
}

/// That is not hypothetical: VS Code spells the Windows drive as a lower-case
/// string compare, so on Windows it never matched: every open file was indexed
/// On Windows that includes a drive-less absolute URI (`file:///a/b.txt`),
/// on Windows that is the drive letter's *case* as well as the percent-encoding:
/// server emits on Windows was already going through `Url::from_file_path`, so
pub(crate) fn canonical_uri(uri: &str) -> String {
    if let Ok(url) = Url::parse(uri)
        && let Some(canonical) = canonical_url(&url)
    {
        return canonical.into();
    }
    uri.to_string()
}

fn canonical_url(url: &Url) -> Option<Url> {
    if url.scheme() != "file" {
        return None;
    }
    let path = url.to_file_path().ok()?;
    let canonical = path_to_uri(&path);
    if canonical == url.as_str() {
        return None;
    }
    Url::parse(&canonical).ok()
}

pub(crate) fn canonicalize_url(url: &mut Url) {
    if let Some(canonical) = canonical_url(url) {
        *url = canonical;
    }
}

pub(crate) fn canonicalize_uri_string(uri: &mut String) {
    let canonical = canonical_uri(uri);
    if *uri != canonical {
        *uri = canonical;
    }
}

pub(crate) fn workspace_prefix_of(workspace_uri: &str) -> std::sync::Arc<str> {
    let ws_path = normalize_separators(uri_to_path_str(workspace_uri));
    std::sync::Arc::from(ws_path.trim_end_matches('/'))
}

pub(crate) fn logical_path_from_uri(
    uri: &str,
    workspace_prefix: &Option<std::sync::Arc<str>>,
) -> String {
    // indexing, path matching). On Windows `uri_to_path_str` yields backslashes,
    let path = normalize_separators(uri_to_path_str(uri));
    if let Some(prefix) = workspace_prefix
        && let Some(rel) = path.strip_prefix(prefix.as_ref())
    {
        return rel.trim_start_matches('/').to_string();
    }
    path
}

/// allocation or scan-and-copy; only Windows paths actually pay the `replace`.
fn normalize_separators(path: String) -> String {
    if path.contains('\\') {
        path.replace('\\', "/")
    } else {
        path
    }
}

/// `u16`; the LSP column is UTF-16 code units (they agree on BMP-only lines).
/// need — see the `position_encoding` note in `initialize_impl`.
pub(crate) fn lsp_pos_to_source(pos: tower_lsp::lsp_types::Position) -> (u32, u16) {
    (pos.line + 1, pos.character.min(u16::MAX as u32) as u16)
}

pub(crate) fn lsp_pos_to_source_in_text(
    text: &str,
    pos: tower_lsp::lsp_types::Position,
    encoding: &tower_lsp::lsp_types::PositionEncodingKind,
) -> (u32, u16) {
    let column = line_prefix_with_encoding(text, pos.line, pos.character, encoding)
        .chars()
        .count();
    (pos.line + 1, column.min(u16::MAX as usize) as u16)
}

/// the wrong document once (the `"<vanilla-cache>"` sentinel, #62), so a stray
pub(crate) fn parse_uri(uri_str: impl AsRef<str>, fallback: &Url) -> Url {
    let uri_str = uri_str.as_ref();
    uri_str.parse().unwrap_or_else(|_| {
        tracing::warn!(uri = %uri_str, "parse_uri: not a valid URI, using fallback location");
        fallback.clone()
    })
}

pub(crate) fn utf16_byte_index(text: &str, column: u32) -> usize {
    let mut utf16 = 0_u32;
    for (byte, ch) in text.char_indices() {
        let next = utf16 + ch.len_utf16() as u32;
        if next > column {
            return byte;
        }
        utf16 = next;
    }
    text.len()
}

fn utf32_byte_index(text: &str, column: u32) -> usize {
    text.char_indices()
        .nth(column as usize)
        .map_or(text.len(), |(byte, _)| byte)
}

pub(crate) fn utf16_len(text: &str) -> u32 {
    text.encode_utf16().count() as u32
}

pub(crate) fn encoded_position_len(
    text: &str,
    encoding: &tower_lsp::lsp_types::PositionEncodingKind,
) -> u32 {
    if encoding == &tower_lsp::lsp_types::PositionEncodingKind::UTF32 {
        text.chars().count() as u32
    } else {
        utf16_len(text)
    }
}

/// A parser column (0-based chars) rendered in the negotiated encoding, for
pub(crate) fn source_column_to_lsp(
    line: &str,
    source_column: u32,
    encoding: &tower_lsp::lsp_types::PositionEncodingKind,
) -> u32 {
    let chars = line.chars().take(source_column as usize);
    if encoding == &tower_lsp::lsp_types::PositionEncodingKind::UTF32 {
        chars.count() as u32
    } else {
        chars.map(|ch| ch.len_utf16() as u32).sum()
    }
}

#[cfg(test)]
pub(crate) fn line_prefix(text: &str, line0: u32, char0: u32) -> &str {
    line_prefix_with_encoding(
        text,
        line0,
        char0,
        &tower_lsp::lsp_types::PositionEncodingKind::UTF16,
    )
}

pub(crate) fn line_prefix_with_encoding<'a>(
    text: &'a str,
    line0: u32,
    char0: u32,
    encoding: &tower_lsp::lsp_types::PositionEncodingKind,
) -> &'a str {
    let line = text.lines().nth(line0 as usize).unwrap_or("");
    let byte = position_byte_index(line, char0, encoding);
    &line[..byte]
}

pub(crate) fn position_byte_index(
    text: &str,
    column: u32,
    encoding: &tower_lsp::lsp_types::PositionEncodingKind,
) -> usize {
    if encoding == &tower_lsp::lsp_types::PositionEncodingKind::UTF32 {
        utf32_byte_index(text, column)
    } else {
        utf16_byte_index(text, column)
    }
}

#[cfg(test)]
pub(crate) fn line_value_key(text: &str, line0: u32, char0: u32) -> Option<String> {
    line_value_key_with_encoding(
        text,
        line0,
        char0,
        &tower_lsp::lsp_types::PositionEncodingKind::UTF16,
    )
}

pub(crate) fn line_value_key_with_encoding(
    text: &str,
    line0: u32,
    char0: u32,
    encoding: &tower_lsp::lsp_types::PositionEncodingKind,
) -> Option<String> {
    text.lines().nth(line0 as usize)?;
    let upto = line_prefix_with_encoding(text, line0, char0, encoding);
    let trimmed = upto.trim_end();
    if !trimmed.ends_with(['=', '<', '>', '!', '?']) {
        return None;
    }
    let op_pos = trimmed.find(['=', '<', '>', '!', '?'])?;
    let key_part = &trimmed[..op_pos];
    let key = key_part
        .rsplit(|c: char| c.is_whitespace() || c == '{')
        .find(|s| !s.is_empty())?;
    if key.is_empty() || key.contains('}') || key.contains('"') {
        return None;
    }
    Some(key.to_string())
}

#[cfg(test)]
pub(crate) fn current_token_range(
    text: &str,
    line0: u32,
    char0: u32,
) -> tower_lsp::lsp_types::Range {
    current_token_range_with_encoding(
        text,
        line0,
        char0,
        &tower_lsp::lsp_types::PositionEncodingKind::UTF16,
    )
}

pub(crate) fn current_token_range_with_encoding(
    text: &str,
    line0: u32,
    char0: u32,
    encoding: &tower_lsp::lsp_types::PositionEncodingKind,
) -> tower_lsp::lsp_types::Range {
    use tower_lsp::lsp_types::{Position, Range};
    let prefix = line_prefix_with_encoding(text, line0, char0, encoding);
    let start_byte = prefix
        .char_indices()
        .rev()
        .find_map(|(byte, c)| (!(c.is_alphanumeric() || c == '_')).then_some(byte + c.len_utf8()))
        .unwrap_or(0);
    Range {
        start: Position {
            line: line0,
            character: encoded_position_len(&prefix[..start_byte], encoding),
        },
        end: Position {
            line: line0,
            character: encoded_position_len(prefix, encoding),
        },
    }
}

#[cfg(test)]
pub(crate) fn current_token_text(text: &str, line0: u32, char0: u32, start_char: u32) -> String {
    current_token_text_with_encoding(
        text,
        line0,
        char0,
        start_char,
        &tower_lsp::lsp_types::PositionEncodingKind::UTF16,
    )
}

pub(crate) fn current_token_text_with_encoding(
    text: &str,
    line0: u32,
    char0: u32,
    start_char: u32,
    encoding: &tower_lsp::lsp_types::PositionEncodingKind,
) -> String {
    let line = text.lines().nth(line0 as usize).unwrap_or("");
    let index = |column| position_byte_index(line, column, encoding);
    let start = index(start_char);
    let end = index(char0);
    line.get(start..end).unwrap_or("").to_string()
}

pub(crate) fn is_loc_file(uri: &str) -> bool {
    if !has_loc_ext(uri) {
        return false;
    }
    let path = normalize_separators(uri_to_path_str(uri)).to_ascii_lowercase();
    LOC_DIR_NAMES
        .iter()
        .any(|dir| cwtools_info::path_contains_segment(&path, dir))
}

const LOC_DIR_NAMES: [&str; 3] = ["localisation", "localisation_synced", "localization"];

pub(crate) fn has_loc_ext(uri: &str) -> bool {
    std::path::Path::new(uri)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(cwtools_file_manager::is_loc_ext)
}

pub(crate) fn is_cwt_file(uri: &str) -> bool {
    uri.to_ascii_lowercase().ends_with(".cwt")
}

pub(crate) fn is_script_file(uri: &str) -> bool {
    std::path::Path::new(uri)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            cwtools_file_manager::SCRIPT_EXTENSIONS
                .iter()
                .any(|script_ext| ext.eq_ignore_ascii_case(script_ext))
        })
}

/// line. `col` uses the negotiated LSP position encoding. Returns the
#[cfg(test)]
pub(crate) fn loc_ref_at_cursor(line: &str, col: u32) -> Option<(String, u32, u32)> {
    loc_ref_at_cursor_with_encoding(
        line,
        col,
        &tower_lsp::lsp_types::PositionEncodingKind::UTF16,
    )
}

pub(crate) fn loc_ref_at_cursor_with_encoding(
    line: &str,
    col: u32,
    encoding: &tower_lsp::lsp_types::PositionEncodingKind,
) -> Option<(String, u32, u32)> {
    let mut dollars: Vec<(u32, usize)> = Vec::new();
    let mut encoded_col: u32 = 0;
    for (b, ch) in line.char_indices() {
        if ch == '$' {
            dollars.push((encoded_col, b));
        }
        encoded_col += if encoding == &tower_lsp::lsp_types::PositionEncodingKind::UTF32 {
            1
        } else {
            ch.len_utf16() as u32
        };
    }
    let mut i = 0;
    while i + 1 < dollars.len() {
        let (open_col, open_b) = dollars[i];
        let (close_col, close_b) = dollars[i + 1];
        let inner = &line[open_b + 1..close_b];
        let key = inner.split('|').next().unwrap_or(inner);
        if is_loc_ident(key) {
            let end_col = close_col + 1;
            if col >= open_col && col <= end_col {
                return Some((key.to_string(), open_col, end_col));
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    None
}

fn is_loc_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

pub(crate) fn default_cache_dir() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    if let Ok(x) = std::env::var("XDG_CACHE_HOME")
        && !x.is_empty()
    {
        return Some(PathBuf::from(x).join("cwtools"));
    }
    if let Ok(la) = std::env::var("LOCALAPPDATA")
        && !la.is_empty()
    {
        return Some(PathBuf::from(la).join("cwtools"));
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        let home = PathBuf::from(home);
        #[cfg(target_os = "macos")]
        {
            return Some(home.join("Library/Caches/cwtools"));
        }
        #[cfg(not(target_os = "macos"))]
        {
            return Some(home.join(".cache/cwtools"));
        }
    }
    Some(std::env::temp_dir().join("cwtools"))
}

pub(crate) fn discover_vanilla_dir(game: &str) -> Option<std::path::PathBuf> {
    let folder = match game {
        "hoi4" => "Hearts of Iron IV",
        "stellaris" => "Stellaris",
        "eu4" => "Europa Universalis IV",
        "ck2" => "Crusader Kings II",
        "ck3" => "Crusader Kings III",
        "vic2" => "Victoria 2",
        "vic3" => "Victoria 3",
        "ir" => "ImperatorRome",
        "eu5" => "Europa Universalis V",
        _ => return None,
    };

    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    // Steam library roots to probe (Linux, macOS, Windows).
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    if let Some(h) = &home {
        roots.push(h.join(".steam/steam/steamapps/common"));
        roots.push(h.join(".local/share/Steam/steamapps/common"));
        roots.push(h.join("Library/Application Support/Steam/steamapps/common"));
    }
    roots.push(std::path::PathBuf::from(
        "C:/Program Files (x86)/Steam/steamapps/common",
    ));
    roots.push(std::path::PathBuf::from(
        "C:/Program Files/Steam/steamapps/common",
    ));

    roots
        .into_iter()
        .map(|r| r.join(folder))
        .find(|p| p.is_dir())
}

pub(crate) fn strip_loc_comment(s: &str) -> &str {
    if let Some(last_quote) = s.rfind('"') {
        let after = &s[last_quote + 1..];
        if let Some(hash) = after.find('#') {
            &s[..last_quote + 1 + hash]
        } else {
            s
        }
    } else {
        if let Some(hash) = s.find('#') {
            &s[..hash]
        } else {
            s
        }
    }
}

/// the quotes is kept as data (issue #50) while a trailing `# comment` after the
pub(crate) fn loc_display_text(desc: &str) -> &str {
    if let Some(rest) = desc.strip_prefix('"') {
        if let Some(end) = rest.rfind('"') {
            return &rest[..end];
        }
        return strip_loc_comment(rest).trim_end();
    }
    strip_loc_comment(desc).trim_end()
}

pub(crate) fn lang_display_name(lang: cwtools_localization::Lang) -> &'static str {
    match lang {
        cwtools_localization::Lang::English => "English",
        cwtools_localization::Lang::French => "French",
        cwtools_localization::Lang::German => "German",
        cwtools_localization::Lang::Spanish => "Spanish",
        cwtools_localization::Lang::Russian => "Russian",
        cwtools_localization::Lang::Polish => "Polish",
        cwtools_localization::Lang::BrazPor => "Brazilian Portuguese",
        cwtools_localization::Lang::SimpChinese => "Chinese",
        cwtools_localization::Lang::Japanese => "Japanese",
        cwtools_localization::Lang::Korean => "Korean",
        cwtools_localization::Lang::Turkish => "Turkish",
        cwtools_localization::Lang::Default => "Default",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_cache_dir_agrees_with_the_driver_copy() {
        let ours = default_cache_dir().expect("the LSP copy always answers");
        assert_eq!(
            Some(&ours),
            cwtools_driver::default_cache_dir().as_ref(),
            "the LSP and CLI cache directories have drifted apart"
        );
        assert_eq!(
            ours.file_name().and_then(|n| n.to_str()),
            Some("cwtools"),
            "got: {}",
            ours.display()
        );
    }

    #[test]
    fn is_loc_file_matches_all_loc_extensions() {
        // hover/goto, completion, and validate must agree (#2/#217).
        assert!(is_loc_file("file:///mod/localisation/foo_l_english.yml"));
        assert!(is_loc_file("file:///mod/localisation/foo_l_english.yaml"));
        assert!(is_loc_file("file:///mod/localisation/names.csv"));
        // case-insensitive (Windows), extension and directory alike
        assert!(is_loc_file("file:///MOD/LOCALISATION/FOO.YML"));
        assert!(is_loc_file("file:///MOD/Localisation/FOO.YAML"));
        assert!(!is_loc_file("file:///mod/common/ideas/foo.txt"));
        assert!(!is_loc_file("file:///mod/gfx/foo.gfx"));
    }

    #[test]
    fn is_loc_file_requires_a_localisation_directory() {
        assert!(!is_loc_file("file:///repo/.github/workflows/ci.yml"));
        assert!(!is_loc_file("file:///repo/docker-compose.yaml"));
        assert!(!is_loc_file("file:///repo/data/export.csv"));
        assert!(is_loc_file("file:///mod/localization/foo_l_english.yml"));
        assert!(is_loc_file(
            "file:///mod/localisation/replace/foo_l_english.yml"
        ));
        assert!(is_loc_file(
            "file:///home/user/My%20Mod/localisation/foo_l_english.yml"
        ));
        assert!(!is_loc_file("file:///mod/localisation.yml"));
    }

    #[test]
    fn is_loc_file_accepts_the_synced_localisation_directory() {
        assert!(is_loc_file(
            "file:///mod/localisation_synced/foo_l_english.yml"
        ));
        assert!(is_loc_file(
            "file:///mod/localisation_synced/replace/foo_l_english.yml"
        ));
        assert!(is_loc_file(
            "file:///MOD/Localisation_Synced/FOO_l_english.YML"
        ));
    }

    #[test]
    fn is_cwt_file_matches_only_cwt() {
        assert!(is_cwt_file("file:///rules/Config/events.cwt"));
        assert!(is_cwt_file("file:///RULES/FOO.CWT")); // case-insensitive (Windows)
        assert!(!is_cwt_file("file:///mod/common/ideas/foo.txt"));
        assert!(!is_cwt_file("file:///mod/localisation/foo_l_english.yml"));
    }

    #[test]
    fn is_script_file_uses_shared_extension_inventory() {
        for ext in cwtools_file_manager::SCRIPT_EXTENSIONS {
            assert!(is_script_file(&format!("file:///mod/test.{ext}")), "{ext}");
            assert!(
                is_script_file(&format!("file:///mod/test.{}", ext.to_uppercase())),
                "{ext}"
            );
        }
        for uri in [
            "file:///mod/icon.dds",
            "file:///mod/localisation/test.yml",
            "file:///rules/test.cwt",
            "file:///mod/readme.md",
        ] {
            assert!(!is_script_file(uri), "{uri}");
        }
    }

    #[test]
    fn lsp_pos_to_source_clamps_oversized_character() {
        assert_eq!(
            lsp_pos_to_source(tower_lsp::lsp_types::Position::new(2, u16::MAX as u32 + 1,)),
            (3, u16::MAX)
        );
    }

    #[test]
    fn test_line_value_key() {
        let text = "decision = {\n    has_completed_focus = \n}\n";
        assert_eq!(
            line_value_key(text, 1, 26).as_deref(),
            Some("has_completed_focus")
        );
        assert_eq!(line_value_key(text, 1, 10), None);
        let text2 = "block = {\n    num > \n}\n";
        assert_eq!(line_value_key(text2, 1, 10).as_deref(), Some("num"));
    }

    #[test]
    fn test_line_value_key_handles_comparison_operators_in_value() {
        assert_eq!(
            line_value_key("has_idea = ==", 0, 12).as_deref(),
            Some("has_idea"),
            "trailing `==` must still resolve to the key"
        );
        assert_eq!(
            line_value_key("has_idea = =", 0, 11).as_deref(),
            Some("has_idea"),
            "trailing `= =` (space, single =) must still resolve to the key"
        );
        assert_eq!(
            line_value_key("num >= ", 0, 6).as_deref(),
            Some("num"),
            "comparison `>=` must still resolve to the key"
        );
        assert_eq!(
            line_value_key("flag != ", 0, 8).as_deref(),
            Some("flag"),
            "comparison `!=` must still resolve to the key"
        );
        assert_eq!(
            line_value_key("my_block = {", 0, 12),
            None,
            "`my_block = {{` is an insert position, not a value position"
        );
    }

    #[test]
    fn test_current_token_range() {
        let line = "\tset_variable = { gdpc_conv }";
        let cur = "\tset_variable = { gdpc_conv".chars().count() as u32;
        let r = current_token_range(line, 0, cur);
        assert_eq!(
            r.start.character,
            "\tset_variable = { ".chars().count() as u32
        );
        assert_eq!(r.end.character, cur);

        let line2 = "\tvar = ";
        let cur2 = line2.chars().count() as u32;
        let r2 = current_token_range(line2, 0, cur2);
        assert_eq!(r2.start.character, cur2, "no token → empty range at cursor");
        assert_eq!(r2.end.character, cur2);

        let line3 = "value = event_target:foo";
        let cur3 = line3.chars().count() as u32;
        let r3 = current_token_range(line3, 0, cur3);
        assert_eq!(
            r3.start.character,
            "value = event_target:".chars().count() as u32,
            "`:` is a boundary; token is `foo`"
        );
    }

    #[test]
    fn current_token_helpers_use_utf16_columns() {
        let line = "😀 value";
        let cursor = utf16_len(line);
        let range = current_token_range_with_encoding(
            line,
            0,
            cursor,
            &tower_lsp::lsp_types::PositionEncodingKind::UTF16,
        );
        assert_eq!(range.start.character, 3);
        assert_eq!(range.end.character, 8);
        assert_eq!(current_token_text(line, 0, cursor, 3), "value");
        assert_eq!(line_prefix(line, 0, 2), "😀");
        assert_eq!(
            lsp_pos_to_source_in_text(
                line,
                tower_lsp::lsp_types::Position::new(0, cursor),
                &tower_lsp::lsp_types::PositionEncodingKind::UTF16,
            ),
            (1, 7)
        );
    }

    #[test]
    fn current_token_helpers_use_utf32_columns() {
        let line = "😀 value";
        let encoding = tower_lsp::lsp_types::PositionEncodingKind::UTF32;
        let cursor = line.chars().count() as u32;
        let range = current_token_range_with_encoding(line, 0, cursor, &encoding);
        assert_eq!(range.start.character, 2);
        assert_eq!(range.end.character, 7);
        assert_eq!(
            current_token_text_with_encoding(line, 0, cursor, 2, &encoding),
            "value"
        );
        assert_eq!(line_prefix_with_encoding(line, 0, 1, &encoding), "😀");
        assert_eq!(
            lsp_pos_to_source_in_text(
                line,
                tower_lsp::lsp_types::Position::new(0, cursor),
                &encoding,
            ),
            (1, 7)
        );
    }

    #[test]
    fn test_current_token_text() {
        let line = "\tset_variable = { gdpc_conv }";
        let start = "\tset_variable = { ".chars().count() as u32;
        let mid = "\tset_variable = { gdpc".chars().count() as u32;
        assert_eq!(current_token_text(line, 0, mid, start), "gdpc");

        let line2 = "\tvar = ";
        let cur2 = line2.chars().count() as u32;
        assert_eq!(current_token_text(line2, 0, cur2, cur2), "");
    }

    #[test]
    fn test_loc_ref_at_cursor() {
        let line = "  k:0 \"a $FOO$ b\"";
        let (key, start, end) = loc_ref_at_cursor(line, 11).expect("cursor in $FOO$");
        assert_eq!(key, "FOO");
        assert_eq!((start, end), (9, 14));
        assert!(loc_ref_at_cursor(line, 2).is_none());
    }

    #[test]
    fn test_loc_ref_at_cursor_uses_negotiated_encoding() {
        let line = "  😀 $FOO$";
        let utf16 = tower_lsp::lsp_types::PositionEncodingKind::UTF16;
        let utf32 = tower_lsp::lsp_types::PositionEncodingKind::UTF32;
        assert_eq!(
            loc_ref_at_cursor_with_encoding(line, 7, &utf16),
            Some(("FOO".to_string(), 5, 10))
        );
        assert_eq!(
            loc_ref_at_cursor_with_encoding(line, 6, &utf32),
            Some(("FOO".to_string(), 4, 9))
        );
    }

    #[test]
    fn test_loc_ref_at_cursor_colour_suffix() {
        let line = "x:0 \"$MY_KEY|Y$\"";
        let (key, _, _) = loc_ref_at_cursor(line, 8).expect("cursor in ref");
        assert_eq!(key, "MY_KEY", "colour suffix must be stripped from the key");
    }

    #[test]
    fn test_loc_ref_at_cursor_currency_not_a_ref() {
        let line = "x:0 \"costs $5 for $ITEM$\"";
        assert!(
            loc_ref_at_cursor(line, 11).is_none(),
            "currency $5 must not be a ref"
        );
        let (key, _, _) = loc_ref_at_cursor(line, 20).expect("cursor in $ITEM$");
        assert_eq!(key, "ITEM");
    }

    #[test]
    fn test_logical_path_from_uri_strips_workspace() {
        let ws = Some(workspace_prefix_of("file:///home/user/mod"));
        let lp = logical_path_from_uri("file:///home/user/mod/events/foo.txt", &ws);
        assert_eq!(lp, "events/foo.txt");
    }

    #[test]
    fn test_logical_path_fallback() {
        let lp = logical_path_from_uri("file:///some/path/events/foo.txt", &None);
        assert_eq!(lp, "/some/path/events/foo.txt");
    }

    #[test]
    fn test_uri_to_path_percent_decode() {
        // Paths with spaces must round-trip through percent-encoding. The path
        // and the non-encoding fallback kicks in.
        #[cfg(not(windows))]
        let path = std::path::Path::new("/home/user/My Mod/events/foo.txt");
        #[cfg(windows)]
        let path = std::path::Path::new(r"C:\Users\user\My Mod\events\foo.txt");
        let uri = path_to_uri(path);
        assert!(
            uri.contains("%20") || uri.contains("+"),
            "expected encoded space in URI, got: {}",
            uri
        );
        let decoded = uri_to_path_str(&uri);
        assert_eq!(
            decoded,
            path.to_str().unwrap(),
            "round-trip failed: {}",
            decoded
        );
    }

    #[test]
    fn test_logical_path_from_uri_percent_decode() {
        let ws = Some(workspace_prefix_of("file:///home/user/My%20Mod"));
        let lp = logical_path_from_uri("file:///home/user/My%20Mod/events/foo.txt", &ws);
        assert_eq!(lp, "events/foo.txt", "got: {}", lp);
    }

    // ── canonical_uri (#319) ──────────────────────────────────────────────

    /// Windows the round trip also upper-cases the drive letter) and only the
    #[cfg(windows)]
    const CLIENT_URI: &str = "file:///d%3A/a/b.txt";
    #[cfg(not(windows))]
    const CLIENT_URI: &str = "file:///a/%62.txt";

    fn client_uri_canonical() -> String {
        #[cfg(windows)]
        let path = std::path::Path::new(r"d:\a\b.txt");
        #[cfg(not(windows))]
        let path = std::path::Path::new("/a/b.txt");
        path_to_uri(path)
    }

    #[test]
    fn canonical_uri_folds_a_client_spelling_onto_path_to_uri() {
        assert_eq!(
            canonical_uri(CLIENT_URI),
            client_uri_canonical(),
            "the client's spelling must fold onto the path_to_uri spelling"
        );
    }

    #[test]
    fn canonical_uri_is_idempotent_and_matches_path_to_uri() {
        #[cfg(not(windows))]
        let path = std::path::Path::new("/home/user/My Mod/events/foo.txt");
        #[cfg(windows)]
        let path = std::path::Path::new(r"d:\Users\user\My Mod\events\foo.txt");
        let canonical = path_to_uri(path);
        assert_eq!(canonical_uri(&canonical), canonical);
        assert!(canonical.contains("%20"), "got: {canonical}");
    }

    #[test]
    fn canonical_uri_passes_through_what_it_cannot_convert() {
        assert_eq!(canonical_uri("not a uri"), "not a uri");
        assert_eq!(
            canonical_uri("untitled:Untitled-1"),
            "untitled:Untitled-1",
            "untitled buffers have no path and must survive unchanged"
        );
    }

    /// Percent-encoding of an ordinary path byte folds on every platform, which
    /// needs a drive on Windows, where a drive-less one names no path at all.
    #[test]
    fn canonical_uri_decodes_an_encoded_path_letter() {
        #[cfg(windows)]
        let (encoded, decoded) = (
            "file:///d:/a/%64up.txt",
            path_to_uri(std::path::Path::new(r"d:\a\dup.txt")),
        );
        #[cfg(not(windows))]
        let (encoded, decoded) = (
            "file:///a/%64up.txt",
            path_to_uri(std::path::Path::new("/a/dup.txt")),
        );
        assert_eq!(canonical_uri(encoded), decoded);
    }

    /// Windows drive is one key. Which case survives is the round trip's call
    #[cfg(windows)]
    #[test]
    fn canonical_uri_folds_every_drive_letter_spelling_onto_one_key() {
        let canonical = canonical_uri("file:///d%3A/a/b.txt");
        for spelling in [
            "file:///D%3A/a/b.txt",
            "file:///d:/a/b.txt",
            "file:///D:/a/b.txt",
        ] {
            assert_eq!(
                canonical_uri(spelling),
                canonical,
                "{spelling} must share one index key with file:///d%3A/a/b.txt"
            );
        }
        assert_eq!(canonical_uri(&canonical), canonical);
    }

    #[test]
    fn canonicalize_url_rewrites_in_place() {
        let expected = client_uri_canonical();
        let mut url: Url = CLIENT_URI.parse().unwrap();
        canonicalize_url(&mut url);
        assert_eq!(url.as_str(), expected);
        canonicalize_url(&mut url);
        assert_eq!(url.as_str(), expected);
    }

    #[test]
    fn canonicalize_uri_string_rewrites_in_place() {
        let mut uri = CLIENT_URI.to_string();
        canonicalize_uri_string(&mut uri);
        assert_eq!(uri, client_uri_canonical());
    }

    /// with a plain compare. `workspace_prefix_of` percent-decodes, so encoding
    #[test]
    fn workspace_prefix_of_a_canonical_folder_uri_strips_a_canonical_document() {
        #[cfg(windows)]
        let (client_folder, doc) = (
            "file:///d%3A/mod",
            path_to_uri(std::path::Path::new(r"d:\mod\events\foo.txt")),
        );
        #[cfg(not(windows))]
        let (client_folder, doc) = (
            "file:///home/user/%6Dod",
            path_to_uri(std::path::Path::new("/home/user/mod/events/foo.txt")),
        );
        let prefix = Some(workspace_prefix_of(&canonical_uri(client_folder)));
        assert_eq!(logical_path_from_uri(&doc, &prefix), "events/foo.txt");
    }

    /// Why the fold matters here rather than being merely tidy: on Windows the
    #[cfg(windows)]
    #[test]
    fn a_canonical_folder_prefix_strips_a_document_whatever_case_the_client_used() {
        for folder in ["file:///d%3A/mod", "file:///D%3A/mod", "file:///D:/mod"] {
            let prefix = Some(workspace_prefix_of(&canonical_uri(folder)));
            for doc in [
                "file:///d%3A/mod/events/foo.txt",
                "file:///D:/mod/events/foo.txt",
            ] {
                assert_eq!(
                    logical_path_from_uri(&canonical_uri(doc), &prefix),
                    "events/foo.txt",
                    "folder {folder} vs document {doc}"
                );
            }
        }
    }

    // ── strip_loc_comment (#50) ───────────────────────────────────────────

    #[test]
    fn strip_loc_comment_removes_inline_comment_after_quoted_value() {
        assert_eq!(strip_loc_comment(r#""value" # comment"#), r#""value" "#);
    }

    #[test]
    fn strip_loc_comment_preserves_quoted_value_without_comment() {
        assert_eq!(strip_loc_comment(r#""value""#), r#""value""#);
    }

    #[test]
    fn strip_loc_comment_keeps_hash_inside_quotes_as_data() {
        assert_eq!(
            strip_loc_comment(r#""value # not a comment""#),
            r#""value # not a comment""#
        );
    }

    #[test]
    fn strip_loc_comment_strips_first_hash_when_no_quotes() {
        assert_eq!(strip_loc_comment("value # comment"), "value ");
    }

    #[test]
    fn strip_loc_comment_preserves_unquoted_value_without_hash() {
        assert_eq!(strip_loc_comment("value"), "value");
    }

    #[test]
    fn strip_loc_comment_handles_empty_quoted_value_with_comment() {
        assert_eq!(strip_loc_comment(r#""" # comment"#), r#""" "#);
    }

    #[test]
    fn strip_loc_comment_strips_only_first_hash_after_closing_quote() {
        assert_eq!(
            strip_loc_comment(r#""value" # comment # more"#),
            r#""value" "#
        );
    }

    #[test]
    fn strip_loc_comment_handles_empty_string() {
        assert_eq!(strip_loc_comment(""), "");
    }

    #[test]
    fn strip_loc_comment_handles_only_comment() {
        assert_eq!(strip_loc_comment("# just a comment"), "");
    }

    // ── loc_display_text (#50) ────────────────────────────────────────────

    #[test]
    fn loc_display_text_quoted_value_strips_outer_quotes() {
        assert_eq!(loc_display_text(r#""value""#), "value");
    }

    #[test]
    fn loc_display_text_drops_trailing_comment() {
        assert_eq!(loc_display_text(r#""value" # comment"#), "value");
    }

    #[test]
    fn loc_display_text_keeps_hash_inside_quotes() {
        assert_eq!(loc_display_text(r#""value # data""#), "value # data");
    }

    #[test]
    fn loc_display_text_keeps_inner_hash_and_drops_trailing_comment() {
        assert_eq!(
            loc_display_text(r#""value # data" # comment"#),
            "value # data"
        );
    }

    #[test]
    fn loc_display_text_unquoted_value_drops_comment() {
        assert_eq!(loc_display_text("value # comment"), "value");
    }

    #[test]
    fn loc_display_text_unquoted_value_without_comment() {
        assert_eq!(loc_display_text("value"), "value");
    }

    #[test]
    fn loc_display_text_empty_quoted_value() {
        assert_eq!(loc_display_text(r#""""#), "");
    }

    #[test]
    fn loc_display_text_empty_quoted_value_with_comment() {
        assert_eq!(loc_display_text(r#""" # comment"#), "");
    }
}
