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
Separately on Linux, run the existing Rust coverage entry point twice against
the same commit and toolchain, saving each output before the next run:

```sh
RUST_TEST_THREADS=4 COVERAGE_THRESHOLD=94.4 python3 ../scripts/coverage.py
RUST_TEST_THREADS=16 COVERAGE_THRESHOLD=94.4 python3 ../scripts/coverage.py
```

Compare both exit statuses, test counts, and `engine/target/coverage/coverage-summary.json`
line totals/percentages. `scripts/coverage.py` invokes the repo's real
`cargo llvm-cov --workspace --all-features` path and applies the normal 94.4%
line floor. Keep both results as artifacts. This is validation only: make no
global thread override or production debounce change based on the existing
local timings alone.
