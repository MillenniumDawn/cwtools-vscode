//! `documentSymbol` over a ~1 MB script file (#471).
//!
//! `build_doc_symbols` converts two positions per keyed clause through the
//! request's `DocLines`, so on a 1 MB focus tree it is tens of thousands of
//! conversions. VS Code asks for this on open and after every edit, which makes
//! it the handler where the per-request line index (#541) and the `DocLines`
//! ASCII fast path (#471) actually show up.
//!
//! Three subjects, so the numbers reconcile rather than hiding each other:
//! `doc_lines/new` is index construction alone, `build_doc_symbols` is the
//! conversion loop over an index built outside the timing, and
//! `document_symbol` is what one request pays for both together.
//!
//! Two synthetic fixtures make the fast path visible. `ascii` is pure ASCII, so
//! `DocLines` takes the whole-document arithmetic branch. `mixed` is the same
//! script with a non-ASCII comment every `MIXED_COMMENT_EVERY` blocks -- enough
//! to clear the document flag and put every line through the per-line check,
//! and roughly the density of the real thing (`events/United States.txt` in the
//! pinned corpus is 1.05 MB with 90 non-ASCII bytes in it). The comments sit
//! between symbol-bearing blocks, so both fixtures produce the same symbol tree
//! and the two timings are directly comparable; the bench asserts that.
//!
//! The corpus case reads a real file and needs a Millennium-Dawn checkout:
//!
//!   CWTOOLS_CORPUS=/path/to/Millennium-Dawn \
//!     cargo bench -p cwtools_lsp --bench document_symbol
//!
//! It is also found under `CWTOOLS_PROJECTS`, or as a sibling of this repo, and
//! `CWTOOLS_DOC_SYMBOL_FILE` points it at any script file instead. When it is
//! missing the case prints why and measures nothing; the fixtures always run.

use std::hint::black_box;
use std::path::PathBuf;

use criterion::{Criterion, criterion_group, criterion_main};
use tower_lsp::lsp_types::{DocumentSymbol, PositionEncodingKind};

use cwtools_lsp::bench_support::{DocLines, build_doc_symbols};
use cwtools_parser::parser::parse_string;
use cwtools_string_table::string_table::StringTable;

/// Fixture size target. #471 asks for "a ~1 MB script file"; the generator
/// appends whole focus trees until it clears this.
const FIXTURE_BYTES: usize = 1_000_000;
/// Focus blocks per generated `focus_tree`.
const FOCUSES_PER_TREE: usize = 40;
/// One non-ASCII comment per this many focus blocks in the `mixed` fixture.
const MIXED_COMMENT_EVERY: usize = 200;
/// Floor the fixtures must clear, so an empty or half-parsed one cannot produce
/// a fast number that means nothing.
const MIN_SYMBOLS: usize = 10_000;

/// The corpus case's file: the largest script file in the pinned mod, and a
/// deeply nested national focus tree -- the exact shape `documentSymbol` walks.
const CORPUS_FILE: &str = "common/national_focus/05_usa.txt";

fn fixture(non_ascii: bool) -> String {
    let mut out = String::with_capacity(FIXTURE_BYTES + 8192);
    let (mut tree, mut block) = (0usize, 0usize);
    while out.len() < FIXTURE_BYTES {
        out.push_str(&format!(
            "focus_tree_{tree} = {{\n\
             \tid = focus_tree_{tree}\n\
             \tcountry = {{ factor = 1 }}\n"
        ));
        for f in 0..FOCUSES_PER_TREE {
            if non_ascii && block % MIXED_COMMENT_EVERY == 0 {
                out.push_str("\t# r\u{e9}vision \u{2014} priorit\u{e9} interne\n");
            }
            out.push_str(&format!(
                "\tfocus = {{\n\
                 \t\tid = FOCUS_{tree}_{f}\n\
                 \t\ticon = GFX_goal_generic_army_doctrines\n\
                 \t\tx = {f}\n\
                 \t\ty = {tree}\n\
                 \t\tcost = 10\n\
                 \t\tavailable = {{ has_country_flag = flag_{tree}_{f} }}\n\
                 \t\tcompletion_reward = {{ add_political_power = 25 }}\n\
                 \t}}\n"
            ));
            block += 1;
        }
        out.push_str("}\n");
        tree += 1;
    }
    out
}

