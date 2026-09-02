use crate::ast::{Arena, Child, ParsedFile, SourcePos, SourceRange, Value};
use crate::fix::{SpanEdit, line_start_bytes, plan_file_edits, pos_to_byte};
use crate::parser::parse_string;
use cwtools_string_table::string_table::StringTable;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndentStyle {
    Space,
    Tab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatOptions {
    pub indent_style: IndentStyle,
    pub indent_size: u32,
    pub trim_trailing_whitespace: bool,
    pub insert_final_newline: bool,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            indent_style: IndentStyle::Space,
            indent_size: 4,
            trim_trailing_whitespace: true,
            insert_final_newline: true,
        }
    }
}

impl FormatOptions {
    pub fn with_editor(mut self, tab_size: u32, insert_spaces: bool) -> Self {
        self.indent_style = if insert_spaces {
            IndentStyle::Space
        } else {
            IndentStyle::Tab
        };
        self.indent_size = tab_size.clamp(1, 16);
        self
    }

    fn unit(&self) -> String {
        match self.indent_style {
            IndentStyle::Tab => "\t".to_string(),
            IndentStyle::Space => " ".repeat(self.indent_size.clamp(1, 16) as usize),
        }
    }
}

pub fn format_text(input: &str, table: &StringTable, opts: &FormatOptions) -> Option<String> {
    let bom = input.starts_with('\u{FEFF}');
    let body = input.strip_prefix('\u{FEFF}').unwrap_or(input);
    let parsed = parse_ok(body, table)?;
    let mut printed = print_file(body, table, &parsed, opts);
    if opts.insert_final_newline {
        if !printed.ends_with('\n') {
            let newline = newline_of(body);
            printed.push_str(newline);
        }
    } else {
        while printed.ends_with('\n') || printed.ends_with('\r') {
            printed.pop();
        }
    }
    if bom {
        printed.insert(0, '\u{FEFF}');
    }
    Some(printed)
}

pub fn format_edits(input: &str, table: &StringTable, opts: &FormatOptions) -> Vec<SpanEdit> {
    let Some(formatted) = format_text(input, table, opts) else {
        return Vec::new();
    };
    if formatted == input {
        return Vec::new();
    }
    let (kept, _) = plan_file_edits(input, vec![((), whole_file_edit(input, formatted))]);
    kept
}

pub fn format_range_edits(
    input: &str,
    table: &StringTable,
    opts: &FormatOptions,
    range: SourceRange,
) -> Vec<SpanEdit> {
    let bom = input.starts_with('\u{FEFF}');
    let body = input.strip_prefix('\u{FEFF}').unwrap_or(input);
    let Some(parsed) = parse_ok(body, table) else {
        return Vec::new();
    };
    let line_starts = line_start_bytes(body);
    let range_start = pos_to_byte(body, &line_starts, range.start);
    let range_end = pos_to_byte(body, &line_starts, range.end);
    if range_start == 0 && range_end >= body.len() {
        return format_edits(input, table, opts);
    }
    let (indent, slice) = select_span(
        &parsed.arena,
        &parsed.root_children,
        0,
        range_start,
        range_end,
        body,
        &line_starts,
    );
    if slice.is_empty() {
        return Vec::new();
    }
    let first = child_span(&parsed.arena, &slice[0]);
    let last = child_span(&parsed.arena, &slice[slice.len() - 1]);
    let start = SourcePos {
        line: first.start.line,
        col: 0,
    };
    let replace_from = pos_to_byte(body, &line_starts, start);
    let replace_to = pos_to_byte(body, &line_starts, last.end);
    let mut printer = Printer::new(body, table, &parsed.arena, opts, indent);
    printer.emit_children(slice);
    let mut replacement = printer.out;
    if opts.trim_trailing_whitespace {
        while replacement.ends_with(' ') || replacement.ends_with('\t') {
            replacement.pop();
        }
    }
    let original = body.get(replace_from..replace_to).unwrap_or("");
    if replacement == original {
        return Vec::new();
    }
    // Positions were measured on the BOM-stripped body. A leading U+FEFF is
    let (kept, _) = plan_file_edits(
        input,
        vec![(
            (),
            SpanEdit {
                range: SourceRange {
                    start: shift_col_for_bom(start, bom),
                    end: shift_col_for_bom(last.end, bom),
                },
                replacement,
            },
        )],
    );
    kept
}

