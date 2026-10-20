# Issue #1025 validation follow-up, 2026-10-20

The 2026-10-18 Linux measurements in `TEST_PERFORMANCE_2026-10-18.md` remain
local evidence only. This attempt was on Linux x86_64
(`b81dd60762ee`, kernel `6.18.55`), but the runner has no `rustc`, `cargo`,
`cargo-llvm-cov`, `llvm-cov`, `llvm-profdata` or `rustup`. The targeted command
`cargo test -p cwtools_lsp --test lsp -- --test-threads=16` could not start
(`cargo: command not found`, exit 127). The repository's real coverage entry
point, `COVERAGE_THRESHOLD=94.4 python3 ../scripts/coverage.py` from `engine/`,
exited 1 before invoking Cargo because `cargo-llvm-cov` is unavailable. Exact
attempts are recorded in `/workspace/cwtools-evidence-1025/local-attempts.txt`.
No test or coverage result was produced here.

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
Separately on Linux, compare coverage for only the `cwtools_lsp` `lsp`
integration target at four and 16 test threads. The local runner still has no
Cargo, so its `cargo llvm-cov --help` could not be checked here; these commands
use the upstream CLI's documented `test`, `--package`, `--test`, `--json`, and
`--summary-only` options and should be confirmed against the CI-installed
0.8.7 CLI before execution. Run from `engine/`, on the same commit/toolchain:

```sh
cargo llvm-cov clean --workspace
cargo llvm-cov test --locked -p cwtools_lsp --test lsp --json --summary-only --output-path target/lsp-coverage-4.json -- --test-threads=4
cargo llvm-cov clean --workspace
cargo llvm-cov test --locked -p cwtools_lsp --test lsp --json --summary-only --output-path target/lsp-coverage-16.json -- --test-threads=16
```

Compare exit statuses, test counts, and each JSON report's line totals and
percentages. Preserve both reports and logs as CI artifacts. These commands
scope the test execution and coverage report to `cwtools_lsp --test lsp`;
they do not change global test-thread settings. The repository's full-workspace
`coverage.py` remains the existing coverage gate, not part of this comparison.
This is validation only: make no global thread override or production debounce
change based on the existing local timings alone.
