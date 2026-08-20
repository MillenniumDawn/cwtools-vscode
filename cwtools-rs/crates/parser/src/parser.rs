use crate::ast::*;
use cwtools_string_table::string_table::{StringTable, StringTokens};
use std::str::Chars;

/// Whether parsed comments are retained in the AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentMode {
    Preserve,
    Discard,
}

struct Parser<'a> {
    input: &'a str,
    chars: Chars<'a>,
    line: u32,
    col: u16,
    table: &'a StringTable,
    arena: Arena,
    errors: Vec<ParseError>,
    comment_mode: CommentMode,
    /// Clauses currently open, bounded by [`MAX_CLAUSE_DEPTH`].
    depth: u32,
}

/// Saved cursor for backtracking: the remaining-input iterator plus line/col.
#[derive(Clone)]
struct Cursor<'a> {
    chars: Chars<'a>,
    line: u32,
    col: u16,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str, table: &'a StringTable, comment_mode: CommentMode) -> Self {
        Self {
            input,
            chars: input.chars(),
            line: 1,
            col: 0,
            table,
            arena: Arena::new(),
            errors: Vec::new(),
            comment_mode,
            depth: 0,
        }
    }

    fn pos(&self) -> SourcePos {
        SourcePos {
            line: self.line,
            col: self.col,
        }
    }

    /// Byte offset of the cursor into the original input.
    fn byte_pos(&self) -> usize {
        self.input.len() - self.chars.as_str().len()
    }

    fn save(&self) -> Cursor<'a> {
        Cursor {
            chars: self.chars.clone(),
            line: self.line,
            col: self.col,
        }
    }

    fn restore(&mut self, c: Cursor<'a>) {
        self.chars = c.chars;
        self.line = c.line;
        self.col = c.col;
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.chars.next()?;
        if c == '\n' {
            self.line += 1;
            self.col = 0;
        } else if c != '\r' {
            // '\r' is not counted toward column (CRLF line endings: \r\n is one newline).
            self.col = self.col.saturating_add(1);
        }
        Some(c)
    }

    fn peek(&self) -> Option<char> {
        self.chars.clone().next()
    }

    /// Peek at the second character without consuming anything.
    fn peek2(&self) -> Option<char> {
        let mut it = self.chars.clone();
        it.next();
        it.next()
    }

    /// Collect up to `N` upcoming chars into a stack-allocated buffer without
    /// advancing the iterator. Returns the actual number of chars written.
    fn peek_n<const N: usize>(&self) -> ([char; N], usize) {
        let mut buf = ['\0'; N];
        let mut count = 0;
        for c in self.chars.clone().take(N) {
            buf[count] = c;
            count += 1;
        }
        (buf, count)
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn consume_comment(&mut self) -> Option<Comment> {
        if self.peek() == Some('#') {
            let start = self.pos();
            let start_byte = self.byte_pos();
            // Do NOT consume the '#'; keep it in the comment text so that
            // directive comments like '## cardinality = ...' remain intact.
            while let Some(c) = self.peek() {
                if c == '\n' {
                    break;
                }
                self.advance();
            }
            let end = self.pos();
            return Some(Comment {
                text: self.input[start_byte..self.byte_pos()].to_string(),
                pos: SourceRange { start, end },
            });
        }
        None
    }

    /// Consume a comment without materializing its text. For the discard paths
    /// (comments before a value) where `consume_comment`'s String would be
    /// thrown away.
    fn skip_comment(&mut self) -> bool {
        if self.peek() == Some('#') {
            while let Some(c) = self.peek() {
                if c == '\n' {
                    break;
                }
                self.advance();
            }
            return true;
        }
        false
    }

    /// Consume a quoted string without materializing it, using the same
    /// termination rules as [`Parser::scan_quoted`]: it ends at the first
    /// unescaped `"` or at the end of the line.
    fn skip_quoted(&mut self) {
        self.advance(); // opening '"'
        while let Some(c) = self.peek() {
            if c == '\n' {
                break;
            }
            self.advance();
            if c == '"' {
                break;
            }
            if c == '\\' && self.peek().is_some_and(|n| n == '"' || n == '\\') {
                self.advance();
            }
        }
    }

    fn parse_operator(&mut self) -> Option<Operator> {
        let c1 = self.peek()?;
        let c2 = self.peek2();
        let op = match (c1, c2) {
            ('<', Some('=')) => Some(Operator::LessThanOrEqual),
            ('>', Some('=')) => Some(Operator::GreaterThanOrEqual),
            ('!', Some('=')) => Some(Operator::NotEqual),
            ('=', Some('=')) => Some(Operator::EqualEqual),
            ('?', Some('=')) => Some(Operator::QuestionEqual),
            ('=', _) => Some(Operator::Equals),
            ('>', _) => Some(Operator::GreaterThan),
            ('<', _) => Some(Operator::LessThan),
            _ => None,
        };

        if let Some(ref o) = op {
            let len = o.as_str().len();
            for _ in 0..len {
                self.advance();
            }
            self.skip_whitespace();
        }
        op
    }

    fn parse_key(&mut self) -> Option<StringTokens> {
        if self.peek() == Some('"') {
            // Quoted key — same escape/termination rules as a quoted value: it
            // never spans a line, and an unclosed one is an error (a quoted key
            // is never a bare clause entry, so always report).
            let key = self.scan_quoted(true);
            self.skip_whitespace();
            Some(key)
        } else {
            let start = self.byte_pos();
            while let Some(c) = self.peek() {
                // A `?` belongs to the key when it is a `?<default>` null-coalescing
                // selector (`my_var?150 = ...`, the TAOG form), but NOT when it is
                // the `?=` QuestionEqual operator — stop before `?=` so the operator
                // still lexes. `^` carries no such ambiguity (no `^=` operator).
                if c == '?' && self.peek2() == Some('=') {
                    break;
                }
                if is_key_char(c) {
                    self.advance();
                } else {
                    break;
                }
            }
            let s = &self.input[start..self.byte_pos()];
            if s.is_empty() {
                return None;
            }
            self.skip_whitespace();
            Some(self.table.intern(s))
        }
    }

    /// Parse a value. `leafvalue` is true when the value is a bare value in a
    /// clause (e.g. a namelist name), false when it is the RHS of `key = value`.
    /// A quoted string closes strictly at the first unescaped `"` in both cases,
    /// so one-line `a = "x" b = "y"` pairs and namelists mixing quoted and bare
    /// entries (`{ "Sunshine" Demon }`) parse as separate values (matches the
    /// game, which splits a name at its first interior quote).
    fn parse_value(&mut self, leafvalue: bool) -> Option<(Value, SourceRange)> {
        self.skip_whitespace();
        // Skip any comments that appear before the actual value (e.g. value on next line).
        // The AST has no place for comments inside Leaf values, so just discard them.
        while self.skip_comment() {
            self.skip_whitespace();
        }
        let start = self.pos();

        if self.peek() == Some('{') {
            return self
                .parse_clause()
                .map(|value| self.finish_value(start, value));
        }

        if self.peek() == Some('"') {
            let value = self.parse_quoted_value(leafvalue);
            return Some(self.finish_value(start, value));
        }

        // Peek ahead for numbers / booleans / rgb / hsv / metaprogramming
        // rgb / hsv detection.
        // Determine the candidate keyword ("rgb", "rgb360", "hsv", "hsv360") and
        // only proceed when the char after the keyword is absent or non-alphanumeric,
        // so that identifiers like `rgbx` or `rgb3foo` are excluded.
        // We save state and restore it if parse_rgb/parse_hsv returns None, so a
        // bare `rgb` token that isn't followed by `{` doesn't get consumed and lost.
        // Peek 7 chars (max needed: "rgb360" + one char after) without allocating.
        // Only the keywords rgb/hsv/yes/no and the metaprogramming prefix start
        // with one of these chars, so plain numbers/identifiers skip the multi-char
        // peek entirely.
        let (peek7, peek7_len) = match self.peek() {
            Some('r' | 'R' | 'h' | 'H' | 'y' | 'n' | '@') => self.peek_n::<7>(),
            _ => (['\0'; 7], 0),
        };
        let is_rgb = peek7_len >= 3
            && peek7[0].eq_ignore_ascii_case(&'r')
            && peek7[1].eq_ignore_ascii_case(&'g')
            && peek7[2].eq_ignore_ascii_case(&'b');
        let is_hsv = peek7_len >= 3
            && peek7[0].eq_ignore_ascii_case(&'h')
            && peek7[1].eq_ignore_ascii_case(&'s')
            && peek7[2].eq_ignore_ascii_case(&'v');
        if is_rgb {
            let kw_len = if peek7_len >= 6 && peek7[3] == '3' && peek7[4] == '6' && peek7[5] == '0'
            {
                6
            } else {
                3
            };
            let after = if peek7_len > kw_len {
                Some(peek7[kw_len])
            } else {
                None
            };
            if after.is_none_or(|c| !c.is_alphanumeric()) {
                let saved = self.save();
                if let Some(v) = self.parse_color_clause() {
                    return Some(self.finish_value(start, v));
                }
                self.restore(saved);
            }
        }
        if is_hsv {
            let kw_len = if peek7_len >= 6 && peek7[3] == '3' && peek7[4] == '6' && peek7[5] == '0'
            {
                6
            } else {
                3
            };
            let after = if peek7_len > kw_len {
                Some(peek7[kw_len])
            } else {
                None
            };
            if after.is_none_or(|c| !c.is_alphanumeric()) {
                let saved = self.save();
                if let Some(v) = self.parse_color_clause() {
                    return Some(self.finish_value(start, v));
                }
                self.restore(saved);
            }
        }
        if let Some(b) = self.parse_bool_keyword(&peek7, peek7_len) {
            return Some(self.finish_value(start, b));
        }
        // F# metaprogramming prefix is "@\[" (at, backslash, open-bracket): the
        // 3-char literal @\[.
        if peek7_len >= 3 && peek7[0] == '@' && peek7[1] == '\\' && peek7[2] == '[' {
            return self
                .parse_metaprogramming()
                .map(|value| self.finish_value(start, value));
        }

        self.parse_number_or_string()
            .map(|value| self.finish_value(start, value))
    }

    fn finish_value(&mut self, start: SourcePos, value: Value) -> (Value, SourceRange) {
        let end = self.pos();
        self.skip_whitespace();
        (value, SourceRange { start, end })
    }

    /// Parse a `"`-delimited string value (cursor is positioned at the opening
    /// quote). `leafvalue` mirrors [`Parser::parse_value`]: it suppresses the
    /// "unclosed quoted string" error for bare clause entries. A quoted string
    /// closes strictly at the first unescaped `"` and never spans lines, in both
    /// modes (see [`Parser::parse_value`] for why).
    fn parse_quoted_value(&mut self, leafvalue: bool) -> Value {
        // A bare clause entry suppresses the unclosed-string error; a key's RHS
        // reports it.
        Value::QString(self.scan_quoted(!leafvalue))
    }

    /// Scan a `"`-delimited string (cursor at the opening quote) and intern it
    /// WITH its surrounding quotes. Shared by quoted keys and quoted values so
    /// their escape/termination rules cannot drift apart.
    ///
    /// A quoted string never spans a line: a raw newline terminates it (quoted
    /// keys and values are single-line in Clausewitz). It closes at the first
    /// unescaped `"`; only `\"` and `\\` are unescaped, any other `\X` keeps the
    /// backslash (matches F# behaviour). When the string is left unclosed and
    /// `report_unclosed` is set, an "unclosed quoted string" error is pushed —
    /// keys and key-RHS values report; bare clause entries (leafvalues) suppress
    /// it.
    ///
    /// Closing at the first interior quote (rather than trying to keep an
    /// embedded-quote name whole) is deliberate: `"X" Y` is ambiguous —
    /// `"Granada" II` (one name, interior quote) is indistinguishable from
    /// `"Sunshine" Demon` (two values). An older "keep interior quotes as one
    /// value" heuristic consumed past the close and swallowed the clause's `}`,
    /// dropping a whole HOI4 names file with a bogus "unclosed clause"
    /// (cwtools-vscode#42).
    fn scan_quoted(&mut self, report_unclosed: bool) -> StringTokens {
        let quote_start = self.pos();
        let start_byte = self.byte_pos();
        self.advance(); // opening '"'
        // Stay on the borrowed input: only a *collapsed* escape forces an owned
        // copy (already carrying the opening quote), and then only for the
        // segments around it.
        let mut owned: Option<String> = None;
        let mut seg_start = self.byte_pos();
        let mut closed = false;
        while let Some(c) = self.peek() {
            if c == '\n' {
                break;
            }
            if c == '\\' {
                let esc_start = self.byte_pos();
                self.advance(); // consume '\'
                // Only \" -> " and \\ -> \ unescape. Any other \X keeps the
                // backslash and the loop picks up the next char naturally.
                if let Some(e @ ('"' | '\\')) = self.peek() {
                    let buf = owned.get_or_insert_with(|| String::from('"'));
                    buf.push_str(&self.input[seg_start..esc_start]);
                    buf.push(e);
                    self.advance();
                    seg_start = self.byte_pos();
                }
                continue;
            } else if c == '"' {
                self.advance();
                closed = true;
                break;
            }
            self.advance();
        }
        if report_unclosed && !closed {
            self.errors.push(ParseError::Pos(
                quote_start.line,
                quote_start.col,
                format!(
                    "unclosed quoted string starting at line {}",
                    quote_start.line
                ),
            ));
        }
        // An unclosed string still gets a synthesized closing quote.
        let end_byte = self.byte_pos();
        let body_end = if closed { end_byte - 1 } else { end_byte };
        match owned {
            Some(mut buf) => {
                buf.push_str(&self.input[seg_start..body_end]);
                buf.push('"');
                self.table.intern(&buf)
            }
            None if closed => self.table.intern(&self.input[start_byte..end_byte]),
            None => {
                let mut buf = String::with_capacity(body_end - start_byte + 1);
                buf.push_str(&self.input[start_byte..body_end]);
                buf.push('"');
                self.table.intern(&buf)
            }
        }
    }

    /// Recognize a standalone `yes`/`no` boolean keyword from the pre-peeked
    /// buffer. Returns `None` (leaving the cursor untouched) when the token is
    /// not a bare `yes`/`no`, so the caller falls through to the number/string
    /// paths.
    fn parse_bool_keyword(&mut self, peek7: &[char; 7], peek7_len: usize) -> Option<Value> {
        if peek7_len >= 3 && peek7[0] == 'y' && peek7[1] == 'e' && peek7[2] == 's' {
            let saved = self.save();
            for _ in 0..3 {
                self.advance();
            }
            if let Some(c) = self.peek() {
                if !is_value_char(c) {
                    return Some(Value::Bool(true));
                }
            } else {
                return Some(Value::Bool(true));
            }
            // Not a standalone "yes" — backtrack
            self.restore(saved);
        }
        if peek7_len >= 2 && peek7[0] == 'n' && peek7[1] == 'o' {
            let saved = self.save();
            for _ in 0..2 {
                self.advance();
            }
            if let Some(c) = self.peek() {
                if !is_value_char(c) {
                    return Some(Value::Bool(false));
                }
            } else {
                return Some(Value::Bool(false));
            }
            // Not a standalone "no" — backtrack
            self.restore(saved);
        }
        None
    }

    /// Parse a numeric literal (int then float) and fall back to a plain string
    /// when the token isn't a clean number. Returns `None` only when no value
    /// chars are available (empty token).
    fn parse_number_or_string(&mut self) -> Option<Value> {
        // Try integer then float.
        //
        // F# uses `attempt valueInt` then `attempt valueFloat` with backtracking.
        // If parsing a numeric literal fails (e.g. "1444.11.11", "1e5", "0x1A"),
        // FParsec backtracks to the start and valueStr consumes the whole token.
        //
        // We replicate this: save position, scan digits+optional-dot, then check
        // that the NEXT char is NOT a value-char (i.e. the token ends here).  If
        // it is still a value-char we must backtrack and let the fallback string
        // path consume the entire token.
        //
        // Leading '+' is accepted by F#'s pint64/pfloat (issue #2).
        let num_saved = self.save();

        let num_start = self.byte_pos();
        let mut had_dot = false;

        if matches!(self.peek(), Some('-' | '+')) {
            self.advance();
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.advance();
            } else if c == '.' && !had_dot {
                had_dot = true;
                self.advance();
            } else {
                break;
            }
        }
        let num_str = &self.input[num_start..self.byte_pos()];

        // Only commit a numeric result when the token ends here (next char is not
        // a value-char).  Otherwise backtrack so the whole token becomes a String.
        let num_token_ends = match self.peek() {
            None => true,
            Some(c) => !is_value_char(c),
        };

        if num_token_ends && !num_str.is_empty() && num_str != "-" && num_str != "+" {
            // Try int first (strips leading '+' via parse::<i64>)
            if let Ok(i) = num_str.parse::<i64>() {
                return Some(Value::Int(i));
            }
            if let Ok(f) = num_str.parse::<f64>() {
                return Some(Value::Float(f));
            }
        }

        // Numeric parse didn't commit — backtrack fully so the string path gets the
        // whole token (e.g. "1444.11.11", "1e5", "0x1A", lone "-").
        self.restore(num_saved);

        // Fallback: plain string
        let start = self.byte_pos();
        while let Some(c) = self.peek() {
            if is_value_char(c) {
                self.advance();
            } else {
                break;
            }
        }
        let s = &self.input[start..self.byte_pos()];
        if s.is_empty() {
            return None;
        }
        let tokens = self.table.intern(s);
        Some(Value::String(tokens))
    }

    fn parse_clause(&mut self) -> Option<Value> {
        if self.peek() != Some('{') {
            return None;
        }
        if self.depth >= MAX_CLAUSE_DEPTH {
            let start = self.pos();
            self.errors.push(ParseError::Pos(
                start.line,
                start.col,
                format!(
                    "clause nesting deeper than the limit of {MAX_CLAUSE_DEPTH}; this subtree was skipped"
                ),
            ));
            self.skip_clause();
            return Some(Value::Clause(Vec::new()));
        }
        self.advance(); // '{'
        self.skip_whitespace();

        self.depth += 1;
        let mut children = Vec::new();
        loop {
            self.skip_whitespace();
            if self.peek() == Some('}') {
                self.advance();
                break;
            }
            if self.peek().is_none() {
                // Unclosed brace — record error and break to avoid infinite loop
                self.errors.push(ParseError::Pos(
                    self.pos().line,
                    self.pos().col,
                    "unclosed clause: expected '}' before end of file".to_string(),
                ));
                break;
            }
            self.parse_statement(&mut children);
        }
        self.depth -= 1;

        Some(Value::Clause(children))
    }

    /// Consume a `{ ... }` that [`MAX_CLAUSE_DEPTH`] refused to descend into,
    /// iteratively so the skip itself cannot overflow. Comments and quoted
    /// strings are lexed rather than scanned for braces, so a `}` hidden in one
    /// does not end the skip early and derail the rest of the file.
    fn skip_clause(&mut self) {
        self.advance(); // '{'
        let mut open = 1u32;
        while open > 0 {
            match self.peek() {
                None => {
                    self.errors.push(ParseError::Pos(
                        self.pos().line,
                        self.pos().col,
                        "unclosed clause: expected '}' before end of file".to_string(),
                    ));
                    return;
                }
                Some('#') => {
                    self.skip_comment();
                }
                Some('"') => self.skip_quoted(),
                Some('{') => {
                    self.advance();
                    open += 1;
                }
                Some('}') => {
                    self.advance();
                    open -= 1;
                }
                Some(_) => {
                    self.advance();
                }
            }
        }
    }

    fn parse_statement(&mut self, out: &mut Vec<Child>) {
        self.skip_whitespace();

        if self.peek() == Some('#') {
            if self.comment_mode == CommentMode::Preserve {
                if let Some(comment) = self.consume_comment() {
                    let idx = self.arena.push_comment(comment);
                    out.push(Child::Comment(idx));
                }
            } else {
                self.skip_comment();
            }
            return;
        }

        // Try key=value (or key { ... } shorthand)
        let saved = self.pos();
        let saved_cursor = self.save();
        if let Some(key) = self.parse_key() {
            if let Some(op) = self.parse_operator() {
                if let Some((value, value_pos)) = self.parse_value(false) {
                    let end = self.pos();
                    let leaf = Leaf {
                        key,
                        value,
                        op,
                        pos: SourceRange { start: saved, end },
                        value_pos,
                    };
                    let idx = self.arena.push_leaf(leaf);
                    out.push(Child::Leaf(idx));
                    return;
                }
                // key = EOF  — commit as error instead of backtracking
                let end = self.pos();
                let leaf = Leaf {
                    key,
                    value: Value::String(self.table.intern("")),
                    op,
                    pos: SourceRange { start: saved, end },
                    value_pos: SourceRange { start: end, end },
                };
                let idx = self.arena.push_leaf(leaf);
                out.push(Child::Leaf(idx));
                self.errors.push(ParseError::Pos(
                    saved.line,
                    saved.col,
                    format!(
                        "key '{}' has no value after '='",
                        self.table.get_string(key.normal).unwrap_or_default()
                    ),
                ));
                return;
            }
            // No operator — check for shorthand `key { ... }`
            self.skip_whitespace();
            if let Some('{') = self.peek() {
                let value_start = self.pos();
                if let Some(value) = self.parse_clause() {
                    let value_end = self.pos();
                    self.skip_whitespace();
                    let end = self.pos();
                    let leaf = Leaf {
                        key,
                        value,
                        op: Operator::Equals,
                        pos: SourceRange { start: saved, end },
                        value_pos: SourceRange {
                            start: value_start,
                            end: value_end,
                        },
                    };
                    let idx = self.arena.push_leaf(leaf);
                    out.push(Child::Leaf(idx));
                    return;
                }
            }
            // Not a key=value or shorthand; restore and try leaf-value
            self.restore(saved_cursor);
        }

        // Leaf value (bare value)
        if let Some((value, _)) = self.parse_value(true) {
            let end = self.pos();
            let lv = LeafValue {
                value,
                pos: SourceRange { start: saved, end },
            };
            let idx = self.arena.push_leaf_value(lv);
            out.push(Child::LeafValue(idx));
            return;
        }

        // Nothing matched — consume one char to avoid infinite loop on malformed input
        self.advance();
    }

    /// Parse a color clause after its keyword (`rgb`/`hsv`, already peeked and
    /// matched in `parse_value`). Consumes the 3-char keyword, an optional `360`
    /// suffix, an optional `=`, then the `{ ... }` clause.
    fn parse_color_clause(&mut self) -> Option<Value> {
        for _ in 0..3 {
            self.advance();
        }
        self.skip_whitespace();
        if self.peek() == Some('3') {
            let (p3, _) = self.peek_n::<3>();
            if p3 == ['3', '6', '0'] {
                self.advance();
                self.advance();
                self.advance();
                self.skip_whitespace();
            }
        }
        // Support `rgb = { ... }` / `hsv = { ... }` by skipping optional '='.
        if self.peek() == Some('=') {
            self.advance();
            self.skip_whitespace();
        }
        self.parse_clause()
    }

    fn parse_metaprogramming(&mut self) -> Option<Value> {
        // F# prefix is "@\[" (at, backslash, open-bracket).
        // metaprogrammingCharSnippet accepts everything except ']' and '\'.
        // The closing char is ']' (consumed by `ch ']'`).
        // Result token includes the prefix "@\[" and the closing ']'.
        if self.peek() != Some('@') {
            return None;
        }
        let start = self.byte_pos();
        self.advance(); // '@'
        if self.peek() != Some('\\') {
            return None;
        }
        self.advance(); // '\'
        if self.peek() != Some('[') {
            return None;
        }
        self.advance(); // '['

        let mut found_close = false;
        while let Some(c) = self.peek() {
            if c == ']' {
                self.advance();
                found_close = true;
                break;
            }
            // F# metaprogrammingCharSnippet stops at '\' too — just collect content.
            self.advance();
        }
        if !found_close {
            self.errors.push(ParseError::Pos(
                self.pos().line,
                self.pos().col,
                "unclosed metaprogramming bracket: expected ']'".to_string(),
            ));
            return None;
        }
        let s = &self.input[start..self.byte_pos()];
        let tokens = self.table.intern(s);
        Some(Value::String(tokens))
    }

    fn parse(mut self) -> ParsedFile {
        self.skip_whitespace();
        let mut root_children = Vec::new();
        while self.peek().is_some() {
            self.parse_statement(&mut root_children);
        }
        ParsedFile {
            arena: self.arena,
            root_children,
            errors: self.errors,
        }
    }
}