fn shift_col_for_bom(pos: SourcePos, bom: bool) -> SourcePos {
    if bom && pos.line == 1 {
        SourcePos {
            line: 1,
            col: pos.col.saturating_add(1),
        }
    } else {
        pos
    }
}

fn parse_ok(input: &str, table: &StringTable) -> Option<ParsedFile> {
    let parsed = parse_string(input, table);
    parsed.errors.is_empty().then_some(parsed)
}

fn newline_of(input: &str) -> &'static str {
    if input.contains("\r\n") { "\r\n" } else { "\n" }
}

fn whole_file_edit(input: &str, formatted: String) -> SpanEdit {
    SpanEdit {
        range: SourceRange {
            start: SourcePos { line: 1, col: 0 },
            end: end_pos(input),
        },
        replacement: formatted,
    }
}

fn end_pos(text: &str) -> SourcePos {
    let mut line = 1u32;
    let mut col = 0u16;
    for c in text.chars() {
        if c == '\n' {
            line += 1;
            col = 0;
        } else if c != '\r' {
            col = col.saturating_add(1);
        }
    }
    SourcePos { line, col }
}

fn print_file(
    input: &str,
    table: &StringTable,
    parsed: &ParsedFile,
    opts: &FormatOptions,
) -> String {
    let mut printer = Printer::new(input, table, &parsed.arena, opts, 0);
    printer.emit_children(&parsed.root_children);
    if opts.trim_trailing_whitespace {
        while printer.out.ends_with(' ') || printer.out.ends_with('\t') {
            printer.out.pop();
        }
    }
    printer.out
}

fn select_span<'a>(
    arena: &'a Arena,
    children: &'a [Child],
    indent: u32,
    range_start: usize,
    range_end: usize,
    input: &str,
    line_starts: &[usize],
) -> (u32, &'a [Child]) {
    for child in children {
        let Some(clause) = arena.keyed_clause(child) else {
            continue;
        };
        let Child::Leaf(i) = child else { continue };
        let leaf = &arena.leaves[*i as usize];
        let Some(open) = find_open_brace(input, leaf.value_pos, line_starts) else {
            continue;
        };
        let close = pos_to_byte(input, line_starts, leaf.pos.end);
        if range_start > open && range_end <= close {
            return select_span(
                arena,
                clause.children,
                indent + 1,
                range_start,
                range_end,
                input,
                line_starts,
            );
        }
    }
    let mut first = None;
    let mut last = 0usize;
    for (i, child) in children.iter().enumerate() {
        let span = child_span(arena, child);
        let s = pos_to_byte(input, line_starts, span.start);
        let e = pos_to_byte(input, line_starts, span.end);
        if s < range_end && range_start < e {
            if first.is_none() {
                first = Some(i);
            }
            last = i;
        }
    }
    match first {
        Some(f) => (indent, &children[f..=last]),
        None => (indent, &[]),
    }
}

fn child_span(arena: &Arena, child: &Child) -> SourceRange {
    match child {
        Child::Leaf(i) => {
            let leaf = &arena.leaves[*i as usize];
            SourceRange {
                start: leaf.pos.start,
                end: leaf.value_pos.end,
            }
        }
        Child::LeafValue(i) => arena.leaf_values[*i as usize].pos,
        Child::Comment(i) => arena.comments[*i as usize].pos,
    }
}

fn find_open_brace(input: &str, value_pos: SourceRange, line_starts: &[usize]) -> Option<usize> {
    let s = pos_to_byte(input, line_starts, value_pos.start);
    let e = pos_to_byte(input, line_starts, value_pos.end).min(input.len());
    let s = s.min(e);
    input[s..e].find('{').map(|i| s + i)
}

struct Printer<'a> {
    input: &'a str,
    table: &'a StringTable,
    arena: &'a Arena,
    opts: &'a FormatOptions,
    out: String,
    indent: u32,
    newline: &'static str,
    line_starts: Vec<usize>,
    unit: String,
}

