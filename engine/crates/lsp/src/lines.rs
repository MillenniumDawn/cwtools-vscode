//! One document's lines, resolved once per request.
//!
//! Every handler that reports a position has to turn the parser's `(line,
//! column)` into an LSP one, and the naive conversion walks the text to find
//! the line. Doing that per node is O(bytes-before-line) redone for every node
//! in the file, so a document-symbol request on a large file never returns
//! (#541). Build a [`DocLines`] at the handler's entry and index into it
//! instead.

use std::collections::HashMap;

use tower_lsp::lsp_types::{Position, PositionEncodingKind, Range};

use crate::FileTextSnapshot;
use crate::paths::{encoded_position_len, position_byte_index, source_column_to_lsp};

/// A document's lines plus the negotiated position encoding: everything a
/// handler needs to place a range. The parser reports 1-based lines and 0-based
/// CHAR columns, but the client reads columns in the encoding negotiated at
/// `initialize`, so every published position goes through
/// [`source_column_to_lsp`] — the same conversion hover, rename, and the
/// code-action fix edits use. Publishing the raw column instead put a
/// diagnostic and its own quick fix on different spans of any line holding a
/// non-BMP character.
pub(crate) struct DocLines<'a> {
    lines: Vec<&'a str>,
    encoding: PositionEncodingKind,
}

impl<'a> DocLines<'a> {
    pub(crate) fn new(text: &'a str, encoding: PositionEncodingKind) -> Self {
        Self {
            lines: text.lines().collect(),
            encoding,
        }
    }

    /// No document text in hand (the workspace scan's non-open files, ruleset
    /// load errors): positions keep the parser's raw char column and the
    /// squiggle stays one character wide. The encoding is never consulted
    /// without a line to convert against.
    pub(crate) fn none() -> Self {
        Self {
            lines: Vec::new(),
            encoding: PositionEncodingKind::UTF16,
        }
    }

    /// The text of `line` (0-based), empty past the end of the document.
    pub(crate) fn line(&self, line: u32) -> &'a str {
        self.lines.get(line as usize).copied().unwrap_or("")
    }

    /// LSP position for a parser position (0-based `line`, 0-based char `col`).
    pub(crate) fn position(&self, line: u32, col: u32) -> Position {
        let character = self
            .lines
            .get(line as usize)
            .map_or(col, |l| source_column_to_lsp(l, col, &self.encoding));
        Position { line, character }
    }

    /// The parser column (0-based chars) an LSP position names — the inverse of
    /// [`DocLines::position`], clamped to the `u16` the parser stores columns in.
    pub(crate) fn source_column(&self, pos: Position) -> u16 {
        let line = self.line(pos.line);
        let byte = position_byte_index(line, pos.character, &self.encoding);
        line[..byte].chars().count().min(u16::MAX as usize) as u16
    }

    /// The span `token` covers when it starts at (`line`, `column`) — 0-based
    /// line, 0-based char column. Both ends resolve against the same line, so a
    /// token holding non-BMP characters still ends where the client reads it.
    pub(crate) fn token_range(&self, line: u32, column: u32, token: &str) -> Range {
        Range {
            start: self.position(line, column),
            end: self.position(line, column + token.chars().count() as u32),
        }
    }

    /// Whether any document text is held, i.e. whether positions can be resolved
    /// against real lines. False for the workspace scan and the ruleset load.
    pub(crate) fn has_text(&self) -> bool {
        !self.lines.is_empty()
    }

    /// LSP position for a parser range end (0-based `line`, 0-based char `col`),
    /// walked back over whitespace to the last content character.
    ///
    /// The parser records a node's end as the cursor after the node *and* the
    /// whitespace behind it, so a raw end sits on the start of the next token and
    /// published verbatim bleeds onto the following line (#107). Floors at
    /// `start`, so the range cannot invert.
    pub(crate) fn clamped_end_position(&self, line: u32, col: u32, start: Position) -> Position {
        let (mut line, mut col) = (line, col);
        loop {
            let text = self.lines.get(line as usize).copied().unwrap_or("");
            // Chars up to and including the last non-whitespace one in the first
            // `col`, counted in a single pass — this runs per diagnostic, so the
            // old collect-then-trim allocated a String per squiggle.
            let mut content_end = 0;
            for (i, c) in text.chars().take(col as usize).enumerate() {
                if !c.is_whitespace() {
                    content_end = i as u32 + 1;
                }
            }
            if content_end > 0 {
                col = content_end;
                break;
            }
            let Some(prev) = line.checked_sub(1) else {
                col = 0;
                break;
            };
            line = prev;
            col = self
                .lines
                .get(line as usize)
                .map_or(0, |l| l.chars().count() as u32);
        }

        let end = self.position(line, col);
        if (end.line, end.character) < (start.line, start.character) {
            start
        } else {
            end
        }
    }

    /// Range for a secondary span, given the emit site's 1-based line, 0-based
    /// char column and exclusive end. Applies the same whole-line fallback the
    /// primary squiggle uses when there's no document text to walk the end back
    /// through.
    pub(crate) fn related_range(&self, line: u32, col: u16, end: (u32, u16)) -> Range {
        let line = line.saturating_sub(1);
        let start = self.position(line, col as u32);
        let end = if self.has_text() {
            self.clamped_end_position(end.0.saturating_sub(1), end.1 as u32, start)
        } else {
            self.end_position(line, start.character)
        };
        Range { start, end }
    }

    /// End position for a diagnostic whose start is at encoded column `start`:
    /// the end of that line's content, but always at least one past `start` so
    /// the range is never empty. With no line info, a single-character span.
    pub(crate) fn end_position(&self, line: u32, start: u32) -> Position {
        let character = self
            .lines
            .get(line as usize)
            .map_or(0, |l| encoded_position_len(l.trim_end(), &self.encoding))
            .max(start + 1);
        Position { line, character }
    }
}