/// Build a `[bool; 128]` ASCII class table at compile time: every ASCII
/// alphanumeric plus the explicit punctuation in `extra` is `true`.
const fn ascii_class(extra: &[u8]) -> [bool; 128] {
    let mut table = [false; 128];
    let mut i = 0u8;
    while i < 128 {
        let b = i;
        if b.is_ascii_alphanumeric() {
            table[i as usize] = true;
        }
        i += 1;
    }
    let mut j = 0;
    while j < extra.len() {
        table[extra[j] as usize] = true;
        j += 1;
    }
    table
}

/// ASCII members of the bare-value (leafvalue / value-string) char class.
/// Non-ASCII chars are handled separately in [`is_value_char`].
static VALUE_CHAR: [bool; 128] = ascii_class(b"_.-:;'[]@+`%/!,<>?$\\|^*&()");

/// ASCII members of the unquoted-key char class. Non-ASCII alphanumerics are
/// handled separately in [`is_key_char`]. A `?` is included for the
/// `my_var?<default>` null-coalescing selector; `parse_key` separately stops
/// before a `?=` so the QuestionEqual operator still lexes.
static KEY_CHAR: [bool; 128] = ascii_class(b"_:@.\"-'[]!<>$^&|()?");

fn is_value_char(c: char) -> bool {
    if c.is_ascii() {
        VALUE_CHAR[c as usize]
    } else {
        c.is_alphanumeric() || c == 'š' || c == 'Š' || c == '’'
    }
}