impl<'a> Printer<'a> {
    fn new(
        input: &'a str,
        table: &'a StringTable,
        arena: &'a Arena,
        opts: &'a FormatOptions,
        indent: u32,
    ) -> Self {
        Self {
            input,
            table,
            arena,
            opts,
            out: String::new(),
            indent,
            newline: newline_of(input),
            line_starts: line_start_bytes(input),
            unit: opts.unit(),
        }
    }

    fn emit_children(&mut self, children: &[Child]) {
        for (i, child) in children.iter().enumerate() {
            if self.is_eol_comment(children, i) {
                self.out.push(' ');
                self.emit_comment(child);
                continue;
            }
            if i > 0 {
                self.push_newline();
                if self.had_blank_line(&children[i - 1], child) {
                    if !self.opts.trim_trailing_whitespace {
                        self.write_indent();
                    }
                    self.push_newline();
                }
            }
            self.write_indent();
            self.emit_child(child);
        }
    }

    fn emit_child(&mut self, child: &Child) {
        match child {
            Child::Leaf(i) => self.emit_leaf(*i),
            Child::LeafValue(i) => self.emit_leaf_value(*i),
            Child::Comment(i) => {
                let text = &self.arena.comments[*i as usize].text;
                self.out.push_str(text);
            }
        }
    }

    fn emit_comment(&mut self, child: &Child) {
        if let Child::Comment(i) = child {
            self.out.push_str(&self.arena.comments[*i as usize].text);
        }
    }

    fn emit_leaf(&mut self, idx: u32) {
        let leaf = &self.arena.leaves[idx as usize];
        self.push_token(leaf.key.normal);
        self.out.push(' ');
        self.out.push_str(leaf.op.as_str());
        self.out.push(' ');
        self.emit_value(&leaf.value, leaf.value_pos);
    }

    fn emit_leaf_value(&mut self, idx: u32) {
        let lv = &self.arena.leaf_values[idx as usize];
        self.emit_value(&lv.value, lv.pos);
    }

    fn emit_value(&mut self, value: &Value, pos: SourceRange) {
        match value {
            Value::Clause(children) => self.emit_clause(children, pos),
            _ => {
                if !self.emit_from_range(pos) {
                    self.emit_value_fallback(value);
                }
            }
        }
    }

    fn emit_clause(&mut self, children: &[Child], pos: SourceRange) {
        if let Some(prefix) = self.clause_prefix(pos) {
            self.out.push_str(&prefix);
            if !prefix.ends_with(' ') {
                self.out.push(' ');
            }
        }
        self.out.push('{');
        if children.is_empty() {
            self.out.push_str(" }");
            return;
        }
        if self.can_inline(children) {
            self.out.push(' ');
            for (i, child) in children.iter().enumerate() {
                if i > 0 {
                    self.out.push(' ');
                }
                self.emit_child(child);
            }
            self.out.push_str(" }");
            return;
        }
        self.push_newline();
        self.indent += 1;
        self.emit_children(children);
        self.indent -= 1;
        self.push_newline();
        self.write_indent();
        self.out.push('}');
    }

    fn clause_prefix(&self, pos: SourceRange) -> Option<String> {
        let open = find_open_brace(self.input, pos, &self.line_starts)?;
        let start = pos_to_byte(self.input, &self.line_starts, pos.start);
        if start >= open {
            return None;
        }
        let prefix = self.input[start..open].trim();
        if prefix.is_empty() {
            None
        } else {
            Some(prefix.to_string())
        }
    }

    fn can_inline(&self, children: &[Child]) -> bool {
        if children.len() > 4 {
            return false;
        }
        children.iter().all(|c| match c {
            Child::LeafValue(i) => {
                !matches!(self.arena.leaf_values[*i as usize].value, Value::Clause(_))
            }
            _ => false,
        })
    }

