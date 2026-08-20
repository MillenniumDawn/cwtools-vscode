//! Suggested-fix payloads carried by diagnostics.
//!
//! A [`SuggestedFix`] is pure metadata attached to a diagnostic at its emit
//! site, where the AST node (and hence its [`SourceRange`]) is still in scope —
//! the span can't be reconstructed later from a diagnostic's start position
//! alone. The engine never applies a fix; the CLI `fix` subcommand and the LSP
//! code-action provider consume these.
//!
//! Ranges use the same convention as [`SourcePos`]: 1-based `line`, 0-based
//! char `col`. Loc diagnostics (1-based columns) convert before building a fix.
//! v1 is single-span, single-line edits only.

use crate::ast::{SourcePos, SourceRange};
use smallvec::SmallVec;

/// A single-span text replacement. An empty `replacement` deletes the span.
#[derive(Debug, Clone, PartialEq)]
pub struct SpanEdit {
    pub range: SourceRange,
    pub replacement: String,
}

/// A named set of edits that resolves one diagnostic. v1 fixes carry exactly one
/// edit; the inline `SmallVec` keeps the common case allocation-free.
#[derive(Debug, Clone, PartialEq)]
pub struct SuggestedFix {
    pub title: String,
    pub edits: SmallVec<[SpanEdit; 1]>,
    /// The loc key a "create missing localisation key" fix would add, for a
    /// diagnostic whose fix can't be expressed as an in-file span edit (the
    /// key doesn't exist anywhere yet, so there's no span to replace — it may
    /// need a new line in an existing loc file, or a whole new file). `None`
    /// for every other fix. A consumer that only applies `edits` (the CLI
    /// `fix` subcommand, the LSP's per-diagnostic quickfix / `source.fixAll`)
    /// sees an empty `edits` for one of these and skips it; the LSP's
    /// dedicated "Create missing localisation key" code action is the only
    /// consumer that reads this field (see `lsp/src/code_action.rs`).
    pub create_loc_key: Option<String>,
}

impl SuggestedFix {
    /// A one-edit fix replacing `range` with `replacement`.
    pub fn replace(
        title: impl Into<String>,
        range: SourceRange,
        replacement: impl Into<String>,
    ) -> Self {
        SuggestedFix {
            title: title.into(),
            edits: smallvec::smallvec![SpanEdit {
                range,
                replacement: replacement.into(),
            }],
            create_loc_key: None,
        }
    }

    /// A one-edit deletion of `range` (empty replacement).
    pub fn delete(title: impl Into<String>, range: SourceRange) -> Self {
        Self::replace(title, range, String::new())
    }

    /// A fix with no span edit, carrying only the loc key a dedicated
    /// out-of-file action should create. See [`create_loc_key`](Self::create_loc_key).
    pub fn create_loc_key(title: impl Into<String>, key: impl Into<String>) -> Self {
        SuggestedFix {
            title: title.into(),
            edits: SmallVec::new(),
            create_loc_key: Some(key.into()),
        }
    }
}

/// Range of a key token that begins at `start` and is `char_len` characters
/// long, on one line. Targets a rename/replacement at a block or leaf key
/// without touching its value (e.g. CW253 `set_empire_name` -> `set_name`).
pub fn key_token_range(start: SourcePos, char_len: usize) -> SourceRange {
    SourceRange {
        start,
        end: SourcePos {
            line: start.line,
            col: start.col.saturating_add(char_len as u16),
        },
    }
}