/// Whether `value` parses as a bare string rather than a boolean or number.
/// This is deliberately conservative: a value that starts numerically may be
/// string-shaped in some cases, but quoting it is always safe.
pub fn is_bare_string_value(value: &str) -> bool {
    !value.is_empty()
        && value != "yes"
        && value != "no"
        && value
            .chars()
            .next()
            .is_some_and(|c| !matches!(c, '+' | '-') && !c.is_ascii_digit())
        && value.chars().all(is_value_char)
}

fn is_key_char(c: char) -> bool {
    if c.is_ascii() {
        KEY_CHAR[c as usize]
    } else {
        c.is_alphanumeric()
    }
}

/// Maximum clause nesting the parser descends into. A deeper clause is consumed
/// without recursing and a [`ParseError`] naming this limit is recorded.
///
/// A stack overflow aborts the process rather than unwinding, so an unbounded
/// parser takes the whole language server down with it on a 48 KB file. The
/// deepest nesting measured across HOI4 vanilla, the Kaiserreich mod and the
/// .cwt ruleset is 24; a release `validate` — whose AST walks recurse far
/// deeper per level than the parser — survives 704 on the 2 MB stack the LSP's
/// worker threads get. 256 clears real content by an order of magnitude and
/// leaves the downstream walkers most of the stack.
pub const MAX_CLAUSE_DEPTH: u32 = 256;

