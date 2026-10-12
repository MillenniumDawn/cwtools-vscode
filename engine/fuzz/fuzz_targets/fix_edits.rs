#![no_main]

use cwtools_parser::ast::{SourcePos, SourceRange};
use cwtools_parser::fix::{SpanEdit, apply_edits, plan_file_edits};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let lines: Vec<usize> = text.lines().map(|line| line.chars().count()).collect();
    let line_count = lines.len().max(1);
    let edits: Vec<SpanEdit> = data
        .chunks(5)
        .enumerate()
        .filter(|(_, chunk)| chunk.len() == 5)
        .map(|(index, chunk)| {
            // Alternate valid in-line ranges with arbitrary positions. This
            // exercises both normal writes and the writer's invalid-range path.
            let (start, end) = if index % 2 == 0 {
                let line = usize::from(chunk[0]) % line_count;
                let width = lines.get(line).copied().unwrap_or_default();
                let start = usize::from(chunk[1]) % (width + 1);
                let end = start + usize::from(chunk[2]) % (width + 1 - start);
                (
                    SourcePos {
                        line: line as u32 + 1,
                        col: start as u16,
                    },
                    SourcePos {
                        line: line as u32 + 1,
                        col: end as u16,
                    },
                )
            } else {
                (
                    SourcePos {
                        line: u32::from(chunk[0]),
                        col: u16::from_le_bytes([chunk[1], chunk[2]]),
                    },
                    SourcePos {
                        line: u32::from(chunk[3]),
                        col: u16::from(chunk[4]),
                    },
                )
            };
            SpanEdit {
                range: SourceRange { start, end },
                replacement: String::from_utf8_lossy(chunk).into_owned(),
            }
        })
        .collect();

    let _ = apply_edits(&text, &edits);
    let planned: Vec<(usize, SpanEdit)> = edits.iter().cloned().enumerate().collect();
    let (kept, _) = plan_file_edits(&text, planned);
    let _ = apply_edits(&text, &kept);
});
