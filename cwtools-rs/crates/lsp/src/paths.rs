use tower_lsp::lsp_types::Url;

/// Convert a `file://` URI string to a filesystem path string, applying
/// percent-decoding (e.g. `%20` → space). Returns the raw URI on failure.
pub(crate) fn uri_to_path_str(uri: &str) -> String {
    if let Ok(url) = Url::parse(uri)
        && let Ok(path) = url.to_file_path()
        && let Some(s) = path.to_str()
    {
        return s.to_string();
    }
    // Fallback: strip scheme manually (no percent-decode, but avoids a panic).
    uri.strip_prefix("file://").unwrap_or(uri).to_string()
}

/// Convert a filesystem path to a `file://` URI, percent-encoding special
/// characters. Uses the `url` crate so paths with spaces or non-ASCII round-trip
/// correctly through `uri_to_path_str`.
///
/// This is the canonical spelling every index keys on; see
/// [`canonical_uri`] for why that matters.
pub(crate) fn path_to_uri(path: &std::path::Path) -> String {
    Url::from_file_path(path)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| format!("file://{}", path.display()))
}

/// Fold a client-supplied URI to the spelling [`path_to_uri`] produces.
///
/// Every index the server keeps — `documents`, `InfoService::files`,
/// `TypeIndex::file_positions`, the loc overlays, the semantic-token cache —
/// is a map keyed on the raw URI string, and `InfoService::clear_file` drops a
/// file's previous contribution by that same string. So one file reaching the
/// server under two spellings is indexed twice and never de-duplicated.
///
/// That is not hypothetical: VS Code spells the Windows drive as a lower-case
/// letter with a percent-encoded colon (`file:///d%3A/mod/x.txt`) while the
/// workspace scan builds its keys with `Url::from_file_path`
/// (`file:///D:/mod/x.txt`), and the `url` crate decodes neither into the
/// other. The scan's "skip documents the editor already has open" guard is a
/// string compare, so on Windows it never matched: every open file was indexed
/// a second time from disk, and CW261 reported every instance of a `## unique`
/// type in it as defined twice.
///
/// Returns the input unchanged for anything that is not a convertible `file:`
/// URI (untitled buffers, virtual documents), so those keep working as before.
/// On Windows that includes a drive-less absolute URI (`file:///a/b.txt`),
/// which names no path there and so has no canonical form to fold onto.
///
/// What gets folded is whatever the URI → path → URI round trip normalises, and
/// on Windows that is the drive letter's *case* as well as the percent-encoding:
/// `std::path` reports `Prefix::Disk` upper-cased and `Url::from_file_path`
/// writes what it reports, so all four of `d%3A`, `D%3A`, `d:` and `D:` land on
/// the single `D:` key. That is the point rather than a side effect — the
/// client's spelling and the scan's have to become the same string. Any URI the
/// server emits on Windows was already going through `Url::from_file_path`, so
/// the case it hands back to the client does not change; VS Code re-normalises
/// a `file:` URI to its own spelling on parse either way.
///
/// The workspace folder URI is canonicalised at the same boundary (see
/// `Backend::initialize_impl`), because `Config::workspace_prefix` is derived
/// from it and [`logical_path_from_uri`] strips that prefix with a plain
/// case-sensitive compare against these canonical document URIs.
pub(crate) fn canonical_uri(uri: &str) -> String {
    if let Ok(url) = Url::parse(uri)
        && let Some(canonical) = canonical_url(&url)
    {
        return canonical.into();
    }
    uri.to_string()
}

/// The canonical form of `url`, or `None` when it is already canonical or is
/// not a `file:` URI that converts to a path. See [`canonical_uri`].
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

/// In-place [`canonical_uri`] for a URI arriving inside request params, applied
/// at the `LanguageServer` boundary so no handler ever keys on a client's own
/// spelling. A no-op when the URI is already canonical.
pub(crate) fn canonicalize_url(url: &mut Url) {
    if let Some(canonical) = canonical_url(url) {
        *url = canonical;
    }
}

