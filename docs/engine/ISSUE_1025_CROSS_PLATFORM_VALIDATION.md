# Issue #1025 validation follow-up, 2026-10-20

The 2026-10-18 Linux timings in `TEST_PERFORMANCE_2026-10-18.md` remain local
evidence only. On Linux x86_64 (`b81dd60762ee`, kernel `6.18.55`), I installed
Rust 1.99.1 and the CI-pinned `cargo-llvm-cov` 0.8.7 under `/workspace` after
the initial commands found no toolchain. No repository configuration changed.
The initial failed attempts are retained in
`/workspace/cwtools-evidence-1025/local-attempts.txt`.

The targeted command
`cargo test --locked -p cwtools_lsp --test lsp -- --test-threads=16` passed all
256 tests in 6.61 seconds. This is one Linux run; it is not a repeatable timing
comparison and says nothing about Windows or macOS.

For targeted coverage, `cargo llvm-cov test --locked -p cwtools_lsp --test lsp`
passed all 256 tests at both four and 16 threads. One summary-only pair reported
14,071/16,768 lines (83.915792%) at four threads and 14,072/16,768 (83.921756%)
at 16. Four line-level LCOV runs in order 4, 16, 16, 4 found the two four-thread
reports identical, but the two 16-thread reports differed at five source lines:
four in `navigation/goto.rs` and one in `scan/workspace.rs`. One 16-thread run
covered three `navigation/goto.rs` lines that the four-thread reports did not;
those lines were uncovered in the other 16-thread run. All four runs passed the
same 256 tests. Coverage equivalence is therefore unproven, and the 16-thread
coverage result is schedule-sensitive on this Linux host.
Commands, logs, JSON summaries and line-level reports are retained under
`/workspace/cwtools-evidence-1025/`.

This establishes no Windows or macOS result. The repo's contributor guide
currently says automated VS Code host tests require Linux; the Rust stdio LSP
target is separate, but was not attempted on either remote OS. No platform
compatibility or memory claim follows from this runner.

## Remaining CI validation proposal

Add a temporary, non-gating validation workflow (or dispatchable workflow) on
Linux, Windows and macOS runners. On each OS, build the locked `cwtools_lsp`
test target and run exactly:

```sh
cargo test --locked -p cwtools_lsp --test lsp -- --test-threads=16
```

Capture OS, runner image, Rust version, exit status and test count. Treat a
platform launch/build limitation as an explicit result rather than a pass.
Separately on Linux, repeat coverage for only the `cwtools_lsp` `lsp`
integration target at four and 16 test threads, since the local 16-thread result
varied by run. The following syntax was checked with the CI-pinned 0.8.7 CLI.
Run from `engine/`, on the same commit and toolchain:

```sh
cargo llvm-cov clean --profraw-only
cargo llvm-cov test --locked -p cwtools_lsp --test lsp --json --summary-only --output-path target/lsp-coverage-4.json -- --test-threads=4
cargo llvm-cov clean --profraw-only
cargo llvm-cov test --locked -p cwtools_lsp --test lsp --json --summary-only --output-path target/lsp-coverage-16.json -- --test-threads=16
```

Compare exit statuses, test counts, and each JSON report's line totals and
percentages across repeated runs. Preserve reports and logs as CI artifacts.
These commands scope the test execution and coverage report to
`cwtools_lsp --test lsp`; they do not change global test-thread settings. The
repository's full-workspace `coverage.py` remains the existing coverage gate,
not part of this comparison. This is validation only: make no global thread
override or production debounce change based on the existing local timings
alone.
