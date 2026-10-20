# Issue #868 hover and validation measurement

## Result

The baseline reproduced a long hover wait when an InfoService writer was queued behind validation: about 374 ms for hover and 377 ms for the writer. A temporary candidate that cloned the `Arc<TypeIndex>` snapshot and released the read guard before CPU validation reduced those waits to roughly 0.4–0.6 ms and 0.8–1.0 ms, respectively. Validation time and diagnostic output stayed similar.

The candidate is **not retained**. On the available fixture, it increased peak RSS by 1.2–1.9 MiB. The fixture is only a 409 KB script workspace, so it cannot establish the copy-on-write memory cost for a full Stellaris installation plus mod. The production change is a no-go until that workspace-scale memory tradeoff can be measured.

## Setup and exact revisions

- Source base: `aa82ebc6035f8dc672b23aa5a1ee7ca7314e4c05` (`main`).
- Measured baseline: `fcd184712f6e4cf59f607655ca035144f8602c84` (`issue/868-baseline`).
- Temporary candidate: `c947221699733d23d711f1885962150c66e1d8e2` (`issue/868-hover-writer-latency`, before reverting the production change).
- The candidate implementation commit `8ea2bbea3977d740dd45d6066be8aa63262c867a` was reverted by `2e0edcde433fa8c14cd18bea59e5c5b9ef40db54`; production code matches the source base.
- Input: `engine/testfiles/performancetest2/events/cc_colony_events.txt`, exactly 165,006 bytes.
- Workspace index: the fixture has 105 files, including 49 `.txt` scripts totaling 408,936 bytes. The harness seeds the other 48 scripts under matching temporary `game/<relative-path>` URIs and logical paths; the measured event file is indexed by its first validation to avoid duplicating exported IDs. The fixture is 664 KiB overall.
- Rules: pinned Stellaris config loaded read-only; 116 config warnings. No config repository files were changed.
- Environment: Linux x86_64, kernel `6.18.55`, 5 logical CPUs, Rust `1.99.1`, about 18.4 GiB total RAM. `CARGO_HOME=/workspace/.cargo`, `RUSTUP_HOME=/workspace/.rustup`, `XDG_CACHE_HOME=/workspace/cwtools-cache-868`.

The harness used the real `parse_and_validate` and `hover_impl` paths. Each process had 2 warmups and 10 uncontended validation samples, then 10 contention samples. In each contention sample validation was held after it acquired the InfoService read lock and prepared its state; a writer was started and allowed to reach the write-lock call before a real event-ID hover was started. The gate was then released. The gate is outside the measured request work. The timed hover was required to return `Some`, and every contention diagnostics signature had to match the uncontended signature.

The interleaved order was **B/C/C/B**. Each B or C run is one process and has 10 samples per phase. The measurement logs are in `/workspace/cwtools-evidence-868/workspace-{B1,C1,C2,B2}.log`.

## Measurements

Times are milliseconds, shown as median / p95 / min–max. Each cell has `n=10`; the p95 for ten observations is the maximum. The run-to-run spread is the difference between the two run medians for that case.

| Phase | B1 | C1 | C2 | B2 | Run-to-run median spread |
|---|---:|---:|---:|---:|---:|
| Uncontended validation | 634.9 / 646.8 / 625.2–646.8 | 624.3 / 630.6 / 617.0–630.6 | 617.0 / 636.1 / 612.1–636.1 | 629.2 / 637.4 / 624.6–637.4 | B 5.7; C 7.3 |
| Validation after gate release | 371.1 / 404.8 / 363.8–404.8 | 371.4 / 382.5 / 362.6–382.5 | 360.9 / 384.5 / 358.2–384.5 | 369.8 / 374.3 / 363.9–374.3 | B 1.3; C 10.5 |
| Writer call through completion | 377.4 / 411.0 / 370.3–411.0 | 0.812 / 1.136 / 0.707–1.136 | 1.018 / 5.888 / 0.751–5.888 | 376.2 / 380.8 / 370.6–380.8 | B 1.2; C 0.2 |
| Hover request through result | 374.5 / 408.1 / 367.5–408.1 | 0.435 / 0.534 / 0.385–0.534 | 0.596 / 2.921 / 0.497–2.921 | 373.6 / 378.0 / 367.7–378.0 | B 0.9; C 0.2 |
| All operations complete after release | 371.6 / 405.2 / 364.2–405.2 | 371.8 / 382.8 / 362.9–382.8 | 361.3 / 384.8 / 358.6–384.8 | 370.3 / 374.7 / 364.3–374.7 | B 1.3; C 10.5 |

All four runs reported the same 101-diagnostic signature, `101:d32453a2a481069e`; all timed hover requests resolved the event ID. Candidate validation time was not materially lower. Its clear benefit in this setup was allowing the queued writer and hover to proceed while validation continued.

The `/proc` RSS sampler polled every 20 ms and recorded the largest test-process `VmRSS` observed during each run. Peak RSS was 47,040 KiB (B1), 47,728 KiB (B2), 48,920 KiB (C1), and 48,948 KiB (C2). The observed candidate delta was +1,880 KiB and +1,220 KiB in the paired runs. These are whole-process peaks, not an isolated TypeIndex allocation measurement. Stored matched logs contain median/p95/min/max but not the individual timing samples, so individual-sample standard deviation/variance cannot be recovered without rerunning. The ranges and run-to-run median spreads are reported above.

## Commands

Both variants were compiled with:

```sh
export PATH=/workspace/.cargo/bin:$PATH
export CARGO_HOME=/workspace/.cargo
export RUSTUP_HOME=/workspace/.rustup
export XDG_CACHE_HOME=/workspace/cwtools-cache-868
export CARGO_TARGET_DIR=/workspace/cwtools-target-868 # baseline
cargo test --locked -p cwtools_lsp --lib measure_large_validation_and_queued_writer_hover --no-run
```

For the candidate, `CARGO_TARGET_DIR` was `/workspace/cwtools-target-868-candidate`. The successful B/C runs used the same command shape with the matching test binary:

```sh
export CWTOOLS_868_MODE=baseline # candidate for C1/C2
export XDG_CACHE_HOME=/workspace/cwtools-cache-868
python3 /workspace/cwtools-evidence-868/sample_rss.py \
  /workspace/cwtools-target-868/debug/deps/cwtools_lsp-7806dd4c6159cc5d \
  validate::issue_868_measurement::measure_large_validation_and_queued_writer_hover \
  --ignored --nocapture --test-threads=1
```

For candidate runs, use `CWTOOLS_868_MODE=candidate` and the binary under `/workspace/cwtools-target-868-candidate/debug/deps/`. The sampler source is preserved at `/workspace/cwtools-evidence-868/sample_rss.py`.

## Scope and test status

The ignored test-only measurement harness compiled and passed in every complete B/C run. No production candidate remains. No full game install was available, and no native Windows/macOS, game, or editor testing was performed.
