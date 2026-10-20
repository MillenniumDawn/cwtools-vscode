# Issue #1025 validation follow-up, 2026-10-20

The 2026-10-18 Linux timings in `TEST_PERFORMANCE_2026-10-18.md` remain local
evidence only. This follow-up ran on Linux x86_64 with Rust 1.99.1 and the
CI-pinned `cargo-llvm-cov` 0.8.7. Its logs and reports are not in the repo.

`cargo test --locked -p cwtools_lsp --test lsp -- --test-threads=16` passed all
256 tests in 6.61 seconds. This is one Linux run. It is not a repeatable timing
comparison and says nothing about Windows or macOS.

`cargo llvm-cov test --locked -p cwtools_lsp --test lsp` passed all 256 tests at
both four and 16 threads. One summary-only pair reported 14,071/16,768 lines
(83.916%) at four threads and 14,072/16,768 (83.922%) at 16. Four line-level LCOV
runs in the order 4, 16, 16, 4 found the two four-thread reports identical, but
the two 16-thread reports differed at five source lines: four in
`navigation/goto.rs` and one in `scan/workspace.rs`. Coverage equivalence is
unproven, and the 16-thread coverage result depends on scheduling.

There is no Windows or macOS result. Automated VS Code host tests require Linux.
The Rust stdio LSP target is separate, but it was not run on either OS.

## Remaining CI validation proposal

Add a temporary, non-gating workflow (or a dispatchable one) on Linux, Windows
and macOS runners. On each OS, build the locked `cwtools_lsp` test target and run:

```sh
cargo test --locked -p cwtools_lsp --test lsp -- --test-threads=16
```

Record the OS, runner image, Rust version, exit status and test count. Treat a
platform launch or build limitation as an explicit result, not a pass.

Separately on Linux, repeat coverage for only the `cwtools_lsp` `lsp` target at
four and 16 test threads, since the local 16-thread result varied by run. From
`engine/`, on the same commit and toolchain:

```sh
cargo llvm-cov clean --profraw-only
cargo llvm-cov test --locked -p cwtools_lsp --test lsp --json --summary-only --output-path target/lsp-coverage-4.json -- --test-threads=4
cargo llvm-cov clean --profraw-only
cargo llvm-cov test --locked -p cwtools_lsp --test lsp --json --summary-only --output-path target/lsp-coverage-16.json -- --test-threads=16
```

Compare exit statuses, test counts, and each report's line totals and
percentages across repeated runs, and keep the reports and logs as CI artifacts.
These commands scope the run to `cwtools_lsp --test lsp` and change no global
test-thread setting. The full-workspace `coverage.py` stays the coverage gate and
is not part of this comparison. Make no global thread override or debounce change
from the local timings alone.