/// [`canonicalize_url`] for the workspace-edit params, whose URIs are plain
/// `String`s rather than `Url`s.
pub(crate) fn canonicalize_uri_string(uri: &mut String) {
    let canonical = canonical_uri(uri);
    if *uri != canonical {
        *uri = canonical;
    }
}

/// Normalized, decoded workspace path prefix for [`logical_path_from_uri`],
/// computed ONCE when the workspace URI is set (`Config::workspace_prefix`)
/// instead of re-parsing the constant workspace URI on every request.
pub(crate) fn workspace_prefix_of(workspace_uri: &str) -> std::sync::Arc<str> {
    let ws_path = normalize_separators(uri_to_path_str(workspace_uri));
    std::sync::Arc::from(ws_path.trim_end_matches('/'))
}

/// Derive the logical path (relative to mod root) from a file:// URI and the
/// precomputed workspace prefix ([`workspace_prefix_of`]). Falls back to the
/// raw path if the workspace prefix cannot be stripped.
pub(crate) fn logical_path_from_uri(
    uri: &str,
    workspace_prefix: &Option<std::sync::Arc<str>>,
) -> String {
    // Logical paths are `/`-separated everywhere downstream (type-instance
    // indexing, path matching). On Windows `uri_to_path_str` yields backslashes,
    // so normalise before stripping the workspace prefix — else the leading
    // separator survives `trim_start_matches('/')` and the path leaks into name
    // extraction (e.g. `load_oob` false positives).
    let path = normalize_separators(uri_to_path_str(uri));
    if let Some(prefix) = workspace_prefix
        && let Some(rel) = path.strip_prefix(prefix.as_ref())
    {
        return rel.trim_start_matches('/').to_string();
    }
    // Fallback: use the decoded path as-is
    path
}

/// Normalise path separators to `/`. On the common (Linux/macOS) path the input
/// has no backslash, so the owned string is returned unchanged with no
/// allocation or scan-and-copy; only Windows paths actually pay the `replace`.
fn normalize_separators(path: String) -> String {
    if path.contains('\\') {
        path.replace('\\', "/")
    } else {
        path
    }
}

