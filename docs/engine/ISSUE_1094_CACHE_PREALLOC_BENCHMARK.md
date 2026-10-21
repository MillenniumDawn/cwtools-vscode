# #1094 cache decode preallocation: no-go

## Result

No production preallocation change is retained for #1094. The median of the
four per-run Criterion mean estimates was 848.05 ms for the candidate versus
852.70 ms for the baseline (0.55% faster), but the paired intervals overlap
and the difference is smaller than variation between runs. Peak RSS did not
improve. The result does not demonstrate a repeatable benefit on the native
warm-load input, so eager allocation is not justified. The cache format and
production decoder remain unchanged; no format bump is needed. The allocation
regression test is retained to ensure an over-cap declared frame is rejected
before a large output allocation.

## Input and environment

`warm_load` used the existing Millennium Dawn checkout at `/workspace/Millennium-Dawn`,
revision `94712ee02bde65b3144c8d14219ae05cd1c05d56`. The bench selected 6,505
`.txt` files under `common`, `events` and `history`, totaling 98,476,490 source
bytes (93.91 MiB). A setup run counted the generated compressed `.cwb` entries:
6,505 files, 67.744 MiB
total; minimum 48 bytes, median 1,710 bytes, p90 17,497 bytes, p95 38,806
bytes, maximum 863,993 bytes (0.824 MiB).

The benchmark archives were produced through `io::serialize_to_file`. Its
streaming zstd encoder does not call `set_pledged_src_size`, so these native
`warm_load` frames have no declared content size and do not exercise the
declared-size preallocation path proposed in #1094. The timing result only
addresses the current writer's undeclared-size archives; it does not establish
performance for archives with a declared size.

Runs were on Debian GNU/Linux 13, x86_64, kernel `6.18.55`, AMD EPYC 9V45
(five CPUs available), with 17 GiB RAM and no swap. The toolchain was Rust
1.99.1 / Cargo 1.99.1. Build artifacts, Criterion results and saved benchmark
binaries were kept under `/tmp/cwtools-issue1094-*`; the shared `/workspace`
filesystem was full during this work. `/usr/bin/time` was unavailable. Peak
RSS is the maximum sampled `VmHWM` from `/proc/<pid>/status`, sampled every
50 ms; this can miss a process that exits between samples, but the benchmark
processes lasted about 20 seconds and each produced 430–447 RSS polling
observations (not Criterion iterations).

## Interleaved results

The issue branch starts at exact base `b705bd6db4b8d2469433a30d7fe4419043b2effc`.
At measurement time its `HEAD` was `a7d04e73623c1f7d5a05fa831878e4b52fc02144`,
a report-only commit whose production files still matched the base. The baseline
executable SHA-256 was
`5bdb8c9581ae50feaf7ddbf582c0fbbc56f8f50a3cdd191260e04f683f58a97e`. The
preallocation candidate was built from that production base plus the temporary
`io.rs` candidate; its executable SHA-256 was
`9b2d4b0c3ce0a5fbd14bdb0ca3423b7965341c371cea9f8c384e826b76134f73`. Both
executables used the same corpus and Criterion settings. The SHA-256 of the
temporary candidate source diff from the exact base, covering `io.rs` and the
allocation regression in `tests/limits.rs`, was
`09c400c6a5870b856f680500983d197d69ce93d7624030ce4efc8116a5618956`. This was
a worktree-only candidate patch, not a commit. The order was ABBA,
then BAAB. Each run collected 10 Criterion samples after a 2-second warm-up,
with a 12-second measurement target (40 samples per variant total).

| Order | Variant | Criterion mean | 95% Criterion interval for mean | Outliers | Peak VmHWM |
| --- | --- | ---: | ---: | --- | ---: |
| B1 | Baseline | 850.42 ms | 843.14–857.09 ms | none | 117,896 KiB |
| C1 | Candidate | 837.05 ms | 828.37–847.41 ms | 1 high mild | 116,384 KiB |
| C2 | Candidate | 847.34 ms | 838.78–858.65 ms | 1 high severe | 118,348 KiB |
| B2 | Baseline | 842.46 ms | 828.59–857.59 ms | none | 117,432 KiB |
| C3 | Candidate | 862.38 ms | 841.64–888.84 ms | 1 high mild, 1 high severe | 119,028 KiB |
| B3 | Baseline | 877.34 ms | 861.40–894.43 ms | none | 118,976 KiB |
| B4 | Baseline | 854.98 ms | 840.99–870.29 ms | none | 116,628 KiB |
| C4 | Candidate | 848.76 ms | 835.78–864.05 ms | 1 high mild | 119,404 KiB |

