# Issue #868 hover and validation measurement

## Result

The baseline reproduced a long hover wait when an InfoService writer was queued behind validation: about 374 ms for hover and 377 ms for the writer. A temporary candidate that cloned the `Arc<TypeIndex>` snapshot and released the read guard before CPU validation cut those waits to roughly 0.4–0.6 ms and 0.8–1.0 ms. Validation time and diagnostic output stayed about the same.

The candidate is **not kept**. On this fixture it raised peak RSS by 1.2–1.9 MiB. The fixture is a 409 KB script workspace, so it cannot show the copy-on-write cost for a full Stellaris installation plus mod: a writer that clones the index while validation still holds the old snapshot copies the whole `TypeIndex`. The change stays a no-go until that workspace-scale memory cost is measured.

## Setup

- Source base: `aa82ebc6035f8dc672b23aa5a1ee7ca7314e4c05` (`main`).
- Candidate: commit `8ea2bbea3977` in PR #1091's history, reverted there. The measurement harness, an ignored test in `engine/crates/lsp/src/validate.rs` with markers in `hover_impl` and `index_parsed_file_as`, is at commit `bdb0a9515d7b` in the same history. It was not merged, because a manual perf check belongs in a bench.
- Input: `engine/testfiles/performancetest2/events/cc_colony_events.txt`, 165,006 bytes.
- Workspace index: the fixture's other 48 `.txt` scripts (49 in all, 408,936 bytes), seeded under temporary `game/<relative-path>` URIs. The measured event file is indexed by its first validation so its IDs are not exported twice.
- Rules: the pinned Stellaris config, 116 config warnings.
- Environment: Linux x86_64, kernel 6.18.55, 5 logical CPUs, Rust 1.99.1, about 18.4 GiB RAM.

The harness used the real `parse_and_validate` and `hover_impl` paths. Each process ran 2 warmups and 10 uncontended validation samples, then 10 contention samples. In each contention sample, validation was held after it took the InfoService read lock and prepared its state. A writer was started and allowed to reach the write-lock call, then a real event-ID hover was started, then the hold was released. The hold is outside the measured work. Every timed hover had to return `Some`, and every contention diagnostics signature had to match the uncontended one.

Runs were interleaved B/C/C/B, one process per run, 10 samples per phase.

## Measurements

Times are milliseconds, shown as median / p95 / min–max. Each cell has `n=10`, so p95 is the maximum. The last column is the difference between the two run medians for that variant.

| Phase | B1 | C1 | C2 | B2 | Run-to-run median spread |
|---|---:|---:|---:|---:|---:|
| Uncontended validation | 634.9 / 646.8 / 625.2–646.8 | 624.3 / 630.6 / 617.0–630.6 | 617.0 / 636.1 / 612.1–636.1 | 629.2 / 637.4 / 624.6–637.4 | B 5.7; C 7.3 |
| Validation after release | 371.1 / 404.8 / 363.8–404.8 | 371.4 / 382.5 / 362.6–382.5 | 360.9 / 384.5 / 358.2–384.5 | 369.8 / 374.3 / 363.9–374.3 | B 1.3; C 10.5 |
| Writer call through completion | 377.4 / 411.0 / 370.3–411.0 | 0.812 / 1.136 / 0.707–1.136 | 1.018 / 5.888 / 0.751–5.888 | 376.2 / 380.8 / 370.6–380.8 | B 1.2; C 0.2 |
| Hover request through result | 374.5 / 408.1 / 367.5–408.1 | 0.435 / 0.534 / 0.385–0.534 | 0.596 / 2.921 / 0.497–2.921 | 373.6 / 378.0 / 367.7–378.0 | B 0.9; C 0.2 |
| All operations complete after release | 371.6 / 405.2 / 364.2–405.2 | 371.8 / 382.8 / 362.9–382.8 | 361.3 / 384.8 / 358.6–384.8 | 370.3 / 374.7 / 364.3–374.7 | B 1.3; C 10.5 |

All four runs reported the same 101-diagnostic signature, `101:d32453a2a481069e`, and every timed hover resolved the event ID. Candidate validation was not materially faster. Its benefit here was letting the queued writer and hover proceed while validation continued.

A `/proc` sampler polled every 20 ms and recorded each run's largest test-process `VmRSS`: 47,040 KiB (B1), 47,728 KiB (B2), 48,920 KiB (C1) and 48,948 KiB (C2), so the candidate added 1,880 KiB and 1,220 KiB in the paired runs. These are whole-process peaks, not an isolated `TypeIndex` allocation. Individual timing samples were not kept, so per-sample variance cannot be recovered without a rerun.

No full game install was available, and no Windows, macOS, game or editor testing was done.
