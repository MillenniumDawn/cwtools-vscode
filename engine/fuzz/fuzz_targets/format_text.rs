#![no_main]

use cwtools_parser::ast::{SourcePos, SourceRange};
use cwtools_parser::format::{
    FormatOptions, IndentStyle, format_edits, format_range_edits, format_text,
};
use cwtools_string_table::string_table::StringTable;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    let table = StringTable::new();
    let bytes = text.as_bytes();
    let options = FormatOptions {
        indent_style: if bytes.first().is_some_and(|byte| byte & 1 == 0) {
            IndentStyle::Space
        } else {
            IndentStyle::Tab
        },
        indent_size: bytes.get(1).copied().unwrap_or_default() as u32,
        max_line_width: bytes.get(2).copied().unwrap_or_default() as usize,
        trim_trailing_whitespace: bytes.get(3).is_some_and(|byte| byte & 1 == 0),
        insert_final_newline: bytes.get(4).is_some_and(|byte| byte & 1 == 0),
    };

    let _ = format_text(text, &table, &options);
    let _ = format_edits(text, &table, &options);

    let line_count = text.lines().count().max(1);
    let position = |line_byte: usize, col_byte: usize| SourcePos {
        line: bytes.get(line_byte).copied().unwrap_or_default() as u32 % line_count as u32 + 1,
        col: bytes.get(col_byte).copied().unwrap_or_default() as u16,
    };
    let range = SourceRange {
        start: position(5, 6),
        end: position(7, 8),
    };
    let _ = format_range_edits(text, &table, &options, range);
});
