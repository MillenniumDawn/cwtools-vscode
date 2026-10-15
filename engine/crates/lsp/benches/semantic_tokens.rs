//! Full and narrow-range semantic token work over large scripts (#870).
//!
//! The two synthetic files contain repeated event blocks with the same local
//! range shape but very different document sizes. Their full requests show the
//! per-document cost; their 40-line range requests show whether work outside
//! the requested viewport still scales with the document. Parsing and token
//! table setup happen outside the timed closures, matching the LSP handler's
//! use of its parsed-document cache.
//!
//! The synthetic rules-backed cases parse the pinned Stellaris events.cwt alone;
//! they cover repeated position dispatch and rule matching but are not a
//! representative game rules corpus. The optional real Millennium Dawn case
//! has no ruleset, so it measures source/AST shape only. In cached-index range
//! cases, index construction is reused as it is for open documents; separately
//! named uncached-index cases include the fallback's full-text index cost.
//!
//!   CWTOOLS_CORPUS=/path/to/Millennium-Dawn cargo bench -p cwtools_lsp --bench semantic_tokens
//!   CWTOOLS_SEMANTIC_TOKEN_FILE=/path/to/script.txt cargo bench -p cwtools_lsp --bench semantic_tokens
//!
//! The corpus is also found under `CWTOOLS_PROJECTS`, or as a sibling of this repo.

use std::hint::black_box;
use std::path::PathBuf;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use cwtools_lsp::bench_support::{LineIndex, semantic_tokens_indexed, semantic_tokens_with_rules};
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_test_support::checkout;
use cwtools_validation::Prepared;
use tower_lsp::lsp_types::PositionEncodingKind;

const SMALL_BLOCKS: usize = 2_000;
const LARGE_BLOCKS: usize = 20_000;
const RANGE_LINES: u32 = 40;
const CORPUS_FILE: &str = "common/national_focus/05_usa.txt";
const LOGICAL_PATH: &str = "game/events/semantic_bench.txt";

fn synthetic(blocks: usize) -> String {
    let mut text = String::with_capacity(blocks * 100);
    for i in 0..blocks {
        text.push_str(&format!(
            "event_{i} = {{\n  id = event.{i}\n  hidden = no\n  trigger = {{ always = yes }}\n}}\n"
        ));
    }
    text
}

fn corpus_file() -> Option<PathBuf> {
    if let Ok(file) = std::env::var("CWTOOLS_SEMANTIC_TOKEN_FILE") {
        return Some(PathBuf::from(file));
    }
    let path = checkout("CWTOOLS_CORPUS", "Millennium-Dawn")?.join(CORPUS_FILE);
    path.is_file().then_some(path)
}

fn bench_source(c: &mut Criterion, name: &str, text: &str) {
    let table = StringTable::new();
    let ast = parse_string(text, &table);
    let line_index = LineIndex::new(text);
    assert!(!ast.root_children.is_empty(), "{name}: input parsed empty");
    let range_start = (text.lines().count() as u32 / 2).saturating_sub(RANGE_LINES / 2);
    let span = (range_start, range_start + RANGE_LINES - 1);
    let expected_range = semantic_tokens_indexed(
        &ast,
        &table,
        text,
        &PositionEncodingKind::UTF16,
        Some((span.0 + 1, span.1 + 1)),
        &line_index,
    );
    eprintln!(
        "semantic_tokens/{name}: {} bytes, {} lines, {} range tokens",
        text.len(),
        text.lines().count(),
        expected_range.len()
    );

    let mut group = c.benchmark_group(format!("semantic_tokens/{name}"));
    group.bench_function(BenchmarkId::new("full", text.len()), |b| {
        b.iter(|| {
            black_box(semantic_tokens_indexed(
                black_box(&ast),
                black_box(&table),
                black_box(text),
                &PositionEncodingKind::UTF16,
                None,
                &line_index,
            ))
        })
    });
    group.bench_function(BenchmarkId::new("range_40_lines", text.len()), |b| {
        b.iter(|| {
            black_box(semantic_tokens_indexed(
                black_box(&ast),
                black_box(&table),
                black_box(text),
                &PositionEncodingKind::UTF16,
                Some((span.0 + 1, span.1 + 1)),
                &line_index,
            ))
        })
    });
    group.bench_function(
        BenchmarkId::new("range_40_lines_uncached_index", text.len()),
        |b| {
            b.iter(|| {
                let index = LineIndex::new(black_box(text));
                black_box(semantic_tokens_indexed(
                    black_box(&ast),
                    black_box(&table),
                    black_box(text),
                    &PositionEncodingKind::UTF16,
                    Some((span.0 + 1, span.1 + 1)),
                    &index,
                ))
            })
        },
    );
    group.finish();
}