fn symbol_count(syms: &[DocumentSymbol]) -> usize {
    syms.iter()
        .map(|s| 1 + s.children.as_deref().map_or(0, symbol_count))
        .sum()
}

fn projects_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CWTOOLS_PROJECTS") {
        return Some(PathBuf::from(dir));
    }
    // crates/lsp -> repo root is ../../.., siblings sit next to it.
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    repo.join("..").canonicalize().ok()
}

fn corpus_file() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("CWTOOLS_DOC_SYMBOL_FILE") {
        return Some(PathBuf::from(path));
    }
    let root = match std::env::var("CWTOOLS_CORPUS") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => projects_dir()?.join("Millennium-Dawn"),
    };
    let path = root.join(CORPUS_FILE);
    path.is_file().then_some(path)
}

/// Time one source over all three subjects. Returns the symbol count so the
/// caller can cross-check two fixtures against each other.
fn bench_source(c: &mut Criterion, case: &str, source: &str) -> usize {
    let table = StringTable::new();
    let ast = parse_string(source, &table);
    assert!(
        !ast.root_children.is_empty(),
        "{case}: parsed to no root children; there would be nothing to convert"
    );
    let encoding = PositionEncodingKind::UTF16;

    let lines = DocLines::new(source, encoding.clone());
    let symbols = build_doc_symbols(&ast.root_children, &ast.arena, &table, &lines);
    let total = symbol_count(&symbols);
    assert!(
        total >= MIN_SYMBOLS,
        "{case}: only {total} symbols from {} bytes; the fixture is not exercising \
         the conversion loop and any number from it would be meaningless",
        source.len()
    );
    assert!(
        symbols.iter().any(|s| s.children.is_some()),
        "{case}: no nested symbols; the recursive arm of build_doc_symbols is unmeasured"
    );
    eprintln!(
        "document_symbol/{case}: {} bytes, {} lines, {total} symbols, whole-document \
         ascii branch: {}",
        source.len(),
        source.lines().count(),
        source.is_ascii(),
    );

    c.bench_function(&format!("doc_lines/new/{case}"), |b| {
        b.iter(|| black_box(DocLines::new(black_box(source), encoding.clone())))
    });
    c.bench_function(&format!("build_doc_symbols/{case}"), |b| {
        b.iter(|| {
            black_box(build_doc_symbols(
                black_box(&ast.root_children),
                black_box(&ast.arena),
                black_box(&table),
                black_box(&lines),
            ))
        })
    });
    // What a request actually pays: the index is built per request, not reused.
    c.bench_function(&format!("document_symbol/{case}"), |b| {
        b.iter(|| {
            let lines = DocLines::new(black_box(source), encoding.clone());
            black_box(build_doc_symbols(
                black_box(&ast.root_children),
                black_box(&ast.arena),
                black_box(&table),
                black_box(&lines),
            ))
        })
    });
    total
}

fn bench_document_symbol(c: &mut Criterion) {
    let ascii = fixture(false);
    let mixed = fixture(true);
    assert!(
        ascii.is_ascii(),
        "the ascii fixture must take the whole-document fast branch"
    );
    assert!(
        !mixed.is_ascii(),
        "the mixed fixture must clear the document-level ASCII flag, or the two \
         cases measure the same branch"
    );
    assert!(
        ascii.len() >= FIXTURE_BYTES && mixed.len() >= FIXTURE_BYTES,
        "both fixtures must clear {FIXTURE_BYTES} bytes"
    );

    let ascii_symbols = bench_source(c, "ascii", &ascii);
    let mixed_symbols = bench_source(c, "mixed", &mixed);
    assert_eq!(
        ascii_symbols, mixed_symbols,
        "the two fixtures must build the same symbol tree, or their timings are \
         not comparable"
    );

    let Some(path) = corpus_file() else {
        eprintln!(
            "document_symbol: no corpus. Set CWTOOLS_CORPUS to a Millennium-Dawn \
             checkout (or CWTOOLS_DOC_SYMBOL_FILE to any script file) for the \
             {CORPUS_FILE} case"
        );
        return;
    };
    let source = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("document_symbol: could not read {}: {e}", path.display());
            return;
        }
    };
    bench_source(c, "corpus", &source);
}

criterion_group! {
    name = benches;
    // Seven cases over ~1 MB inputs each; 30 samples separates a 10x move and
    // keeps a full run inside a minute.
    config = Criterion::default().sample_size(30);
    targets = bench_document_symbol
}
criterion_main!(benches);