    fn emit_from_range(&mut self, range: SourceRange) -> bool {
        let s = pos_to_byte(self.input, &self.line_starts, range.start);
        let e = pos_to_byte(self.input, &self.line_starts, range.end).min(self.input.len());
        let s = s.min(e);
        let slice = self.input[s..e].trim();
        if slice.is_empty() {
            return false;
        }
        self.out.push_str(slice);
        true
    }

    fn emit_value_fallback(&mut self, value: &Value) {
        match value {
            Value::String(t) | Value::QString(t) => self.push_token(t.normal),
            Value::Int(i) => self.out.push_str(&i.to_string()),
            Value::Float(f) => self.out.push_str(&f.to_string()),
            Value::Bool(true) => self.out.push_str("yes"),
            Value::Bool(false) => self.out.push_str("no"),
            Value::Clause(_) => self.out.push_str("{ }"),
        }
    }

    fn push_token(&mut self, id: cwtools_string_table::string_table::StringId) {
        self.table.with_string(id, |s| self.out.push_str(s));
    }

    fn write_indent(&mut self) {
        for _ in 0..self.indent {
            self.out.push_str(&self.unit);
        }
    }

    fn push_newline(&mut self) {
        if self.opts.trim_trailing_whitespace {
            while self.out.ends_with(' ') || self.out.ends_with('\t') {
                self.out.pop();
            }
        }
        self.out.push_str(self.newline);
    }

    fn is_eol_comment(&self, children: &[Child], i: usize) -> bool {
        if i == 0 {
            return false;
        }
        let Child::Comment(_) = children[i] else {
            return false;
        };
        let prev = child_span(self.arena, &children[i - 1]);
        let cur = child_span(self.arena, &children[i]);
        prev.end.line == cur.start.line
    }