fn bench_source_with_rules(c: &mut Criterion, name: &str, text: &str) {
    let table = StringTable::new();
    let ast = parse_string(text, &table);
    let line_index = LineIndex::new(text);
    let rules_ast = parse_string(
        &std::fs::read_to_string(cwtools_test_support::stellaris_config().join("events.cwt"))
            .expect("read events.cwt"),
        &table,
    );
    let ruleset = ast_to_ruleset(&rules_ast, &table);
    let prepared = Prepared {
        ruleset: &ruleset,
        table: &table,
        game: None,
        type_index: None,
        modifier_keys: None,
        loc_index: None,
        extra_loc_keys: None,
        inline_scripts: None,
        registry: None,
        scope_checks: false,
        var_checks: false,
    };
    let range_start = (text.lines().count() as u32 / 2).saturating_sub(RANGE_LINES / 2);
    let span = (range_start, range_start + RANGE_LINES - 1);
    let expected_range = semantic_tokens_with_rules(
        &ast,
        &prepared,
        text,
        LOGICAL_PATH,
        &PositionEncodingKind::UTF16,
        Some((span.0 + 1, span.1 + 1)),
        &line_index,
    );
    eprintln!(
        "semantic_tokens/{name}_events_cwt: {} bytes, {} lines, {} range tokens, {} rules, path {LOGICAL_PATH}",
        text.len(),
        text.lines().count(),
        expected_range.len(),
        ruleset.types.len()
    );

    let mut group = c.benchmark_group(format!("semantic_tokens/{name}_events_cwt"));
    group.bench_function(BenchmarkId::new("full", text.len()), |b| {
        b.iter(|| {
            black_box(semantic_tokens_with_rules(
                black_box(&ast),
                black_box(&prepared),
                black_box(text),
                LOGICAL_PATH,
                &PositionEncodingKind::UTF16,
                None,
                &line_index,
            ))
        })
    });
    group.bench_function(BenchmarkId::new("range_40_lines", text.len()), |b| {
        b.iter(|| {
            black_box(semantic_tokens_with_rules(
                black_box(&ast),
                black_box(&prepared),
                black_box(text),
                LOGICAL_PATH,
                &PositionEncodingKind::UTF16,
                Some((span.0 + 1, span.1 + 1)),
                &line_index,
            ))
        })
    });
    group.bench_function(
        BenchmarkId::new("range_40_lines_uncached_index", text.len()),
        |b| {
            b.iter(|| {
                let index = LineIndex::new(black_box(text));
                black_box(semantic_tokens_with_rules(
                    black_box(&ast),
                    black_box(&prepared),
                    black_box(text),
                    LOGICAL_PATH,
                    &PositionEncodingKind::UTF16,
                    Some((span.0 + 1, span.1 + 1)),
                    &index,
                ))
            })
        },
    );
    group.finish();
}

fn bench_semantic_tokens(c: &mut Criterion) {
    let small = synthetic(SMALL_BLOCKS);
    let large = synthetic(LARGE_BLOCKS);
    bench_source(c, "synthetic_2000_blocks", &small);
    bench_source(c, "synthetic_20000_blocks", &large);
    bench_source_with_rules(c, "synthetic_2000_blocks", &small);
    bench_source_with_rules(c, "synthetic_20000_blocks", &large);
    match corpus_file() {
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(text) => bench_source(c, "millennium_dawn_05_usa", &text),
            Err(error) => eprintln!(
                "semantic_tokens corpus skipped ({}): {error}",
                path.display()
            ),
        },
        None => eprintln!(
            "semantic_tokens corpus skipped (set CWTOOLS_CORPUS or CWTOOLS_SEMANTIC_TOKEN_FILE)"
        ),
    }
}

criterion_group!(benches, bench_semantic_tokens);
criterion_main!(benches);