/// A line index per file for a batch of snapshots, so a loop over sites spread
/// across many files resolves each position by lookup instead of rescanning the
/// file it sits in. References, rename, code lens and workspace symbols all
/// gather sites this way.
pub(crate) fn index_snapshots<'a>(
    texts: &'a HashMap<String, FileTextSnapshot>,
    encoding: &PositionEncodingKind,
) -> HashMap<&'a str, DocLines<'a>> {
    texts
        .iter()
        .map(|(uri, snapshot)| {
            (
                uri.as_str(),
                DocLines::new(&snapshot.text, encoding.clone()),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_use_negotiated_encoding() {
        let text = "😀 alpha";
        assert_eq!(
            DocLines::new(text, PositionEncodingKind::UTF16).position(0, 2),
            Position::new(0, 3)
        );
        assert_eq!(
            DocLines::new(text, PositionEncodingKind::UTF32).position(0, 2),
            Position::new(0, 2)
        );
    }

    /// The line index has to answer exactly what a fresh `text.lines().nth(n)`
    /// scan did, or a handler that switched to it moves the positions it
    /// publishes. Covers the past-the-end case both spellings have to agree on.
    #[test]
    fn line_lookup_matches_a_fresh_scan() {
        let text = "alpha\nbeta\n\ngamma";
        let lines = DocLines::new(text, PositionEncodingKind::UTF16);
        for line0 in 0..6 {
            assert_eq!(
                lines.line(line0),
                text.lines().nth(line0 as usize).unwrap_or(""),
                "line {line0}"
            );
        }
    }

    /// `source_column` is the inverse of `position`, and has to agree with the
    /// free-standing conversion the cursor handlers still use.
    #[test]
    fn source_column_inverts_position() {
        let text = "  😀name𐐀 = 1";
        for encoding in [PositionEncodingKind::UTF16, PositionEncodingKind::UTF32] {
            let lines = DocLines::new(text, encoding.clone());
            for col in 0..text.chars().count() as u32 {
                let pos = lines.position(0, col);
                assert_eq!(
                    lines.source_column(pos),
                    col as u16,
                    "{encoding:?} col {col}"
                );
                assert_eq!(
                    (pos.line + 1, lines.source_column(pos)),
                    crate::paths::lsp_pos_to_source_in_text(text, pos, &encoding),
                );
            }
        }
    }

    #[test]
    fn token_range_spans_the_token_in_encoded_columns() {
        let text = "  😀name𐐀 = 1";
        let utf16 = DocLines::new(text, PositionEncodingKind::UTF16);
        assert_eq!(
            utf16.token_range(0, 2, "😀name𐐀"),
            Range::new(Position::new(0, 2), Position::new(0, 10))
        );
        let utf32 = DocLines::new(text, PositionEncodingKind::UTF32);
        assert_eq!(
            utf32.token_range(0, 2, "😀name𐐀"),
            Range::new(Position::new(0, 2), Position::new(0, 8))
        );
    }
}