Across the four per-run Criterion mean estimates, baseline mean was 856.30 ms,
median 852.70 ms, sample standard deviation 14.95 ms, and range 842.46–877.34
ms. Candidate mean was 848.88 ms, median 848.05 ms, sample standard deviation
10.40 ms, and range 837.05–862.38 ms. The 4.65 ms median-of-means difference
is a 0.55% change, with overlapping intervals in all four adjacent
baseline/candidate comparisons.
For the measured native archives, candidate mean peak VmHWM was 118,291 KiB
versus 117,733 KiB baseline; the ranges overlap. These timing and RSS
observations are for undeclared-size native frames and do not measure the
declared-size preallocation path or its memory impact. No preallocation
optimization is retained for native caches because their writer emits no
declared sizes. Preallocation behavior and memory use for declared-size
external frames remain unmeasured; a permitted frame could reserve up to the
128 MiB decode cap at once, while the existing growing buffer reserves
according to decoded output.

## Reproduction

From `/workspace/cwtools-issue-1094/engine`, the setup/build command was:

```sh
env PATH=/workspace/.cargo/bin:$PATH \
  RUSTUP_HOME=/workspace/.rustup CARGO_HOME=/workspace/.cargo \
  CARGO_TARGET_DIR=/tmp/cwtools-issue1094-target \
  CWTOOLS_CORPUS=/workspace/Millennium-Dawn \
  CRITERION_HOME=/tmp/cwtools-issue1094-criterion-candidate-setup \
  cargo bench --locked -p cwtools_cache --bench cache -- warm_load --noplot \
  --sample-size 10 --warm-up-time 2 --measurement-time 12 --save-baseline setup
```

The final interleaved runs used saved binaries so rebuilding could not evict the
page cache between samples. Each run had a separate Criterion directory and
log:

```sh
/tmp/run-cwtools-issue1094-sample.sh /tmp/cwtools-issue1094-base-bench B1
/tmp/run-cwtools-issue1094-sample.sh /tmp/cwtools-issue1094-candidate-bench C1
/tmp/run-cwtools-issue1094-sample.sh /tmp/cwtools-issue1094-candidate-bench C2
/tmp/run-cwtools-issue1094-sample.sh /tmp/cwtools-issue1094-base-bench B2
/tmp/run-cwtools-issue1094-sample.sh /tmp/cwtools-issue1094-candidate-bench C3
/tmp/run-cwtools-issue1094-sample.sh /tmp/cwtools-issue1094-base-bench B3
/tmp/run-cwtools-issue1094-sample.sh /tmp/cwtools-issue1094-base-bench B4
/tmp/run-cwtools-issue1094-sample.sh /tmp/cwtools-issue1094-candidate-bench C4
```

The script ran each executable with `--bench warm_load --noplot --sample-size
10 --warm-up-time 2 --measurement-time 12 --save-baseline sample`, set
`CRITERION_HOME=/tmp/cwtools-issue1094-criterion-<label>`, and sampled
`VmHWM` every 50 ms. A preliminary base run used only a 5-second measurement
target; Criterion warned that it needed about 8.6 seconds for 10 samples. That
run, the separate archive-size scan setup run, and the initial candidate
build/setup run are excluded from the comparison.
The preliminary Cargo command was:

```sh
env PATH=/workspace/.cargo/bin:$PATH \
  RUSTUP_HOME=/workspace/.rustup CARGO_HOME=/workspace/.cargo \
  CARGO_TARGET_DIR=/tmp/cwtools-issue1094-target \
  CWTOOLS_CORPUS=/workspace/Millennium-Dawn \
  CRITERION_HOME=/tmp/cwtools-issue1094-criterion-base \
  cargo bench --locked -p cwtools_cache --bench cache -- warm_load --noplot \
  --sample-size 10 --warm-up-time 2 --measurement-time 5 \
  --save-baseline initial-base
```

## Validation

Before the preallocation candidate, the new cap-allocation regression passed on
the base implementation. The full cache crate suite also passed against the
candidate: 38 unit, 22 corrupt archive, 4 corrupt sidecar, 2 layout, 14 limits,
and 7 roundtrip tests. After reverting the unsubstantiated production change,
the same full suite passed again on the final test-only source. The final branch
retains only that regression test and this report; no production decoder change
is kept. Final commands:

```sh
env PATH=/workspace/.cargo/bin:$PATH \
  RUSTUP_HOME=/workspace/.rustup CARGO_HOME=/workspace/.cargo \
  CARGO_TARGET_DIR=/tmp/cwtools-issue1094-target \
  cargo test --locked -p cwtools_cache

env PATH=/workspace/.cargo/bin:$PATH \
  RUSTUP_HOME=/workspace/.rustup CARGO_HOME=/workspace/.cargo \
  cargo fmt --all -- --check

git diff --check
```
