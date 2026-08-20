#!/usr/bin/env bash

set -euo pipefail

: "${COVERAGE_THRESHOLD:=85}"

if ! command -v cargo-llvm-cov >/dev/null; then
	echo "cargo-llvm-cov is required. Install it with: cargo install cargo-llvm-cov" >&2
	exit 1
fi

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(dirname -- "$script_dir")
workspace=${CWTOOLS_RS:-"$repo_root/cwtools-rs"}

cd "$workspace"
mkdir -p target/coverage

# Binary entry points are exercised via the `cwtools-server` child-process
# integration tests (`crates/lsp/tests/lsp_tests.rs`) and are not instrumented
# by `cargo llvm-cov` (the child is not built with `-C instrument-coverage`).
# Counting them as uncovered would make the gate measure "is the child
# instrumented" rather than "are the library paths tested". Only the two
# binaries are excluded here; the remaining integration coverage is via the
# child process and the new pure-helper unit tests count toward the library
# gate.
ignore='(crates/lsp/src/main\.rs|crates/cli/src/main\.rs)'
cargo llvm-cov \
	--workspace \
	--all-features \
	--lcov \
	--output-path target/coverage/lcov.info \
	--ignore-filename-regex "$ignore" \
	--fail-under-lines "$COVERAGE_THRESHOLD"

cargo llvm-cov report --summary-only --ignore-filename-regex "$ignore"
