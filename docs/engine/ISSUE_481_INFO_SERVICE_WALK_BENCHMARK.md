# #481 InfoService collection benchmark

## Result

No production change is kept for #481. A candidate that combined the typed
reference and alias-key traversals completed the direct collection benchmark
within the baseline's run-to-run variation. The candidate unit test also
matched both original collectors' values and source locations, but the measured
change did not justify keeping the extra traversal code.

The residual per-file work is measured by
`cwtools_info::FileCollector::collect` in
[`crates/info/benches/residual_walks.rs`](../../engine/crates/info/benches/residual_walks.rs).
It times the collector directly, after preparing the rules, AST, string table,
and collector. It excludes parsing, type-instance collection, workspace merge,
and the CLI/LSP driver. The input contains a 73,399-byte generated scripted-GUI
file with 256 repeated GUI records. The records include callback clauses,
`set_variable`, typed references, event targets and trigger clauses. The
inline rules exercise type references, variable definitions, a complex enum,
and scripted-GUI names.

The benchmark checks this collected-output signature before timing:
`file_info=0 type_refs=1 variables=256 complex_values=1 scripted_guis=true`.
The `file_info` count is the number of heuristic `FileInfo.type_references`;
`type_refs` is the separate typed-reference index.

## Measurements

Every row below is a Criterion run with 30 samples, 2-second warm-up and
10-second measurement. The interval is Criterion's reported range for that
run; the median is its point estimate. Runs were sequential on one host. The
later B/C/B/C sequence used the same saved baseline and candidate executables
in alternating order.

| Run | Source | Median | Criterion interval |
| --- | --- | ---: | ---: |
| B0 | Base | 601.96 µs | 591.61–612.32 µs |
| C0 | Candidate | 580.35 µs | 578.53–582.22 µs |
| B1 | Base | 578.41 µs | 573.25–584.74 µs |
| B1a | Base | 570.57 µs | 569.47–571.99 µs |
| B2 | Base | 568.41 µs | 566.78–570.87 µs |
| C2 | Candidate | 577.99 µs | 576.74–579.24 µs |
| B3 | Base | 574.84 µs | 571.84–578.35 µs |
| C3 | Candidate | 575.63 µs | 573.31–579.48 µs |

Criterion's direct B/C comparisons marked the later differences as within its
noise threshold: B2→C2 was +1.17% (p<0.05, within noise), and B3→C3 was +0.56%
(p=0.10). The initial B/C pair suggested a small gain, while later pairs were
flat or slower; intervals overlap. The baseline itself varied by about 6%
between the first and warm-cache runs. Together the repeated results do not
show a stable improvement.

For the later B/C/B/C sequence, a helper sampled the benchmark child process's
`/proc/<pid>/status` `VmRSS` every 20 ms. Peaks were B2 15,188 KiB, C2 14,488
KiB, B3 14,908 KiB and C3 15,428 KiB. These are approximate peak RSS values
for the benchmark process only; they show no consistent memory reduction.

## Reproduction and source revisions

Run from `engine/` with Rust 1.99.1 / Cargo 1.99.1 on Linux x86_64 (kernel
6.18.55), AMD EPYC 9V45, five CPUs available:

```sh
PATH=/workspace/.cargo/bin:$PATH \
CARGO_HOME=/workspace/.cargo \
RUSTUP_HOME=/workspace/.rustup \
CARGO_TARGET_DIR=/workspace/cwtools-target-481 \
cargo bench --locked -p cwtools_info --bench residual_walks -- --noplot
```

The timed code is the call to `FileCollector::collect`; Criterion is configured
for 30 samples, 2-second warm-up and 10-second measurement. For RSS, invoke a
saved benchmark binary with `--bench --noplot` and sample its process status
while it runs. The saved stdout, timing logs and sampler are in
`/workspace/cwtools-evidence-481/` in the local environment.

The base engine revision was `aa82ebc6035f8dc672b23aa5a1ee7ca7314e4c05`.
The benchmark-only change was committed as `f3759f75a8db5f4d41a5bbb92dd60261480a794e`.
The measured candidate code was committed as
`7f6d8fb40c83de7b5f87052127256245d127890a`, based on that benchmark commit.
The candidate commit was reverted after the no-go decision; the final branch
contains the benchmark and this report, not the combined collector code.
