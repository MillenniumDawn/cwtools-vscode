//! (#541). Build a [`DocLines`] at the handler's entry and index into it

use std::collections::HashMap;

use tower_lsp::lsp_types::{Position, PositionEncodingKind, Range};

use crate::FileTextSnapshot;
use crate::paths::{encoded_position_len, position_byte_index, source_column_to_lsp};

/// A document's lines plus the negotiated position encoding: everything a
/// CHAR columns, but the client reads columns in the encoding negotiated at
pub struct DocLines<'a> {
    lines: Vec<&'a str>,
    encoding: PositionEncodingKind,
    trailing_newline: bool,
}

impl<'a> DocLines<'a> {
    pub fn new(text: &'a str, encoding: PositionEncodingKind) -> Self {
        Self {
            lines: text.lines().collect(),
            encoding,
            trailing_newline: text.ends_with('\n'),
        }
    }

    /// squiggle stays one character wide. The encoding is never consulted
    pub(crate) fn none() -> Self {
        Self {
            lines: Vec::new(),
            encoding: PositionEncodingKind::UTF16,
            trailing_newline: false,
        }
    }

    pub(crate) fn line(&self, line: u32) -> &'a str {
        self.lines.get(line as usize).copied().unwrap_or("")
    }

    pub fn position(&self, line: u32, col: u32) -> Position {
        let character = self
            .lines
            .get(line as usize)
            .map_or(col, |l| source_column_to_lsp(l, col, &self.encoding));
        Position { line, character }
    }

    pub(crate) fn source_column(&self, pos: Position) -> u16 {
        let line = self.line(pos.line);
        let byte = position_byte_index(line, pos.character, &self.encoding);
        line[..byte].chars().count().min(u16::MAX as usize) as u16
    }

    pub(crate) fn document_end_position(&self) -> Position {
        let line = if self.trailing_newline {
            self.lines.len() as u32
        } else {
            self.lines.len().saturating_sub(1) as u32
        };
        let last_line = if self.trailing_newline {
            ""
        } else {
            self.lines.last().copied().unwrap_or("")
        };
        Position::new(line, encoded_position_len(last_line, &self.encoding))
    }

    /// The end of `token_range` on its own, for a caller that already holds the
    /// start and would otherwise pay to compute it twice (#471).
    pub(crate) fn token_end(&self, line: u32, column: u32, token: &str) -> Position {
        self.position(line, column + token.chars().count() as u32)
    }

    pub(crate) fn token_range(&self, line: u32, column: u32, token: &str) -> Range {
        Range {
            start: self.position(line, column),
            end: self.token_end(line, column, token),
        }
    }

    /// The document's lines with their 0-based numbers, for a caller that would
    /// otherwise re-run `text.lines()` over a text this already indexed (#471).
    pub(crate) fn iter(&self) -> impl Iterator<Item = (u32, &'a str)> + '_ {
        self.lines.iter().enumerate().map(|(i, l)| (i as u32, *l))
    }

    pub(crate) fn has_text(&self) -> bool {
        !self.lines.is_empty()
    }

    /// published verbatim bleeds onto the following line (#107). Floors at
    pub(crate) fn clamped_end_position(&self, line: u32, col: u32, start: Position) -> Position {
        let (mut line, mut col) = (line, col);
        loop {
            let text = self.lines.get(line as usize).copied().unwrap_or("");
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

    pub(crate) fn end_position(&self, line: u32, start: u32) -> Position {
        let character = self
            .lines
            .get(line as usize)
            .map_or(0, |l| encoded_position_len(l.trim_end(), &self.encoding))
            .max(start + 1);
        Position { line, character }
    }
}

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

    #[test]
    fn document_end_position_handles_empty_and_newline_terminated_text() {
        for (text, expected_line, expected_utf16, expected_utf32) in [
            ("", 0, 0, 0),
            ("x😀", 0, 3, 2),
            ("x😀\n", 1, 0, 0),
            ("x😀\r\n", 1, 0, 0),
            // All-ASCII, and a CRLF terminator: `str::lines` strips the `\r`,
            // so it never reaches the reported column (#471).
            ("abc", 0, 3, 3),
            ("abc\r\n", 1, 0, 0),
        ] {
            assert_eq!(
                DocLines::new(text, PositionEncodingKind::UTF16).document_end_position(),
                Position::new(expected_line, expected_utf16),
                "UTF-16: {text:?}"
            );
            assert_eq!(
                DocLines::new(text, PositionEncodingKind::UTF32).document_end_position(),
                Position::new(expected_line, expected_utf32),
                "UTF-32: {text:?}"
            );
        }
    }

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

    /// The whole index has to agree with the char-walking implementations in
    /// `paths.rs` that the handlers used to call directly: for every line, every
    /// column past the end of it included, out-of-range line indices, and both
    /// encodings. #471 moved several cursor lookups off `paths.rs` and onto
    /// `DocLines`, and this is what says those were the same computation. Any
    /// later attempt to make these conversions cheaper has an oracle to answer
    /// to -- one was tried on this branch, measured at nothing, and dropped.
    #[test]
    fn the_index_agrees_with_the_char_walking_reference() {
        // Pure ASCII; ASCII but for one accented comment, the shape of a real
        // corpus file; non-ASCII throughout, BMP and astral.
        for text in [
            "alpha = 1\n  beta = { gamma = 2 }\n\nlast",
            "alpha = 1\n  # caf\u{e9} comment\n  beta = 2\n",
            "  \u{1f600}name\u{10400} = 1\n\u{4e16}\u{754c} = 2\n",
        ] {
            for encoding in [PositionEncodingKind::UTF16, PositionEncodingKind::UTF32] {
                let lines = DocLines::new(text, encoding.clone());
                for line0 in 0..text.lines().count() as u32 + 2 {
                    let raw = text.lines().nth(line0 as usize);
                    let width = raw.map_or(0, |l| l.chars().count() as u32);
                    for col in 0..width + 3 {
                        let want = raw.map_or(col, |l| source_column_to_lsp(l, col, &encoding));
                        assert_eq!(
                            lines.position(line0, col),
                            Position::new(line0, want),
                            "position {encoding:?} {text:?} line {line0} col {col}"
                        );
                    }
                    for col in 0..width {
                        let pos = lines.position(line0, col);
                        assert_eq!(
                            (pos.line + 1, lines.source_column(pos)),
                            crate::paths::lsp_pos_to_source_in_text(text, pos, &encoding),
                            "source_column {encoding:?} {text:?} line {line0} col {col}"
                        );
                    }
                    assert_eq!(
                        lines.end_position(line0, 0).character,
                        raw.map_or(0, |l| encoded_position_len(l.trim_end(), &encoding))
                            .max(1),
                        "end_position {encoding:?} {text:?} line {line0}"
                    );
                    assert_eq!(
                        lines.token_range(line0, 0, "beta"),
                        Range::new(lines.position(line0, 0), lines.position(line0, 4)),
                        "token_range {encoding:?} {text:?} line {line0}"
                    );
                }
                let last = text.lines().last().unwrap_or("");
                let count = text.lines().count() as u32;
                assert_eq!(
                    lines.document_end_position(),
                    Position::new(
                        if text.ends_with('\n') {
                            count
                        } else {
                            count.saturating_sub(1)
                        },
                        if text.ends_with('\n') {
                            0
                        } else {
                            encoded_position_len(last, &encoding)
                        },
                    ),
                    "document_end_position {encoding:?} {text:?}"
                );
            }
        }
    }

    /// A column past the end of a line clamps to that line's width, because
    /// `chars().take(col)` runs out -- but a column on a line that does not
    /// exist passes straight through, which is what `position`'s `map_or(col,
    /// ..)` promises. Two different answers to "out of range", and both are
    /// load-bearing (#471).
    #[test]
    fn a_column_past_the_end_of_the_line_clamps_to_its_width() {
        for encoding in [PositionEncodingKind::UTF16, PositionEncodingKind::UTF32] {
            let lines = DocLines::new("abc\n", encoding.clone());
            assert_eq!(lines.position(0, 99), Position::new(0, 3), "{encoding:?}");
            assert_eq!(lines.position(9, 99), Position::new(9, 99), "{encoding:?}");
            assert_eq!(lines.source_column(Position::new(0, 99)), 3, "{encoding:?}");
            assert_eq!(lines.source_column(Position::new(9, 99)), 0, "{encoding:?}");
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
