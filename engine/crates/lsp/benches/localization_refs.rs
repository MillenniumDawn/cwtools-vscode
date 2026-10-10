//! Synthetic before/after scan for the raw localization-reference prefilter.
//!
//! Run with `cargo bench -p cwtools_lsp --bench localization_refs -- --bench`.
//! This measures in-memory text processing only: it does not include workspace
//! traversal, authorization, file reads, or URI handling.

use std::hint::black_box;
use std::time::{Duration, Instant};

use cwtools_lsp::bench_support::loc_script_usage_columns;
use tower_lsp::lsp_types::{Position, PositionEncodingKind};

const FILES: usize = 5_000;
const ROUNDS: usize = 11;
const KEY: &str = "target_key";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--list") {
        println!("localization_refs: bench");
        return;
    }
    if args.iter().any(|arg| arg == "--bench") {
        run();
    }
}

fn run() {
    run_profile("small", 15);
    run_profile("larger", 127);
}

fn run_profile(name: &str, filler_lines: usize) {
    let keys = vec![KEY.to_string()];
    let texts: Vec<String> = (0..FILES)
        .map(|index| {
            let mut text = format!("# synthetic script file {index}\n");
            for line in 0..filler_lines {
                text.push_str(&format!(
                    "  unrelated_effect_{line} = {{ value = {index} }}\n"
                ));
            }
            match index % 100 {
                0 => text.push_str("  TARGET_KEY = yes\n"),
                1 => text.push_str("  label = target_key_suffix\n"),
                _ => text.push_str("  plain_value = yes\n"),
            }
            text
        })
        .collect();

    // Verify the complete location set before timing. The prefilter can only
    // reject files lacking the raw key substring; token matching is unchanged.
    let baseline = scan(&texts, &keys, false);
    let filtered = scan(&texts, &keys, true);
    assert_eq!(filtered, baseline, "prefilter changed localization matches");

    // Alternate order to reduce cache and thermal drift between variants.
    let (mut baseline_times, mut filtered_times) = (Vec::new(), Vec::new());
    for round in 0..ROUNDS {
        if round % 2 == 0 {
            baseline_times.push(time_scan(&texts, &keys, false));
            filtered_times.push(time_scan(&texts, &keys, true));
        } else {
            filtered_times.push(time_scan(&texts, &keys, true));
            baseline_times.push(time_scan(&texts, &keys, false));
        }
    }
    println!(
        "{name} dataset: {FILES} in-memory files, {filler_lines} filler lines/file, {} total bytes, 1% true hits plus 1% substring-only candidates; {ROUNDS} interleaved pairs",
        texts.iter().map(String::len).sum::<usize>()
    );
    report("baseline (unfiltered)", &baseline_times);
    report("prefiltered", &filtered_times);
    println!("matching locations: {} (identical)", baseline.len());
    println!("caveat: synthetic CPU-only scan; filesystem reads and authorization are excluded");
}

fn scan(texts: &[String], keys: &[String], prefilter: bool) -> Vec<(usize, Position, Position)> {
    texts
        .iter()
        .enumerate()
        .flat_map(|(file, text)| {
            loc_script_usage_columns(
                black_box(text),
                keys,
                PositionEncodingKind::UTF16,
                prefilter,
            )
            .into_iter()
            .map(move |range| (file, range.start, range.end))
        })
        .collect()
}

fn time_scan(texts: &[String], keys: &[String], prefilter: bool) -> Duration {
    let start = Instant::now();
    black_box(scan(texts, keys, prefilter).len());
    start.elapsed()
}

fn report(name: &str, times: &[Duration]) {
    let mut sorted = times.to_vec();
    sorted.sort_unstable();
    let median = sorted[sorted.len() / 2];
    println!(
        "{name}: median {:.2} ms; range {:.2}–{:.2} ms",
        median.as_secs_f64() * 1000.0,
        sorted[0].as_secs_f64() * 1000.0,
        sorted[sorted.len() - 1].as_secs_f64() * 1000.0
    );
}