/// Byte offset of the start of each line, indexed by 0-based line number (source
/// line 1 is index 0). Used to convert a (line, char-col) [`SourcePos`] into a
/// byte offset when applying an edit.
pub fn line_start_bytes(text: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// Byte offset of a [`SourcePos`] (1-based `line`, 0-based char `col`). Walks
/// `col` characters from the line start so a multibyte char counts as one
/// column. A position past the line's end (or the text) clamps to the line end /
/// text end; `col` at the line's newline resolves to the newline's offset.
pub fn pos_to_byte(text: &str, line_starts: &[usize], pos: SourcePos) -> usize {
    let line_idx = pos.line.saturating_sub(1) as usize;
    let Some(&line_start) = line_starts.get(line_idx) else {
        return text.len();
    };
    let mut byte = line_start;
    for (col, ch) in (0_u16..).zip(text[line_start..].chars()) {
        if col >= pos.col || ch == '\n' {
            break;
        }
        byte += ch.len_utf8();
    }
    byte
}

/// Resolve one file's planned edits: drop any that overlap an already-kept edit,
/// returning the survivors in file order plus the `tag`s of the ones dropped.
/// Overlapping edits can't both be applied without one corrupting the other, so
/// the later one is skipped and handed back for the caller to report — the CLI
/// `fix` subcommand warns per file, the LSP `source.fixAll` action leaves the
/// dropped diagnostic to its own quick fix.
///
/// `tag` is whatever identifies an edit's source to the caller (the diagnostic
/// code, an index); it is only carried through to the skipped list.
pub fn plan_file_edits<T>(text: &str, mut planned: Vec<(T, SpanEdit)>) -> (Vec<SpanEdit>, Vec<T>) {
    let starts = line_start_bytes(text);
    // Sort by start byte ascending so overlap detection is a single forward scan.
    planned.sort_by_key(|(_, e)| pos_to_byte(text, &starts, e.range.start));
    let mut kept: Vec<SpanEdit> = Vec::new();
    let mut skipped: Vec<T> = Vec::new();
    let mut last_end = 0usize;
    let mut first = true;
    for (tag, edit) in planned {
        let s = pos_to_byte(text, &starts, edit.range.start);
        let e = pos_to_byte(text, &starts, edit.range.end);
        if !first && s < last_end {
            skipped.push(tag);
            continue;
        }
        last_end = e;
        first = false;
        kept.push(edit);
    }
    (kept, skipped)
}

/// Apply single-span edits to `text`, returning the new text. Edits are resolved
/// to byte ranges, sorted by start descending, and applied later-first so earlier
/// offsets stay valid. Overlaps are not checked here — the caller filters them
/// with [`plan_file_edits`]; the single-edit fixtures never overlap.
pub fn apply_edits(text: &str, edits: &[SpanEdit]) -> String {
    let starts = line_start_bytes(text);
    let mut ranges: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|e| {
            (
                pos_to_byte(text, &starts, e.range.start),
                pos_to_byte(text, &starts, e.range.end),
                e.replacement.as_str(),
            )
        })
        .collect();
    ranges.sort_by_key(|r| std::cmp::Reverse(r.0));
    let mut out = text.to_string();
    for (s, e, repl) in ranges {
        if s <= e && e <= out.len() {
            out.replace_range(s..e, repl);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: u32, col: u16) -> SourcePos {
        SourcePos { line, col }
    }

    #[test]
    fn pos_to_byte_walks_chars_including_multibyte() {
        let text = "ab\ncafé x\n";
        let starts = line_start_bytes(text);
        // line 1, col 2 -> just past "ab" (byte 2).
        assert_eq!(pos_to_byte(text, &starts, pos(1, 2)), 2);
        // line 2 starts at byte 3; col 4 is past "café" (é is 2 bytes) -> the space.
        assert_eq!(pos_to_byte(text, &starts, pos(2, 4)), 3 + 5);
        // col past the line clamps at the newline, not into the next line.
        assert_eq!(pos_to_byte(text, &starts, pos(1, 99)), 2);
    }

    #[test]
    fn apply_single_replacement() {
        let text = "set_empire_name = { }\n";
        let edit = SpanEdit {
            range: key_token_range(pos(1, 0), "set_empire_name".len()),
            replacement: "set_name".to_string(),
        };
        assert_eq!(apply_edits(text, &[edit]), "set_name = { }\n");
    }

    #[test]
    fn apply_multiple_edits_is_order_independent() {
        // Two edits on one line applied later-first: the earlier edit's offsets
        // stay valid regardless of the order they appear in the slice.
        let text = "aaaa bbbb\n";
        let e1 = SpanEdit {
            range: SourceRange {
                start: pos(1, 0),
                end: pos(1, 4),
            },
            replacement: "X".to_string(),
        };
        let e2 = SpanEdit {
            range: SourceRange {
                start: pos(1, 5),
                end: pos(1, 9),
            },
            replacement: "Y".to_string(),
        };
        assert_eq!(apply_edits(text, &[e1.clone(), e2.clone()]), "X Y\n");
        assert_eq!(apply_edits(text, &[e2, e1]), "X Y\n");
    }

    fn span(sl: u32, sc: u16, el: u32, ec: u16, repl: &str) -> SpanEdit {
        SpanEdit {
            range: SourceRange {
                start: pos(sl, sc),
                end: pos(el, ec),
            },
            replacement: repl.to_string(),
        }
    }

    #[test]
    fn plan_keeps_disjoint_edits_regardless_of_input_order() {
        let text = "aaaa bbbb\n";
        let forward = vec![("A", span(1, 0, 1, 4, "X")), ("B", span(1, 5, 1, 9, "Y"))];
        let reversed = vec![("B", span(1, 5, 1, 9, "Y")), ("A", span(1, 0, 1, 4, "X"))];
        for planned in [forward, reversed] {
            let (kept, skipped) = plan_file_edits(text, planned);
            assert!(skipped.is_empty());
            assert_eq!(kept.len(), 2);
            assert_eq!(apply_edits(text, &kept), "X Y\n");
        }
    }

    #[test]
    fn plan_skips_the_later_of_two_overlapping_edits() {
        let text = "aaaa bbbb\n";
        // "aaaa b" and "bbbb" share column 5.
        let planned = vec![("A", span(1, 0, 1, 6, "X")), ("B", span(1, 5, 1, 9, "Y"))];
        let (kept, skipped) = plan_file_edits(text, planned);
        assert_eq!(kept.len(), 1);
        assert_eq!(skipped, vec!["B"], "the overlapping edit is reported back");
        assert_eq!(apply_edits(text, &kept), "Xbbb\n");
    }

    #[test]
    fn plan_keeps_edits_that_only_touch_at_a_boundary() {
        // An edit starting exactly where the previous one ends is not an overlap.
        let text = "aaaabbbb\n";
        let planned = vec![("A", span(1, 0, 1, 4, "X")), ("B", span(1, 4, 1, 8, "Y"))];
        let (kept, skipped) = plan_file_edits(text, planned);
        assert!(skipped.is_empty());
        assert_eq!(apply_edits(text, &kept), "XY\n");
    }

    #[test]
    fn plan_orders_across_lines_by_byte_offset() {
        // Sorting is by resolved byte offset, so a later line never sorts first.
        let text = "one\ntwo\nthree\n";
        let planned = vec![("C", span(3, 0, 3, 5, "3")), ("A", span(1, 0, 1, 3, "1"))];
        let (kept, skipped) = plan_file_edits(text, planned);
        assert!(skipped.is_empty());
        assert_eq!(kept[0].replacement, "1");
        assert_eq!(kept[1].replacement, "3");
        assert_eq!(apply_edits(text, &kept), "1\ntwo\n3\n");
    }

    #[test]
    fn plan_of_an_empty_list_is_empty() {
        let (kept, skipped) = plan_file_edits::<&str>("x = y\n", Vec::new());
        assert!(kept.is_empty() && skipped.is_empty());
    }

    #[test]
    fn create_loc_key_fix_carries_the_key_with_no_span_edits() {
        // A "create missing localisation key" fix has nothing to replace — it
        // carries the key for a dedicated out-of-file action instead, and
        // must not be mistaken for an in-file edit by a consumer that only
        // walks `edits` (the CLI `fix` subcommand, `source.fixAll`).
        let fix =
            SuggestedFix::create_loc_key("Create localisation key my_thing_desc", "my_thing_desc");
        assert_eq!(fix.title, "Create localisation key my_thing_desc");
        assert!(fix.edits.is_empty());
        assert_eq!(fix.create_loc_key.as_deref(), Some("my_thing_desc"));
    }

    #[test]
    fn span_edit_fixes_leave_create_loc_key_unset() {
        let replace = SuggestedFix::replace("x", key_token_range(pos(1, 0), 1), "y");
        let delete = SuggestedFix::delete("x", key_token_range(pos(1, 0), 1));
        assert_eq!(replace.create_loc_key, None);
        assert_eq!(delete.create_loc_key, None);
    }
}
