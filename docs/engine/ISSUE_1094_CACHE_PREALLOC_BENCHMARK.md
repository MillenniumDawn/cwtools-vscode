# #1094 cache decode preallocation: no-go

## Result

No production preallocation change is retained for #1094. A follow-up used
valid declared-size frames derived from the same corpus; the candidate's
median-of-four-run means were 355.36 ms versus 356.11 ms baseline (0.21%
faster). Three of four adjacent 95% intervals overlap, the baseline has one
high run, and the undeclared-size control was 0.86% slower with the candidate.
These results do not demonstrate a repeatable meaningful benefit. The native
writer also emits undeclared-size frames, which cannot use this optimization.
The cache format and production decoder remain unchanged; no format bump is
needed. The over-cap allocation regression and corpus-derived benchmark harness
are retained.

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
processes in the initial native run lasted about 20 seconds and each produced
430–447 RSS polling observations (not Criterion iterations). Follow-up process
counts are recorded separately below.

## Initial native parse-cache `warm_load` results

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

## Declared-size fixture follow-up

To exercise the proposed branch, the same 6,505 corpus inputs were parsed and
stored as native `.cwb` archives. For each native archive, setup decoded its
archive body and recompressed the identical decoded rkyv bytes with
`set_pledged_src_size(Some(decoded_len))`, preserving the five-byte cache
header. Setup asserted that every native frame reported no content size and
every declared twin reported its decoded length. Both sets were then validated
through `io::with_archived_file` before timing. Total decoded archive bodies
were 323,518,572 bytes; native archives totaled 71,034,755 bytes and declared
twins 71,349,551 bytes. Generated fixtures are only in
`/tmp/cwtools-issue1094-fixtures-r2`; the Millennium Dawn checkout was not
modified.

The candidate's `read_archive_bytes` branch allocated `AlignedVec` capacity
equal to a valid declared size after filtering out values over the 128 MiB cap.
The outside-the-loop candidate allocation test selected the largest declared
corpus fixture and observed exactly one allocation request equal to its frame's
declared size. The declared and undeclared fixture metadata checks and archive
validation ran for all 6,505 entries in each benchmark process.

The measurement baseline source was issue branch head
`d9bfeecf5e62e0fe6e71bfdea2234fb97702655a` plus the temporary benchmark
harness. The baseline executable SHA-256 is
`92608304b0d036166c31aaa8ce37f41fa72777d4dd1cd0184ac8c57ae4c69452`; the
candidate executable SHA-256 is
`3f50cea0b68ac109d39fb2f035fcd6ff649ffebb2e899cf9c6223d713532e12b`. The
baseline harness source diff SHA-256 was
`e754ec97d3ce6c8a041b93a0cc531c76b073a050d968c7dcf0547f2859595ea9a`; the
temporary candidate source diff, including `io.rs` preallocation and the same
bench harness, was
`ba8fda4f8214a5b15716d690da3a166d33441b055f5d5bc13fb7e8eab18d834b`. Both
diff hashes are from exact base `b705bd6db4b8d2469433a30d7fe4419043b2effc`;
the candidate patch was worktree-only and is not retained.

Runs used saved binaries and isolated Criterion result directories in the
sequence ABBA then BAAB (`B` = baseline, `C` = candidate). Each run used 10
Criterion samples per workload, 2 seconds warm-up and a 12-second measurement
target (40 samples per variant and workload). Criterion warned that 12 seconds
was below its estimated minimum; it nevertheless collected all 10 samples,
with estimated collection times between 19.2 and 20.7 seconds per workload.
The table reports each run's Criterion mean and its 95% interval. Outliers are
Criterion's classifications.

| Order | Variant | Undeclared mean [95% interval] | Outliers | Declared mean [95% interval] | Outliers | VmHWM |
| --- | --- | ---: | --- | ---: | --- | ---: |
| B1 | Baseline | 350.98 [348.82–352.37] ms | none | 355.21 [354.36–356.34] ms | 1 high mild | 25,008 KiB |
| C1 | Candidate | 354.58 [351.61–356.78] ms | none | 356.59 [355.23–358.23] ms | 1 high severe | 24,432 KiB |
| C2 | Candidate | 354.56 [350.10–358.97] ms | 1 high mild | 354.43 [352.78–357.12] ms | none | 24,084 KiB |
| B2 | Baseline | 355.04 [351.72–357.96] ms | none | 369.89 [365.52–374.18] ms | none | 23,280 KiB |
| C3 | Candidate | 354.40 [351.58–359.34] ms | 1 high severe | 354.67 [354.09–355.44] ms | 1 high mild | 23,228 KiB |
| B3 | Baseline | 350.85 [349.55–352.20] ms | 1 low mild, 1 high mild, 1 high severe | 356.43 [354.64–358.70] ms | 1 high mild | 24,240 KiB |
| B4 | Baseline | 351.91 [350.93–352.85] ms | none | 355.78 [354.58–357.36] ms | none | 25,056 KiB |
| C4 | Candidate | 352.37 [350.48–355.20] ms | none | 356.04 [354.82–357.24] ms | none | 23,548 KiB |