/// Convert an LSP `Position` to the parser's 1-based line + 0-based column pair.
/// The parser counts Unicode scalar values (`char`s) and stores columns as
/// `u16`; the LSP column is UTF-16 code units (they agree on BMP-only lines).
/// One place for the `line + 1` / `as u16` conversion the position resolvers all
/// need — see the `position_encoding` note in `initialize_impl`.
pub(crate) fn lsp_pos_to_source(pos: tower_lsp::lsp_types::Position) -> (u32, u16) {
    (pos.line + 1, pos.character as u16)
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

/// Parse a string into an LSP Url, falling back to a clone of `fallback` on
/// error. A failed parse is logged: the fallback silently resolved a location to
/// the wrong document once (the `"<vanilla-cache>"` sentinel, #62), so a stray
/// non-URI reaching here is worth a breadcrumb rather than a silent wrong answer.
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

pub(crate) fn source_position_to_lsp(
    text: &str,
    line: u32,
    source_column: u32,
    encoding: &tower_lsp::lsp_types::PositionEncodingKind,
) -> tower_lsp::lsp_types::Position {
    let character = text.lines().nth(line as usize).map_or(source_column, |l| {
        source_column_to_lsp(l, source_column, encoding)
    });
    tower_lsp::lsp_types::Position { line, character }
}

/// The column half of [`source_position_to_lsp`]: a parser column (0-based
/// chars) rendered in the negotiated encoding, for callers that already hold the
/// line. Diagnostics resolve their line once per file and convert from there, so
/// they share this conversion instead of re-scanning the text per squiggle.
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

/// When the line prefix before the cursor reads `key =` (value not typed yet,
/// so the last good parse has no leaf there), return the key so value
/// completions can still resolve. `line0`/`char0` are LSP 0-based.
///
/// Implementation note: the function keys off the *first* operator in the
/// trimmed line, not the last. A previous version stripped trailing operators
/// then `rsplit` on whitespace, which returned just `"="` for inputs like
/// `has_idea = ==` (the `==` after the value the user is editing was
/// incorrectly consumed as the key). That broke the rescue in
/// `completion::completion_impl`: the wrong key was passed to
/// `value_rules_for_key`, the rule match was empty, value completions were
/// empty, and the generic variable fallback was shown to the user — the
/// "context evaporates after backspace" symptom. Keying on the first operator
/// is also robust against multi-character comparison operators (`==`, `>=`,
/// `!=`, `?=`) which the previous logic treated as a sequence of single
/// characters.
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
    // Require the line to end with an operator: the value position is
    // recognisable by the `=` / `<` / `>` the user is sitting on (or has just
    // typed). Lines like `my_block = {` end with `{` — the user is at an
    // insert position inside the block, not a value position for `my_block`,
    // so the rescue must NOT claim a key here.
    if !trimmed.ends_with(['=', '<', '>', '!', '?']) {
        return None;
    }
    // First operator = the boundary between key and value. Everything after
    // is the value the user is typing (which may contain its own `=` / `<`
    // / `>` from a comparison), so we ignore it and look at the prefix.
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

/// The range of the identifier token the cursor sits in or after, used as the
/// completion replace-range. Without an explicit range the client guesses a
/// word boundary, which it gets wrong right after a backspace across a `=` /
/// `<` / `>` (it filters the candidate list against the operator or whitespace
/// instead of the half-typed identifier, so the ranking goes to noise). Pinning
/// the range to the identifier under the cursor makes the client filter against
/// exactly the typed text. The charset is the bare identifier (`A-Za-z0-9_`):
/// `.` / `:` are token boundaries so member/scope-chain completion restarts the
/// word after them. `line0` / `char0` are LSP 0-based.
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

/// The text of the token between `start_char` (typically
/// `current_token_range`'s start) and the cursor column `char0` — what the
/// user has actually typed of the current token so far. Deliberately keyed
/// off `char0`, not a range end: the token may extend past the cursor in a
/// mid-word edit, and filtering completions against characters the user
/// hasn't typed yet would hide items they could still reach. `line0`/`char0`
/// are LSP 0-based.
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

/// Whether a URI is a localisation file, where `$KEY$` references resolve to
/// other loc entries rather than to game-script rules. One predicate so
/// hover/goto, completion, and validate agree on what counts as loc.
///
/// Both halves of the loc walker's rule apply: a loc extension AND a
/// `localisation` / `localization` directory somewhere above the file, matching
/// what `cwtools_localization`'s `walk_folder` actually loads (its own doc
/// comment warns that extension alone pulls in CI workflows and editor caches —
/// which is how `.github/workflows/ci.yml` earned CW255/CW256).
pub(crate) fn is_loc_file(uri: &str) -> bool {
    if !has_loc_ext(uri) {
        return false;
    }
    // `path_contains_segment` is the shared whole-segment scan, so a file merely
    // NAMED `localisation.yml` doesn't match. Lowercased and `/`-separated as it
    // requires.
    let path = normalize_separators(uri_to_path_str(uri)).to_ascii_lowercase();
    LOC_DIR_NAMES
        .iter()
        .any(|dir| cwtools_info::path_contains_segment(&path, dir))
}

/// Directory names the game (and the loc walker) treat as localisation roots.
/// Mirrors `cwtools_localization`'s private `is_loc_dir_name`, which isn't
/// exported for the LSP to call, and the client's own list in
/// `release/package.json` (activation event and file-watcher glob).
const LOC_DIR_NAMES: [&str; 3] = ["localisation", "localisation_synced", "localization"];

/// Whether a URI has a localisation file extension (`.yml` / `.yaml` / `.csv`),
/// wherever it lives. The wider test behind [`is_loc_file`]: a loc-extension
/// file outside any `localisation` dir is neither loc nor game script, so the
/// game-script paths skip it rather than parsing YAML as Paradox script.
pub(crate) fn has_loc_ext(uri: &str) -> bool {
    std::path::Path::new(uri)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(cwtools_file_manager::is_loc_ext)
}

/// Whether a URI is a `.cwt` rule-config file. These are the schema the rules
/// engine is built from, not game content, so they get their own structural
/// linting (undefined type/enum/single_alias refs + parse errors) rather than
/// the game-script validator. One predicate so validate/hover/completion/goto
/// all agree on what counts as a rule file.
pub(crate) fn is_cwt_file(uri: &str) -> bool {
    uri.to_ascii_lowercase().ends_with(".cwt")
}

/// Whether a URI is one of the game-script file types discovered and validated
/// by the shared file manager.
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

/// Locate the `$KEY$` loc-reference token under the cursor in a localisation
/// line. `col` uses the negotiated LSP position encoding. Returns the
/// referenced key plus the token's `[start, end)` range in that encoding.
/// Mirrors the loc parser: the body must be an identifier (`[A-Za-z0-9_.]`,
/// optionally with a `|colour` suffix) or it's literal text (a currency `$`),
/// not a reference.
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
    // Record every `$`'s (encoded column, byte index).
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
    // Pair consecutive dollars into `$…$` tokens. A non-identifier body (e.g.
    // `$5 today $`) is a stray currency `$`: skip just the opening one so the
    // next dollar can still open a real token.
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

/// A `$…$` body that names a loc key / variable: non-empty, identifier chars only.
fn is_loc_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

/// Best-effort discovery of a base-game install for `game`, checking the usual
/// OS cache directory for persistent caches, used when the client doesn't pass
/// `cacheDir`. Honors `XDG_CACHE_HOME`/`LOCALAPPDATA`, then `~/.cache` (Linux) or
/// `~/Library/Caches` (macOS), and finally the temp dir.
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

/// Steam library locations across platforms. Returns the first existing dir.
/// Used as a fallback when the client passes neither `vanilla` nor `vanillaCache`.
pub(crate) fn discover_vanilla_dir(game: &str) -> Option<std::path::PathBuf> {
    // Map our game id to the Steam "common" install folder name.
    let folder = match game {
        "hoi4" => "Hearts of Iron IV",
        "stellaris" => "Stellaris",
        "eu4" => "Europa Universalis IV",
        "ck2" => "Crusader Kings II",
        "ck3" => "Crusader Kings III",
        "vic2" => "Victoria 2",
        "vic3" => "Victoria 3",
        "ir" => "ImperatorRome",
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

/// Strip matching outer double quotes from a loc desc string for hover display.
/// `"Hello"` → `Hello`, `Hello` → `Hello`, `""` → `` (empty).
/// Strip an inline `#` comment from a loc desc string for hover display.
/// `"value" # comment` → `"value"`, `"value"` → `"value"`.
/// The `#` inside a quoted string is data, not a comment — only the LAST
/// unescaped `#` after the closing quote is stripped.
pub(crate) fn strip_loc_comment(s: &str) -> &str {
    // Find the last `"` in the string. If there is one, only strip `#` after it.
    if let Some(last_quote) = s.rfind('"') {
        let after = &s[last_quote + 1..];
        if let Some(hash) = after.find('#') {
            &s[..last_quote + 1 + hash]
        } else {
            s
        }
    } else {
        // No quotes at all — strip the first `#`.
        if let Some(hash) = s.find('#') {
            &s[..hash]
        } else {
            s
        }
    }
}

/// Extract the display text of a loc line for hover tooltips.
///
/// A loc value is a quoted string, optionally followed by a `# comment`. The
/// value runs from the first `"` to the LAST `"` on the line, so a `#` *inside*
/// the quotes is kept as data (issue #50) while a trailing `# comment` after the
/// closing quote is dropped. An unquoted value just has its inline `# comment`
/// stripped.
pub(crate) fn loc_display_text(desc: &str) -> &str {
    if let Some(rest) = desc.strip_prefix('"') {
        // Quoted: content runs to the last `"` on the line.
        if let Some(end) = rest.rfind('"') {
            return &rest[..end];
        }
        // Unterminated quote: best-effort, strip an inline comment from the rest.
        return strip_loc_comment(rest).trim_end();
    }
    // Unquoted value: drop an inline `# comment`.
    strip_loc_comment(desc).trim_end()
}

/// Human-readable language name for hover display.
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

    /// `default_cache_dir` exists twice — here for the LSP and in
    /// `cwtools_driver` for the CLI — and the driver's doc promises both land
    /// in the same `cwtools` directory. Nothing checked that, so an edit to one
    /// copy could leave the editor and the CLI caching the same install in two
    /// places and each re-indexing what the other already had.
    ///
    /// Reads the ambient environment rather than setting any: both are pure
    /// functions of it, and agreeing on whatever it happens to be is the whole
    /// contract. Setting it would race every other test in this binary.
    #[test]
    fn default_cache_dir_agrees_with_the_driver_copy() {
        let ours = default_cache_dir().expect("the LSP copy always answers");
        assert_eq!(
            Some(&ours),
            cwtools_driver::default_cache_dir().as_ref(),
            "the LSP and CLI cache directories have drifted apart"
        );
        // Every branch of both copies ends in `cwtools`, whichever env var won.
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
        // not loc
        assert!(!is_loc_file("file:///mod/common/ideas/foo.txt"));
        assert!(!is_loc_file("file:///mod/gfx/foo.gfx"));
    }

    #[test]
    fn is_loc_file_requires_a_localisation_directory() {
        // Extension alone routed CI workflows and editor configs into loc
        // validation (false CW255/CW256). Only files the loc walker would load
        // — those under a `localisation` / `localization` dir — count.
        assert!(!is_loc_file("file:///repo/.github/workflows/ci.yml"));
        assert!(!is_loc_file("file:///repo/docker-compose.yaml"));
        assert!(!is_loc_file("file:///repo/data/export.csv"));
        // American spelling and nested subdirectories still count.
        assert!(is_loc_file("file:///mod/localization/foo_l_english.yml"));
        assert!(is_loc_file(
            "file:///mod/localisation/replace/foo_l_english.yml"
        ));
        // Percent-encoded workspace paths decode before the dir check.
        assert!(is_loc_file(
            "file:///home/user/My%20Mod/localisation/foo_l_english.yml"
        ));
        // A file merely NAMED like the directory isn't loc.
        assert!(!is_loc_file("file:///mod/localisation.yml"));
    }

    // The client treats this as loc in three places (activation event, watcher
    // glob, language detection); disagreeing here stripped the files of features.
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
    fn source_positions_use_negotiated_encoding() {
        let text = "😀 alpha";
        assert_eq!(
            source_position_to_lsp(
                text,
                0,
                2,
                &tower_lsp::lsp_types::PositionEncodingKind::UTF16,
            ),
            tower_lsp::lsp_types::Position::new(0, 3)
        );
        assert_eq!(
            source_position_to_lsp(
                text,
                0,
                2,
                &tower_lsp::lsp_types::PositionEncodingKind::UTF32,
            ),
            tower_lsp::lsp_types::Position::new(0, 2)
        );
    }

    #[test]
    fn test_line_value_key() {
        let text = "decision = {\n    has_completed_focus = \n}\n";
        // Cursor right after `= ` on line 1 (0-based), char 26.
        assert_eq!(
            line_value_key(text, 1, 26).as_deref(),
            Some("has_completed_focus")
        );
        // Cursor on the key itself — no `=` before it.
        assert_eq!(line_value_key(text, 1, 10), None);
        // Comparison operators count too.
        let text2 = "block = {\n    num > \n}\n";
        assert_eq!(line_value_key(text2, 1, 10).as_deref(), Some("num"));
    }

    #[test]
    fn test_line_value_key_handles_comparison_operators_in_value() {
        // Regression for the "context evaporates after backspace" symptom:
        // a value-position line where the user is mid-way through typing a
        // comparison operator (e.g. `key = ==` or `key = >= `) must still
        // resolve to the key. The previous implementation keyed off the
        // LAST operator and returned the operator character itself (`"="`),
        // which then failed the value_rules lookup and triggered the
        // generic variable fallback.
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
        // An insert position right after a block-opener `{` is NOT a value
        // position — the rescue must return None, not the block's key.
        assert_eq!(
            line_value_key("my_block = {", 0, 12),
            None,
            "`my_block = {{` is an insert position, not a value position"
        );
    }

    #[test]
    fn test_current_token_range() {
        // Mid-identifier value: `set_variable = { gdpc_conv| }` — the range
        // covers the half-typed `gdpc_conv` so the client filters against it.
        let line = "\tset_variable = { gdpc_conv }";
        let cur = "\tset_variable = { gdpc_conv".chars().count() as u32;
        let r = current_token_range(line, 0, cur);
        assert_eq!(
            r.start.character,
            "\tset_variable = { ".chars().count() as u32
        );
        assert_eq!(r.end.character, cur);

        // Empty value position right after `= `: an empty range at the cursor
        // (start == end) so the client shows the unfiltered list until typing.
        let line2 = "\tvar = ";
        let cur2 = line2.chars().count() as u32;
        let r2 = current_token_range(line2, 0, cur2);
        assert_eq!(r2.start.character, cur2, "no token → empty range at cursor");
        assert_eq!(r2.end.character, cur2);

        // `.` and `:` are token boundaries: typing after them restarts the word.
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
        // Half-typed token: only the characters left of the cursor count, not
        // the rest of the identifier if the cursor sits mid-word.
        let line = "\tset_variable = { gdpc_conv }";
        let start = "\tset_variable = { ".chars().count() as u32;
        let mid = "\tset_variable = { gdpc".chars().count() as u32;
        assert_eq!(current_token_text(line, 0, mid, start), "gdpc");

        // Empty range (cursor right after `= `) → empty token.
        let line2 = "\tvar = ";
        let cur2 = line2.chars().count() as u32;
        assert_eq!(current_token_text(line2, 0, cur2, cur2), "");
    }

    #[test]
    fn test_loc_ref_at_cursor() {
        //                0123456789012345678
        let line = "  k:0 \"a $FOO$ b\"";
        // `$FOO$` spans cols 9..14 (`$`=9, F=10..12, `$`=13).
        let (key, start, end) = loc_ref_at_cursor(line, 11).expect("cursor in $FOO$");
        assert_eq!(key, "FOO");
        assert_eq!((start, end), (9, 14));
        // Cursor outside any ref.
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
        // A stray currency `$` followed by a real ref: only the ref resolves.
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
        // must be absolute for the platform, or Url::from_file_path rejects it
        // and the non-encoding fallback kicks in.
        #[cfg(not(windows))]
        let path = std::path::Path::new("/home/user/My Mod/events/foo.txt");
        #[cfg(windows)]
        let path = std::path::Path::new(r"C:\Users\user\My Mod\events\foo.txt");
        let uri = path_to_uri(path);
        // The URI must percent-encode the space.
        assert!(
            uri.contains("%20") || uri.contains("+"),
            "expected encoded space in URI, got: {}",
            uri
        );
        // Decoding must recover the original path.
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
        // Workspace and file URIs with percent-encoded spaces.
        let ws = Some(workspace_prefix_of("file:///home/user/My%20Mod"));
        let lp = logical_path_from_uri("file:///home/user/My%20Mod/events/foo.txt", &ws);
        assert_eq!(lp, "events/foo.txt", "got: {}", lp);
    }

    // ── canonical_uri (#319) ──────────────────────────────────────────────

    /// A client's spelling of a file, and the path it names. On Windows that is
    /// what VS Code sends — lower-case drive, percent-encoded colon; elsewhere
    /// an encoded ordinary path byte, since there is no drive to encode.
    /// Expectations are written against [`path_to_uri`] of the path rather than
    /// a literal, because *which* spelling wins is the platform's business (on
    /// Windows the round trip also upper-cases the drive letter) and only the
    /// fold onto one key is this module's contract.
    #[cfg(windows)]
    const CLIENT_URI: &str = "file:///d%3A/a/b.txt";
    #[cfg(not(windows))]
    const CLIENT_URI: &str = "file:///a/%62.txt";

    /// The canonical spelling of the file [`CLIENT_URI`] names.
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
        // A space stays percent-encoded; canonicalising must not decode it into
        // an invalid URI.
        assert!(canonical.contains("%20"), "got: {canonical}");
    }

    #[test]
    fn canonical_uri_passes_through_what_it_cannot_convert() {
        // Not a URI at all, and a scheme with no filesystem path behind it.
        assert_eq!(canonical_uri("not a uri"), "not a uri");
        assert_eq!(
            canonical_uri("untitled:Untitled-1"),
            "untitled:Untitled-1",
            "untitled buffers have no path and must survive unchanged"
        );
    }

    /// Percent-encoding of an ordinary path byte folds on every platform, which
    /// is what makes the end-to-end test in `lsp_tests.rs` portable. The URI
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

    /// The fold the double-index bug needed: every way a client can spell the
    /// Windows drive is one key. Which case survives is the round trip's call
    /// (today, upper), so assert they agree rather than naming a winner.
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
        // And that key is idempotent under a second fold.
        assert_eq!(canonical_uri(&canonical), canonical);
    }

    #[test]
    fn canonicalize_url_rewrites_in_place() {
        let expected = client_uri_canonical();
        let mut url: Url = CLIENT_URI.parse().unwrap();
        canonicalize_url(&mut url);
        assert_eq!(url.as_str(), expected);
        // Second application changes nothing.
        canonicalize_url(&mut url);
        assert_eq!(url.as_str(), expected);
    }

    #[test]
    fn canonicalize_uri_string_rewrites_in_place() {
        let mut uri = CLIENT_URI.to_string();
        canonicalize_uri_string(&mut uri);
        assert_eq!(uri, client_uri_canonical());
    }

    /// The workspace folder URI has to be folded at the same boundary the
    /// documents are: `workspace_prefix` comes from it, and
    /// [`logical_path_from_uri`] strips that prefix off canonical document URIs
    /// with a plain compare. `workspace_prefix_of` percent-decodes, so encoding
    /// alone never mattered here — the Windows drive-letter case does.
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
    /// prefix and the document URI can disagree on the drive-letter case, and
    /// `strip_prefix` is a plain byte compare — a miss leaves every logical path
    /// as the absolute one, so no open document resolves a type or a rule.
    /// Canonicalising both ends makes every combination agree.
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
        // `"value" # comment` → `"value" ` (space before # is kept)
        assert_eq!(strip_loc_comment(r#""value" # comment"#), r#""value" "#);
    }

    #[test]
    fn strip_loc_comment_preserves_quoted_value_without_comment() {
        assert_eq!(strip_loc_comment(r#""value""#), r#""value""#);
    }

    #[test]
    fn strip_loc_comment_keeps_hash_inside_quotes_as_data() {
        // The `#` inside a quoted string is data, not a comment.
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
        // Space before # is kept.
        assert_eq!(strip_loc_comment(r#""" # comment"#), r#""" "#);
    }

    #[test]
    fn strip_loc_comment_strips_only_first_hash_after_closing_quote() {
        // Only the first `#` after the closing quote is the comment start.
        // Space before the first # is kept.
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
    // Extracts the value a loc line should show in the hover tooltip: the
    // quoted string with outer quotes removed and any trailing `# comment`
    // dropped, while a `#` *inside* the quotes is preserved as data.

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
        // The reported bug: a `#` inside the quoted value is data, not a comment.
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
