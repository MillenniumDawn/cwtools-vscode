# Guard isolation and headless test measurements, 2026-10-18

The guard now gives each validation subprocess an empty cache. Two headless
tests cover edit supersession under contention. Higher LSP integration-test
concurrency reduced elapsed time locally, with higher memory use. CI settings
and the production debounce remain unchanged.

## Source and scope

- Baseline: `bffea3567c534d21c995fa2adb957aea78efef55` (`main`).
- Cache fix: `72ea4536`; test candidate: `980cb79a406bc678a2ec4175904002c5b18ec457`.
- Open PRs checked before editing: #1049, #1050, #1051 and #1052, all AngriestBird
  drafts. Their parser, traversal, collection and warm-cache changes are untouched.
  #1054 remains outside this batch.
- Work is in `/workspace/cwtools-vscode` on
  `test/isolated-guards-edit-supersession`. The existing Millennium-Dawn checkout
  remains at `532b046fd6fb8c19089a52b4a53b2578816a704d` without source changes.

## Correctness coverage

For #1055, `scripts/guard.py` copies the caller environment and overrides
`XDG_CACHE_HOME` and `LOCALAPPDATA` with separate newly created directories under
the guard's temporary work directory. This covers both the current binary and
`--compare` binary. The parser cache fingerprint still lacks build identity;
starting each validation cold prevents that shared fingerprint from hiding drift.

Four pytest cases cover ordinary/comparison mode and inherited/unset cache
settings. They check empty distinct directories, preserved unrelated environment,
an untouched caller-cache sentinel, and cleanup. All four failed on the original
source because validate had no explicit environment; all pass with the fix.

For #868, one Rust test polls a validation until it is pending on occupied permits,
changes the document version, and checks that the stale operation installs no AST
or diagnostics. A second drives real `didOpen`/`didChange` handlers, parking stale
validation at a test-only gate inside the synchronous read-lock region while
another permit is occupied. It checks latest-only diagnostics, AST version, task
cleanup and permit recovery. Twenty repetitions of both tests on two CPUs passed.

The second test observed the aborted old task still running with no available
permits while the synchronous phase was held. Releasing the gate lets it finish,
discard its result and release its permit. This confirms the cancellation limit;
it does not measure real large-file validation cost or queued-writer hover latency.
No cancellation or lock architecture change was made.

## LSP integration-suite measurements (#1025)

Linux x86_64, Intel Xeon Platinum 8573C, four pinned CPUs (`0-3`), four-CPU cgroup
quota, 16 GiB memory cap, Rust 1.99.1, debug test/server builds with locked
dependencies. Each run executes the same 253 headless stdio integration tests and
their synthetic fixtures. The integration binary is byte-identical between
revisions; the server binaries are saved separately and selected explicitly with
`CARGO_BIN_EXE_cwtools-server`. Hashes and exact commands are in the
[raw measurements](measurements/lsp-tests-2026-10-18.json).

One warmup per case was excluded. Three measured repetitions per case ran in
interleaved order: baseline4, candidate4, candidate16, candidate32; the second
round reversed that order. Builds, aggregate tests and guards were stopped during
measurement. The fixtures, server settings and debounce were identical; the
thread-count experiments changed only the libtest thread count.

| Revision / test threads | Wall times, seconds | Median | Maximum sampled summed RSS, MiB | Server-only RSS, MiB |
| --- | --- | --- | --- | --- |
| Baseline / 4 | 21.368, 21.274, 21.322 | 21.322 | 127.4 | 111.9 |
| Candidate / 4 | 21.540, 21.420, 21.012 | 21.420 | 129.2 | 111.4 |
| Candidate / 16 | 7.546, 7.574, 7.380 | 7.546 | 424.9 | 403.3 |
| Candidate / 32 | 6.356, 6.011, 5.747 | 6.011 | 794.9 | 771.1 |

All 12 measured runs and four warmups passed, 253 tests each. Baseline and
candidate at four threads overlap within run variation. Relative to candidate
four-thread median, 16 threads reduced suite wall time about 65%; 32 reduced it
about 72%. These are test-suite concurrency results. No application speedup is
claimed.

RSS was sampled across the test process group, with a separate server subtotal.
The interval is a 20 ms sleep plus `/proc` scan time; brief peaks can be missed.
Summed RSS counts shared pages in each process, so it is not physical memory use.
Wall time includes process launch, sampling and final polling. The sampler's cost
can grow with process count. Cgroup CPU records include the sampler and other
cgroup activity. Measured throttling was zero at four threads, at most 1.8 ms at
16 and at most 98.7 ms at 32. The 32-thread wall-time range was 5.747-6.356 seconds;
three local repetitions establish limited evidence. No OOM events occurred.

The saved environment retains binaries, logs and the measurement harness under
`/workspace/cwtools-evidence/`. Reproduce by building each listed revision with
`cargo test --locked --workspace --no-run`, saving its `cwtools-server` and
`target/debug/deps/lsp-*` executable, and running the commands in the JSON with
the matching `CARGO_BIN_EXE_cwtools-server`. For an ordinary local comparison,
`cargo test -p cwtools_lsp --test lsp -- --test-threads=16` selects just this suite.

## Validation

- Baseline `cargo test --locked --workspace`: 2,605 passed.
- Candidate `cargo test --locked --workspace --all-features --no-fail-fast`:
  2,607 passed, none failed or ignored.
- `cargo fmt --all --check` and
  `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`:
  passed.
- Python: ruff, black, pylint (10.00/10), strict mypy (49 files), and pytest:
  400 passed, one skipped because `xvfb-run` is absent. Focused guard tests: 29 passed.
- Node 26.12.0: `npm run check` passed; `npm test`: 418 passed, two existing skips.
- MD guard: 12,801 diagnostics match a fresh baseline for corpus `532b046fd6`
  and Kaiserreich rules `40a563ccfded2f56d32e83d8def1a20c27fcdb1d`.
  The old guard captured this baseline with an explicitly isolated outer cache;
  both runs used the same release CLI because engine behavior is unchanged.
- Vanilla guard: all five committed diagnostics match. Metadata notes the baseline
  fixture revision differs; fixture content is unchanged. Both actual guard runs
  left the inherited caller-cache directory and sentinel unchanged.
- A real `guard.py vanilla --no-build --compare bffea3567c534d21c995fa2adb957aea78efef55`
  built the old release CLI and matched all five diagnostics. The comparison
  worktree was cleaned up and the inherited caller cache remained untouched.
- Independent GPT-6.1 Sol review found no blocking issues in the cache fix,
  supersession coverage or measurement method. No visible VS Code window launched.

## Remaining work

Validate a targeted 16-thread LSP test job on Windows, macOS and the llvm-cov job
before proposing a CI change. Sixteen threads is the smaller-memory candidate;
the local improvement does not justify a global `RUST_TEST_THREADS` override.
Keep debounce behavior unchanged.

For #868, measure a real large-file validation with a queued writer and concurrent
hover before selecting a cancellation or snapshot change. The deterministic
tests added here cover stale-result safety and recovery, not early cancellation
or a request-latency budget.