    fn had_blank_line(&self, prev: &Child, next: &Child) -> bool {
        let prev = child_span(self.arena, prev);
        let next = child_span(self.arena, next);
        next.start.line > prev.end.line.saturating_add(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Child;

    fn table() -> StringTable {
        StringTable::new()
    }

    fn fmt(input: &str) -> String {
        format_text(input, &table(), &FormatOptions::default()).expect("parse")
    }

    fn fmt_opts(input: &str, opts: FormatOptions) -> String {
        format_text(input, &table(), &opts).expect("parse")
    }

    fn dump(input: &str) -> String {
        let table = table();
        let parsed = parse_string(input, &table);
        dump_children(&parsed.arena, &parsed.root_children, &table)
    }

    fn dump_children(arena: &Arena, children: &[Child], table: &StringTable) -> String {
        children
            .iter()
            .map(|c| dump_child(arena, c, table))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn dump_child(arena: &Arena, child: &Child, table: &StringTable) -> String {
        match child {
            Child::Comment(i) => format!("C:{}", arena.comments[*i as usize].text),
            Child::LeafValue(i) => {
                format!(
                    "V:{}",
                    dump_value(arena, &arena.leaf_values[*i as usize].value, table)
                )
            }
            Child::Leaf(i) => {
                let leaf = &arena.leaves[*i as usize];
                let key = table.get_string(leaf.key.normal).unwrap_or_default();
                format!(
                    "L:{key}{}{}",
                    leaf.op.as_str(),
                    dump_value(arena, &leaf.value, table)
                )
            }
        }
    }

    fn dump_value(arena: &Arena, value: &Value, table: &StringTable) -> String {
        match value {
            Value::Clause(ch) => format!("{{{}}}", dump_children(arena, ch, table)),
            Value::String(t) | Value::QString(t) => table.get_string(t.normal).unwrap_or_default(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Bool(true) => "yes".into(),
            Value::Bool(false) => "no".into(),
        }
    }

    #[test]
    fn badly_indented_file_normalizes_and_keeps_ast() {
        let src = "root={\n a=1\n\tb = {\n x=2\n}\n}\n";
        let out = fmt(src);
        assert_eq!(
            out,
            "root = {\n    a = 1\n    b = {\n        x = 2\n    }\n}\n"
        );
        assert_eq!(dump(src), dump(&out));
    }

    #[test]
    fn format_twice_is_idempotent() {
        let once = fmt("foo={\nbar=1\n}\n");
        let twice = fmt(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn tabs_vs_spaces() {
        let src = "a = {\n b = 1\n}\n";
        let spaces = fmt_opts(src, FormatOptions::default());
        let tabs = fmt_opts(
            src,
            FormatOptions {
                indent_style: IndentStyle::Tab,
                ..FormatOptions::default()
            },
        );
        assert!(spaces.contains("    b = 1"));
        assert!(tabs.contains("\tb = 1"));
        assert_ne!(spaces, tabs);
    }

    #[test]
    fn indent_size() {
        let src = "a = {\n b = 1\n}\n";
        let two = fmt_opts(
            src,
            FormatOptions {
                indent_size: 2,
                ..FormatOptions::default()
            },
        );
        assert!(two.contains("  b = 1"));
        assert!(!two.contains("    b = 1"));
    }

    #[test]
    fn trim_trailing_whitespace_clears_blank_line_indent() {
        let src = "a = {\n    x = 1\n\n    y = 2\n}\n";
        let trimmed = fmt_opts(src, FormatOptions::default());
        let kept = fmt_opts(
            src,
            FormatOptions {
                trim_trailing_whitespace: false,
                ..FormatOptions::default()
            },
        );
        assert!(!trimmed.contains("    \n"));
        assert!(kept.contains("    \n"));
    }

    #[test]
    fn insert_final_newline() {
        let src = "foo = 1";
        let with = fmt_opts(src, FormatOptions::default());
        let without = fmt_opts(
            src,
            FormatOptions {
                insert_final_newline: false,
                ..FormatOptions::default()
            },
        );
        assert_eq!(with, "foo = 1\n");
        assert_eq!(without, "foo = 1");
    }

    #[test]
    fn parse_error_returns_none() {
        let table = table();
        assert!(format_text("foo = {", &table, &FormatOptions::default()).is_none());
        assert!(format_edits("foo = {", &table, &FormatOptions::default()).is_empty());
    }

    #[test]
    fn comments_stay_in_sibling_order() {
        let src = "# head\nfoo = 1 # eol\nbar = 2\n";
        let out = fmt(src);
        assert!(out.contains("# head"));
        assert!(out.contains("foo = 1 # eol"));
        assert_eq!(dump(src), dump(&out));
    }

    #[test]
    fn rgb_prefix_is_kept() {
        let src = "color = rgb { 255 0 0 }\n";
        let out = fmt(src);
        assert!(out.contains("rgb { 255 0 0 }"), "{out}");
        assert_eq!(dump(src), dump(&out));
    }

    #[test]
    fn already_formatted_yields_no_edits() {
        let table = table();
        let src = "foo = 1\n";
        assert!(format_edits(src, &table, &FormatOptions::default()).is_empty());
    }

    #[test]
    fn range_format_touches_only_the_selected_statement() {
        let table = table();
        let src = "foo=1\nbar=2\n";
        let range = SourceRange {
            start: SourcePos { line: 2, col: 0 },
            end: SourcePos { line: 2, col: 5 },
        };
        let edits = format_range_edits(src, &table, &FormatOptions::default(), range);
        assert_eq!(edits.len(), 1);
        let out = crate::fix::apply_edits(src, &edits);
        assert!(out.starts_with("foo=1\n"));
        assert!(out.contains("bar = 2"));
    }

    #[test]
    fn editor_options_override_indent() {
        let opts = FormatOptions::default().with_editor(2, true);
        assert_eq!(opts.indent_size, 2);
        assert_eq!(opts.indent_style, IndentStyle::Space);
        let tabs = FormatOptions::default().with_editor(4, false);
        assert_eq!(tabs.indent_style, IndentStyle::Tab);
    }

    #[test]
    fn empty_file_gets_a_final_newline() {
        assert_eq!(fmt(""), "\n");
        let without = fmt_opts(
            "",
            FormatOptions {
                insert_final_newline: false,
                ..FormatOptions::default()
            },
        );
        assert_eq!(without, "");
    }

    #[test]
    fn empty_clause_is_padded() {
        assert_eq!(fmt("foo={}\n"), "foo = { }\n");
        assert_eq!(fmt("foo={ }\n"), "foo = { }\n");
    }

    #[test]
    fn shorthand_clause_gets_an_equals() {
        assert_eq!(fmt("foo { a = 1 }\n"), "foo = {\n    a = 1\n}\n");
    }

    #[test]
    fn comparison_and_question_equal_keep_their_operator() {
        assert_eq!(fmt("a >= 1\n"), "a >= 1\n");
        assert_eq!(fmt("a ?= b\n"), "a ?= b\n");
    }

    #[test]
    fn glued_greater_equal_is_tokenized_as_key_then_equals() {
        assert_eq!(fmt("a>=1\n"), "a> = 1\n");
    }

    #[test]
    fn quoted_key_keeps_its_quotes() {
        assert_eq!(fmt("\"a b\"=1\n"), "\"a b\" = 1\n");
    }

    #[test]
    fn crlf_stays_crlf_and_is_idempotent() {
        let src = "a={\r\n b=1\r\n}\r\n";
        let out = fmt(src);
        assert!(out.contains("\r\n"), "{out:?}");
        assert!(!out.replace("\r\n", "").contains('\n'), "{out:?}");
        assert_eq!(fmt(&out), out);
    }

    #[test]
    fn bom_is_kept_on_a_whole_file_format() {
        let mangled = "\u{FEFF}foo=1\n";
        let out = fmt(mangled);
        assert!(out.starts_with('\u{FEFF}'), "{out:?}");
        assert_eq!(out, "\u{FEFF}foo = 1\n");
        let table = table();
        assert!(format_edits(&out, &table, &FormatOptions::default()).is_empty());
    }

    #[test]
    fn hsv360_prefix_is_kept() {
        let src = "color = hsv360 { 0 100 50 }\n";
        let out = fmt(src);
        assert!(out.contains("hsv360 { 0 100 50 }"), "{out}");
        assert_eq!(dump(src), dump(&out));
    }

    #[test]
    fn range_format_inside_a_clause_leaves_siblings() {
        let table = table();
        let src = "root = {\nfoo=1\nbar=2\n}\n";
        let range = SourceRange {
            start: SourcePos { line: 3, col: 0 },
            end: SourcePos { line: 3, col: 5 },
        };
        let edits = format_range_edits(src, &table, &FormatOptions::default(), range);
        let out = crate::fix::apply_edits(src, &edits);
        assert!(
            out.contains("foo=1"),
            "sibling must stay unformatted: {out}"
        );
        assert!(out.contains("bar = 2"), "{out}");
        assert!(!out.contains("foo = 1"), "{out}");
    }

    #[test]
    fn range_format_keeps_a_leading_bom() {
        let table = table();
        let src = "\u{FEFF}foo=1\nbar=2\n";
        let range = SourceRange {
            start: SourcePos { line: 2, col: 0 },
            end: SourcePos { line: 2, col: 5 },
        };
        let edits = format_range_edits(src, &table, &FormatOptions::default(), range);
        let out = crate::fix::apply_edits(src, &edits);
        assert!(out.starts_with('\u{FEFF}'), "bom dropped: {out:?}");
        assert!(out.contains("foo=1"), "{out}");
        assert!(out.contains("bar = 2"), "{out}");
    }

    #[test]
    fn range_format_of_the_first_statement_keeps_a_leading_bom() {
        let table = table();
        let src = "\u{FEFF}foo=1\n";
        let range = SourceRange {
            start: SourcePos { line: 1, col: 0 },
            end: SourcePos { line: 1, col: 5 },
        };
        let edits = format_range_edits(src, &table, &FormatOptions::default(), range);
        let out = crate::fix::apply_edits(src, &edits);
        assert!(out.starts_with('\u{FEFF}'), "bom dropped: {out:?}");
        assert_eq!(out, "\u{FEFF}foo = 1\n");
    }

    #[test]
    fn format_edits_is_idempotent_after_apply() {
        let table = table();
        let src = "foo={\nbar=1\n}\n";
        let edits = format_edits(src, &table, &FormatOptions::default());
        let once = crate::fix::apply_edits(src, &edits);
        assert!(format_edits(&once, &table, &FormatOptions::default()).is_empty());
    }
}