Across the four per-run means, undeclared baseline mean/median/sample SD/range
were 352.20/351.45/1.95 ms/350.85–355.04 ms; candidate values were
353.98/354.48/1.07 ms/352.37–354.58 ms. The median-of-means control difference
was +3.03 ms (+0.86%). For declared frames, baseline values were
359.33/356.11/7.06 ms/355.21–369.89 ms; candidate values were
355.43/355.36/1.05 ms/354.43–356.59 ms. The median-of-means difference was
-0.75 ms (-0.21%); three of four adjacent baseline/candidate intervals
overlap. The B2 baseline result is much slower than its other baseline runs,
while the declared candidate run means cluster within about 2.2 ms. This is
not enough evidence to claim a repeatable optimization.

VmHWM was sampled from `/proc/<pid>/status` every 50 ms because `/usr/bin/time`
is unavailable. Each full process produced 860–876 polling observations (not
Criterion iterations). Mean peak VmHWM was 24,396 KiB baseline (sample SD 833,
range 23,280–25,056) and 23,823 KiB candidate (sample SD 538, range
23,228–24,432). The ranges overlap; these process-wide readings include both
frame groups and setup validation, so they do not establish a declared-frame
memory reduction. The candidate allocates each declared output capacity up
front; a valid declared frame can reserve up to the 128 MiB decode cap.

## Declared-size follow-up reproduction

From `/workspace/cwtools-issue-1094/engine`, the baseline bench binary was
built with:

```sh
env PATH=/workspace/.cargo/bin:$PATH \
  RUSTUP_HOME=/workspace/.rustup CARGO_HOME=/workspace/.cargo \
  CARGO_TARGET_DIR=/tmp/cwtools-issue1094-target \
  cargo bench --locked -p cwtools_cache --bench cache --no-run
```

The saved baseline binary generated the fixtures once, outside timed runs:

```sh
env PATH=/workspace/.cargo/bin:$PATH \
  RUSTUP_HOME=/workspace/.rustup CARGO_HOME=/workspace/.cargo \
  CARGO_TARGET_DIR=/tmp/cwtools-issue1094-target \
  CWTOOLS_CORPUS=/workspace/Millennium-Dawn \
  CWTOOLS_ARCHIVE_BENCH_ONLY=1 \
  CWTOOLS_ARCHIVE_BENCH_SETUP=/tmp/cwtools-issue1094-fixtures-r2 \
  /tmp/cwtools-issue1094-archive-base-bench --bench archive_decode --noplot
```

After temporarily applying the `io.rs` preallocation patch, the candidate binary
was built with the same `cargo bench --no-run` command and saved separately.
A temporary candidate-only path/capacity assertion, outside timing, passed
with:

```sh
env PATH=/workspace/.cargo/bin:$PATH \
  RUSTUP_HOME=/workspace/.rustup CARGO_HOME=/workspace/.cargo \
  CARGO_TARGET_DIR=/tmp/cwtools-issue1094-target \
  CWTOOLS_ARCHIVE_BENCH_FIXTURES=/tmp/cwtools-issue1094-fixtures-r2 \
  cargo test --locked -p cwtools_cache --test limits \
  declared_corpus_frame_reserves_its_reported_size -- --exact --nocapture
```

Each timed process ran both groups, with a separate Criterion directory and RSS
log:

```sh
/tmp/run-cwtools-issue1094-archive-sample.sh /tmp/cwtools-issue1094-archive-base-bench B1
/tmp/run-cwtools-issue1094-archive-sample.sh /tmp/cwtools-issue1094-archive-candidate-bench C1
/tmp/run-cwtools-issue1094-archive-sample.sh /tmp/cwtools-issue1094-archive-candidate-bench C2
/tmp/run-cwtools-issue1094-archive-sample.sh /tmp/cwtools-issue1094-archive-base-bench B2
/tmp/run-cwtools-issue1094-archive-sample.sh /tmp/cwtools-issue1094-archive-base-bench B3
/tmp/run-cwtools-issue1094-archive-sample.sh /tmp/cwtools-issue1094-archive-base-bench B4
/tmp/run-cwtools-issue1094-archive-sample.sh /tmp/cwtools-issue1094-archive-candidate-bench C4
```

The runner's exact Criterion invocation was:

```sh
env CWTOOLS_ARCHIVE_BENCH_ONLY=1 \
  CWTOOLS_ARCHIVE_BENCH_FIXTURES=/tmp/cwtools-issue1094-fixtures-r2 \
  CRITERION_HOME=/tmp/cwtools-issue1094-archive-criterion-<label> \
  <saved-binary> --bench archive_decode --noplot --sample-size 10 \
  --warm-up-time 2 --measurement-time 12 --save-baseline sample
```

The runner sampled VmHWM every 50 ms. Logs and RSS polling files remain under
`/tmp/cwtools-issue1094-archive-*`.

## Initial native `warm_load` reproduction

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

The retained over-cap allocation regression passes on the current implementation.
It asserts that a declared-over-cap frame is rejected before reserving its output.
The candidate-only exact-capacity assertion also passed against temporary
preallocation code, then was removed with that code. The full cache crate suite
passed once against the temporary candidate (38 unit, 22 corrupt archive, 4
corrupt sidecar, 2 layout, 15 limits, 7 roundtrip tests) and again after
reverting production preallocation (38 unit, 22 corrupt archive, 4 corrupt
sidecar, 2 layout, 14 limits, 7 roundtrip tests). The retained branch contains
the over-cap regression, corpus-derived declared/undeclared benchmark harness,
and this report; it has no production decoder change. Final commands:

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