/// Strip UTF-8 BOM if present, then parse with comments preserved.
#[tracing::instrument(skip_all)]
pub fn parse_string(input: &str, table: &StringTable) -> ParsedFile {
    parse_string_with_comment_mode(input, table, CommentMode::Preserve)
}

/// Strip UTF-8 BOM if present, then parse without retaining comment text.
#[tracing::instrument(skip_all)]
pub fn parse_string_without_comments(input: &str, table: &StringTable) -> ParsedFile {
    parse_string_with_comment_mode(input, table, CommentMode::Discard)
}

fn parse_string_with_comment_mode(
    input: &str,
    table: &StringTable,
    comment_mode: CommentMode,
) -> ParsedFile {
    let stripped = input.strip_prefix('\u{FEFF}').unwrap_or(input);
    Parser::new(stripped, table, comment_mode).parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_key_value() {
        let table = StringTable::new();
        let result = parse_string("foo = bar", &table);
        assert_eq!(result.root_children.len(), 1);
    }

    #[test]
    fn discarding_comments_preserves_statements() {
        let table = StringTable::new();
        let parsed = parse_string_without_comments(
            "# ignored\nfirst = 1\n# also ignored\nsecond = 2",
            &table,
        );
        assert!(parsed.arena.comments.is_empty());
        assert_eq!(parsed.root_children.len(), 2);
    }

    #[test]
    fn nested_clause() {
        let table = StringTable::new();
        let result = parse_string("root = { a = 1 }", &table);
        assert_eq!(result.root_children.len(), 1);
    }

    #[test]
    fn names_file_has_no_false_unclosed_clause() {
        // Regression for cwtools-vscode#42: a HOI4 common/names file (quoted
        // names with apostrophes and non-ASCII, nested name_list clauses) must
        // parse with no errors — it was flagged "unclosed clause: expected '}'
        // before end of file" despite balanced braces on older builds. The real
        // trigger is a callsigns clause mixing quoted and BARE values
        // (`"Adler" Demon`): the bare value after a quoted one made the parser
        // swallow the clause's `}` and cascade to EOF.
        let src = "\
GER = {
    male = {
        names = { \"Hans\" \"Jürgen\" \"O'Brien\" \"José\" }
    }
    female = {
        names = { \"Anna\" \"María\" }
    }
    surnames = { \"Müller\" \"D'Angelo\" \"Schröder\" }
    callsigns = { \"Falke\" \"Adler\" Demon }
}
ENG = {
    male = { names = { \"John\" \"Jack\" } }
    female = { names = { \"Mary\" } }
    surnames = { \"Smith\" }
}
";
        let table = StringTable::new();
        let result = parse_string(src, &table);
        assert!(
            result.errors.is_empty(),
            "expected no parse errors, got: {:?}",
            result.errors
        );
        assert_eq!(result.root_children.len(), 2, "two country blocks");
    }

    #[test]
    fn parse_real_file() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../testfiles/performancetest2/common/static_modifiers/cc_colony_events_static_modifiers.txt"
        );
        let input = std::fs::read_to_string(path).unwrap();
        let table = StringTable::new();
        let result = parse_string(&input, &table);
        assert!(!result.root_children.is_empty());
        assert!(!table.is_empty());
    }

    #[test]
    fn parse_angle_bracket_value() {
        let table = StringTable::new();
        let result = parse_string("ethos = <ethos>", &table);
        assert_eq!(result.root_children.len(), 1);
        if let Child::Leaf(idx) = &result.root_children[0] {
            let leaf = &result.arena.leaves[*idx as usize];
            let key = table.get_string(leaf.key.normal).unwrap_or_default();
            assert_eq!(key, "ethos");
            let val = match &leaf.value {
                Value::String(t) | Value::QString(t) => {
                    table.get_string(t.normal).unwrap_or_default()
                }
                _ => panic!("expected string value, got {:?}", leaf.value),
            };
            assert_eq!(val, "<ethos>");
        } else {
            panic!("expected leaf child");
        }
    }

    // -----------------------------------------------------------------------
    // Issue 1: Numeric token corruption — tokens like "1444.11.11" must not
    // be split; the whole thing should become a String.
    // -----------------------------------------------------------------------

    fn value_of(result: &ParsedFile, _table: &StringTable, idx: usize) -> Value {
        match &result.root_children[idx] {
            Child::Leaf(i) => result.arena.leaves[*i as usize].value.clone(),
            Child::LeafValue(i) => result.arena.leaf_values[*i as usize].value.clone(),
            _ => panic!("unexpected child kind"),
        }
    }

    #[test]
    fn date_token_is_string() {
        // "1444.11.11" must parse as one String, not be split at the first dot.
        let table = StringTable::new();
        let result = parse_string("start = 1444.11.11", &table);
        match value_of(&result, &table, 0) {
            Value::String(t) => {
                assert_eq!(table.get_string(t.normal).unwrap_or_default(), "1444.11.11");
            }
            v => panic!("expected String, got {:?}", v),
        }
    }

    #[test]
    fn normal_float_parses() {
        let table = StringTable::new();
        let result = parse_string("x = 2.75", &table);
        match value_of(&result, &table, 0) {
            Value::Float(f) => assert!((f - 2.75).abs() < 1e-9),
            v => panic!("expected Float, got {:?}", v),
        }
    }

    #[test]
    fn hex_like_token_is_string() {
        // "0x1A" — after "0" the 'x' is a value-char so the whole thing is a String.
        let table = StringTable::new();
        let result = parse_string("x = 0x1A", &table);
        match value_of(&result, &table, 0) {
            Value::String(t) => {
                assert_eq!(table.get_string(t.normal).unwrap_or_default(), "0x1A");
            }
            v => panic!("expected String, got {:?}", v),
        }
    }

    #[test]
    fn scientific_like_token_is_string() {
        // "1e5" — after "1" the 'e' is a value-char so the whole token is a String.
        let table = StringTable::new();
        let result = parse_string("x = 1e5", &table);
        match value_of(&result, &table, 0) {
            Value::String(t) => {
                assert_eq!(table.get_string(t.normal).unwrap_or_default(), "1e5");
            }
            v => panic!("expected String, got {:?}", v),
        }
    }

    // -----------------------------------------------------------------------
    // Issue 2: Leading '+' — "+5" should parse as Int(5).
    // -----------------------------------------------------------------------

    #[test]
    fn leading_plus_parses_as_int() {
        let table = StringTable::new();
        let result = parse_string("x = +5", &table);
        match value_of(&result, &table, 0) {
            Value::Int(5) => {}
            v => panic!("expected Int(5), got {:?}", v),
        }
    }

    #[test]
    fn leading_plus_float() {
        let table = StringTable::new();
        let result = parse_string("x = +2.75", &table);
        match value_of(&result, &table, 0) {
            Value::Float(f) => assert!((f - 2.75).abs() < 1e-9),
            v => panic!("expected Float, got {:?}", v),
        }
    }

    // -----------------------------------------------------------------------
    // Issue 3: Quoted-string escapes — only \" and \\ are unescaped.
    // \n in source stays as backslash + 'n', not a newline char.
    // Applies equally to quoted keys and quoted values.
    // -----------------------------------------------------------------------

    #[test]
    fn qstr_backslash_n_stays_literal() {
        // Input: x = "hello\nworld"  — \n must NOT become newline
        let table = StringTable::new();
        let result = parse_string(r#"x = "hello\nworld""#, &table);
        match value_of(&result, &table, 0) {
            Value::QString(t) => {
                let raw = table.get_string(t.normal).unwrap_or_default();
                // The stored string is wrapped in quotes; strip them
                let inner = raw.trim_matches('"');
                assert!(
                    !inner.contains('\n'),
                    "\\n should stay as two chars, not a newline; got: {:?}",
                    inner
                );
                assert!(inner.contains('\\'), "backslash should be preserved");
            }
            v => panic!("expected QString, got {:?}", v),
        }
    }

    #[test]
    fn qstr_escaped_quote_is_unescaped() {
        // Input: x = "say \"hi\""  — \" becomes "
        // Stored token is wrapped in outer quotes: "say "hi""
        // Use strip_prefix/suffix to remove exactly the outermost quotes.
        let table = StringTable::new();
        let result = parse_string(r#"x = "say \"hi\"""#, &table);
        match value_of(&result, &table, 0) {
            Value::QString(t) => {
                let raw = table.get_string(t.normal).unwrap_or_default();
                let inner = raw
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .unwrap_or(&raw);
                assert_eq!(inner, r#"say "hi""#);
            }
            v => panic!("expected QString, got {:?}", v),
        }
    }

    #[test]
    fn qstr_double_backslash_collapses() {
        // Input: x = "a\\b"  — \\ becomes single \
        let table = StringTable::new();
        let result = parse_string(r#"x = "a\\b""#, &table);
        match value_of(&result, &table, 0) {
            Value::QString(t) => {
                let raw = table.get_string(t.normal).unwrap_or_default();
                let inner = raw.trim_matches('"');
                assert_eq!(inner, r"a\b");
            }
            v => panic!("expected QString, got {:?}", v),
        }
    }

    // Helper: the leafvalues (bare values) of `n = { ... }`.
    fn clause_leafvalues(result: &ParsedFile, table: &StringTable) -> Vec<String> {
        let leaf = match &result.root_children[0] {
            Child::Leaf(i) => &result.arena.leaves[*i as usize],
            _ => panic!("expected leaf"),
        };
        let children = match &leaf.value {
            Value::Clause(c) => c,
            v => panic!("expected clause, got {:?}", v),
        };
        children
            .iter()
            .filter_map(|c| match c {
                Child::LeafValue(i) => {
                    let raw = match &result.arena.leaf_values[*i as usize].value {
                        Value::QString(t) | Value::String(t) => {
                            table.get_string(t.normal).unwrap_or_default()
                        }
                        other => panic!("expected string leafvalue, got {:?}", other),
                    };
                    Some(
                        raw.strip_prefix('"')
                            .and_then(|s| s.strip_suffix('"'))
                            .unwrap_or(&raw)
                            .to_string(),
                    )
                }
                _ => None,
            })
            .collect()
    }

    // A quoted string closes at the first interior quote, like the game. A name
    // that embeds quotes therefore splits into several values — but critically it
    // never swallows the clause's `}`. `"X" Y` (a quoted value then a bare value)
    // is the common namelist/callsign shape and MUST parse cleanly; an earlier
    // "keep interior quotes as one value" heuristic ate the `}` and corrupted the
    // rest of the file (cwtools-vscode#42).
    #[test]
    fn qstr_interior_quotes_split_and_never_swallow_brace() {
        let table = StringTable::new();
        // The real trigger: a quoted value followed by a bare value. Two values,
        // one clause, no parse error.
        let result = parse_string(r#"callsigns = { "Sunshine" Demon }"#, &table);
        assert!(
            result.errors.is_empty(),
            "quoted-then-bare must not error: {:?}",
            result.errors
        );
        assert_eq!(
            clause_leafvalues(&result, &table),
            vec!["Sunshine", "Demon"],
            "quoted value then bare value are two separate entries"
        );
        // A name embedding quotes splits at the first interior quote (game
        // behaviour) rather than being kept whole — and still no error.
        let result = parse_string(r#"n = { "Division "Castillejos"" }"#, &table);
        assert!(
            result.errors.is_empty(),
            "interior-quote name must not error: {:?}",
            result.errors
        );
        assert!(
            clause_leafvalues(&result, &table).len() > 1,
            "interior-quote name splits into multiple values"
        );
    }

    // Whitespace-separated quoted strings must STILL split into separate values
    // (e.g. `division_types = { "light_armor" "medium_armor" }`).
    #[test]
    fn qstr_space_separated_strings_still_split() {
        let table = StringTable::new();
        let result = parse_string(
            r#"n = { "light_armor" "medium_armor" "heavy_armor" }"#,
            &table,
        );
        let lvs = clause_leafvalues(&result, &table);
        assert_eq!(
            lvs,
            vec!["light_armor", "medium_armor", "heavy_armor"],
            "space-separated quoted strings must remain separate"
        );
    }

    #[test]
    fn quoted_key_escape_rules_match_value() {
        // "\"key\"" = value — the key's \" should unescape to just "key" without outer quotes
        let table = StringTable::new();
        let result = parse_string(r#""my\"key" = 1"#, &table);
        if let Child::Leaf(i) = &result.root_children[0] {
            let leaf = &result.arena.leaves[*i as usize];
            let key_raw = table.get_string(leaf.key.normal).unwrap_or_default();
            // The key is stored as "my"key" (quoted form), inner part is my"key
            assert!(key_raw.contains('"'), "key should contain unescaped quote");
        } else {
            panic!("expected leaf");
        }
    }

    #[test]
    fn well_formed_quoted_key_position_unchanged() {
        // The newline-termination hardening must not shift a *well-formed*
        // quoted key's recorded position: a single-line quoted key advances the
        // cursor exactly as before (open quote, body, close quote), so its leaf
        // start col is still the column of the opening quote and no error fires.
        let table = StringTable::new();
        let result = parse_string("  \"k\" = 5", &table);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        match &result.root_children[0] {
            Child::Leaf(i) => {
                let leaf = &result.arena.leaves[*i as usize];
                assert_eq!(leaf.pos.start.col, 2, "quoted key starts after 2 spaces");
                assert_eq!(leaf.value, Value::Int(5));
            }
            other => panic!("expected a leaf, got {:?}", other),
        }
    }

    #[test]
    fn leaf_value_position_excludes_trailing_whitespace() {
        let table = StringTable::new();
        let result = parse_string("a = # comment\n  \"value\"  # tail\nb = yes", &table);
        let Child::Leaf(i) = &result.root_children[0] else {
            panic!("expected a leaf");
        };
        let leaf = &result.arena.leaves[*i as usize];
        assert_eq!(
            leaf.value_pos,
            SourceRange {
                start: SourcePos { line: 2, col: 2 },
                end: SourcePos { line: 2, col: 9 },
            }
        );
    }

    #[test]
    fn leaf_value_positions_cover_every_rhs_shape() {
        use crate::fix::{SpanEdit, apply_edits};

        let input = r#"plain=bare # trailing
quoted = "a😀\"b" # trailing
integer = +42
boolean= no
block = { nested = value }
shorthand { nested = value }
"#;
        let table = StringTable::new();
        let result = parse_string(input, &table);
        let edits = result
            .root_children
            .iter()
            .filter_map(|child| match child {
                Child::Leaf(idx) => Some(&result.arena.leaves[*idx as usize]),
                Child::Comment(_) => None,
                other => panic!("expected keyed leaf, got {other:?}"),
            })
            .map(|leaf| {
                let key = table.get_string(leaf.key.normal).unwrap();
                SpanEdit {
                    range: leaf.value_pos,
                    replacement: format!("<{key}>"),
                }
            })
            .collect::<Vec<_>>();

        assert_eq!(
            apply_edits(input, &edits),
            "plain=<plain> # trailing\nquoted = <quoted> # trailing\ninteger = <integer>\nboolean= <boolean>\nblock = <block>\nshorthand <shorthand>\n"
        );
    }

    #[test]
    fn missing_leaf_value_has_an_empty_value_span() {
        let table = StringTable::new();
        let result = parse_string("missing = ", &table);
        let Child::Leaf(idx) = result.root_children[0] else {
            panic!("expected a leaf");
        };
        let leaf = &result.arena.leaves[idx as usize];
        assert_eq!(
            leaf.value_pos,
            SourceRange {
                start: SourcePos { line: 1, col: 10 },
                end: SourcePos { line: 1, col: 10 },
            }
        );
    }

    #[test]
    fn bare_string_value_excludes_non_string_forms() {
        assert!(is_bare_string_value("my_key"));
        assert!(is_bare_string_value("YES"));
        assert!(!is_bare_string_value("yes"));
        assert!(!is_bare_string_value("123"));
        assert!(!is_bare_string_value("+token"));
        assert!(!is_bare_string_value("-token"));
        assert!(!is_bare_string_value("foo=bar"));
    }

    // -----------------------------------------------------------------------
    // Issue 4: CRLF column tracking — '\r' must not advance col.
    // -----------------------------------------------------------------------

    #[test]
    fn crlf_does_not_double_count_column() {
        // Two identical assignments, one with CRLF line ending, one with LF.
        // The column of the second key should be the same in both cases.
        let table = StringTable::new();
        let crlf = parse_string("a = 1\r\nb = 2", &table);
        let lf = parse_string("a = 1\nb = 2", &table);
        let col_crlf = match &crlf.root_children[1] {
            Child::Leaf(i) => crlf.arena.leaves[*i as usize].pos.start.col,
            _ => panic!(),
        };
        let col_lf = match &lf.root_children[1] {
            Child::Leaf(i) => lf.arena.leaves[*i as usize].pos.start.col,
            _ => panic!(),
        };
        assert_eq!(
            col_crlf, col_lf,
            "CRLF should not skew column (crlf={}, lf={})",
            col_crlf, col_lf
        );
    }

    // -----------------------------------------------------------------------
    // Issue 5: Metaprogramming prefix is "@\[" (at, backslash, bracket).
    // -----------------------------------------------------------------------

    #[test]
    fn metaprogramming_prefix() {
        // "@\[expr]" should parse as a String containing "@\[expr]".
        let table = StringTable::new();
        let input = r"x = @\[expr]";
        let result = parse_string(input, &table);
        match value_of(&result, &table, 0) {
            Value::String(t) => {
                let s = table.get_string(t.normal).unwrap_or_default();
                assert_eq!(s, r"@\[expr]");
            }
            v => panic!("expected String, got {:?}", v),
        }
    }

    // -----------------------------------------------------------------------
    // Phase 3: Unclosed key-RHS quoted string must push a parse error and
    // stop at the newline rather than swallowing subsequent statements.
    // -----------------------------------------------------------------------

    #[test]
    fn unclosed_key_rhs_quote_produces_error() {
        let table = StringTable::new();
        // a = "oops has no closing " before the newline.
        // b = 1 must still parse as a separate statement.
        let result = parse_string("a = \"oops\nb = 1", &table);
        assert!(
            !result.errors.is_empty(),
            "expected a parse error for the unclosed quoted string"
        );
        assert_eq!(
            result.root_children.len(),
            2,
            "both statements must be present even after an unclosed quote"
        );
    }

    #[test]
    fn unclosed_key_rhs_quote_at_eof_produces_error() {
        let table = StringTable::new();
        let result = parse_string("a = \"unterminated", &table);
        assert!(
            !result.errors.is_empty(),
            "expected a parse error for an unclosed string at EOF"
        );
    }

    // -----------------------------------------------------------------------
    // PP1: an unclosed *quoted key* must terminate at end-of-line and push an
    // "unclosed quoted string" error, exactly like an unclosed key-RHS quoted
    // value — instead of silently swallowing the following statement. The
    // key-side was missed when parse_quoted_value was hardened for
    // cwtools-vscode#42; both now share one escape-scanning helper.
    // -----------------------------------------------------------------------

    #[test]
    fn unclosed_quoted_key_terminates_at_newline_with_error() {
        let table = StringTable::new();
        // `"foo\nbar = 1\n" = 5\n`: the unclosed quoted key used to span three
        // lines, swallowing the well-formed `bar = 1`. It must now stop at the
        // first newline, flag the unclosed string, and leave `bar = 1` intact.
        let result = parse_string("\"foo\nbar = 1\n\" = 5\n", &table);
        // (b) an unclosed-quoted-string error is recorded.
        assert!(
            !result.errors.is_empty(),
            "expected an unclosed quoted string error, got none"
        );
        // (a) the key terminated at the newline, so (c) `bar = 1` survives as
        // its own leaf rather than being absorbed into a multi-line key.
        let bar = result
            .root_children
            .iter()
            .find_map(|c| match c {
                Child::Leaf(i) => {
                    let leaf = &result.arena.leaves[*i as usize];
                    (table.get_string(leaf.key.normal).unwrap_or_default() == "bar")
                        .then(|| leaf.value.clone())
                }
                _ => None,
            })
            .expect("`bar = 1` must parse as its own leaf, not be swallowed by the quoted key");
        assert_eq!(bar, Value::Int(1));
    }

    #[test]
    fn unclosed_quoted_key_at_eof_produces_error() {
        let table = StringTable::new();
        // A quoted key with no closing quote at EOF must error, not be accepted
        // silently.
        let result = parse_string("\"unterminated = 5", &table);
        assert!(
            !result.errors.is_empty(),
            "expected a parse error for an unclosed quoted key at EOF"
        );
    }

    // A `?<default>` (and `^`) null-coalescing selector on a variable-defining
    // key (`my_var?150 = { ... }`, the TAOG form) must lex as ONE key, not split
    // at the `?` into a bare value + orphaned clause. `^` was already a key char;
    // `?` was not, so the selector form was mis-parsed.

    fn keyed_clause_key(result: &ParsedFile, table: &StringTable, idx: usize) -> String {
        match &result.root_children[idx] {
            Child::Leaf(i) => {
                let leaf = &result.arena.leaves[*i as usize];
                assert!(
                    matches!(leaf.value, Value::Clause(_)),
                    "expected a keyed clause, got {:?}",
                    leaf.value
                );
                table.get_string(leaf.key.normal).unwrap_or_default()
            }
            other => panic!("expected a keyed-clause Leaf, got {:?}", other),
        }
    }

    #[test]
    fn question_selector_key_is_one_keyed_clause() {
        let table = StringTable::new();
        let result = parse_string("war_propaganda_decision_cost?150 = { value = 150 }", &table);
        assert_eq!(
            result.root_children.len(),
            1,
            "the `?150` selector must not split the key from its clause: {:?}",
            result.root_children
        );
        assert_eq!(
            keyed_clause_key(&result, &table, 0),
            "war_propaganda_decision_cost?150"
        );
    }

    #[test]
    fn question_selector_key_pretaog_leaf_form() {
        // The pre-TAOG `my_var?150 = 100` form: one key=value leaf, not a bare
        // value plus an orphan `= 100`.
        let table = StringTable::new();
        let result = parse_string("war_propaganda_decision_cost?150 = 100", &table);
        assert_eq!(result.root_children.len(), 1, "{:?}", result.root_children);
        match &result.root_children[0] {
            Child::Leaf(i) => {
                let leaf = &result.arena.leaves[*i as usize];
                assert_eq!(
                    table.get_string(leaf.key.normal).unwrap_or_default(),
                    "war_propaganda_decision_cost?150"
                );
            }
            other => panic!("expected a single key=value Leaf, got {:?}", other),
        }
    }

    #[test]
    fn caret_selector_key_is_one_keyed_clause() {
        // The `^` variant was already a key char; lock it in as a sibling case.
        let table = StringTable::new();
        let result = parse_string("war_propaganda_decision_cost^foo = { value = 150 }", &table);
        assert_eq!(result.root_children.len(), 1, "{:?}", result.root_children);
        assert_eq!(
            keyed_clause_key(&result, &table, 0),
            "war_propaganda_decision_cost^foo"
        );
    }

    #[test]
    fn question_equal_operator_still_parses() {
        // `?=` is the QuestionEqual operator. Folding `?` into the key char set
        // must NOT break it: `key ?= value` and the no-space `key?= value` both
        // stay a single key=value leaf whose op is QuestionEqual, not a key with
        // a trailing `?`.
        for src in ["foo ?= bar", "foo?= bar"] {
            let table = StringTable::new();
            let result = parse_string(src, &table);
            assert_eq!(
                result.root_children.len(),
                1,
                "`{}` must be one statement: {:?}",
                src,
                result.root_children
            );
            match &result.root_children[0] {
                Child::Leaf(i) => {
                    let leaf = &result.arena.leaves[*i as usize];
                    assert_eq!(
                        table.get_string(leaf.key.normal).unwrap_or_default(),
                        "foo",
                        "`{}` key must be `foo`, not carry the `?`",
                        src
                    );
                    assert_eq!(
                        leaf.op,
                        Operator::QuestionEqual,
                        "`{}` must keep the `?=` operator",
                        src
                    );
                }
                other => panic!("`{}`: expected a Leaf, got {:?}", src, other),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Clause nesting is bounded. A stack overflow aborts the process instead of
    // unwinding, so an unbounded parser lets a 48 KB file kill the language
    // server on every scan — and the file stays on disk, so it kills it again
    // on every restart.
    // -----------------------------------------------------------------------

    /// `root = { a = { a = ... 1 ... } }` nested exactly `depth` clauses deep.
    fn nested_clauses(depth: usize) -> String {
        let mut s = String::with_capacity(depth * 8 + 16);
        s.push_str("root = ");
        for _ in 0..depth {
            s.push_str("{ a = ");
        }
        s.push('1');
        for _ in 0..depth {
            s.push_str(" }");
        }
        s
    }

    fn depth_errors(result: &ParsedFile) -> Vec<String> {
        result
            .errors
            .iter()
            .map(|e| e.to_string())
            .filter(|m| m.contains("nesting"))
            .collect()
    }

    #[test]
    fn nesting_past_the_depth_cap_errors_instead_of_overflowing() {
        // 30,000 levels is the reproducer that aborted with "fatal runtime
        // error: stack overflow" on the default 8 MB main stack.
        let table = StringTable::new();
        let result = parse_string(&nested_clauses(30_000), &table);
        let errs = depth_errors(&result);
        assert_eq!(errs.len(), 1, "one depth error, got: {:?}", result.errors);
        assert!(
            errs[0].contains(&MAX_CLAUSE_DEPTH.to_string()),
            "the error must name the limit, got: {}",
            errs[0]
        );
        assert_eq!(result.root_children.len(), 1, "the root statement survives");
    }

    #[test]
    fn nesting_at_the_depth_cap_is_unaffected() {
        let table = StringTable::new();
        // 24 is the deepest nesting in HOI4 vanilla / Kaiserreich / the ruleset.
        for depth in [1, 24, MAX_CLAUSE_DEPTH as usize] {
            let result = parse_string(&nested_clauses(depth), &table);
            assert!(
                result.errors.is_empty(),
                "depth {} must parse clean, got: {:?}",
                depth,
                result.errors
            );
            let mut value = value_of(&result, &table, 0);
            for _ in 0..depth {
                let Value::Clause(children) = value else {
                    panic!("depth {}: expected a clause, got {:?}", depth, value);
                };
                match children.as_slice() {
                    [Child::Leaf(i)] => value = result.arena.leaves[*i as usize].value.clone(),
                    other => panic!("depth {}: expected one child, got {:?}", depth, other),
                }
            }
            assert_eq!(value, Value::Int(1), "depth {} bottomed out wrong", depth);
        }
    }

    #[test]
    fn depth_cap_skips_only_the_over_deep_clause() {
        // The skipped subtree must not cascade: braces hidden in a comment or a
        // quoted string inside it are not counted, so the skip lands on the
        // matching `}` and the next statement still parses.
        let table = StringTable::new();
        let deep = nested_clauses(MAX_CLAUSE_DEPTH as usize + 1);
        let src = format!("{deep}\n# trailing }} brace\nafter = \"}}{{\"\nlast = 2\n");
        let result = parse_string(&src, &table);
        assert_eq!(depth_errors(&result).len(), 1, "{:?}", result.errors);
        let last = result
            .root_children
            .iter()
            .find_map(|c| match c {
                Child::Leaf(i) => {
                    let leaf = &result.arena.leaves[*i as usize];
                    (table.get_string(leaf.key.normal).unwrap_or_default() == "last")
                        .then(|| leaf.value.clone())
                }
                _ => None,
            })
            .expect("`last = 2` must survive the skipped subtree");
        assert_eq!(last, Value::Int(2));
    }

    #[test]
    fn single_line_over_u16_max_chars_does_not_panic() {
        // col is a u16; a single line past 65,535 chars must saturate instead
        // of overflowing (debug builds panic on overflow, release wraps and
        // corrupts positions). Regression for the col += 1 add in advance().
        let table = StringTable::new();
        let src = format!("foo = {}", "a".repeat(70_000));
        let result = parse_string(&src, &table);
        assert_eq!(
            result.root_children.len(),
            1,
            "parse must complete without panicking"
        );
    }
}
