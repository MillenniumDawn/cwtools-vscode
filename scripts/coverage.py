#!/usr/bin/env python3

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent


def main() -> int:
    if shutil.which("cargo-llvm-cov") is None:
        print(
            "cargo-llvm-cov is required. Install it with: cargo install cargo-llvm-cov",
            file=sys.stderr,
        )
        return 1

    workspace = Path(os.environ.get("CWTOOLS_RS") or (REPO_ROOT / "engine"))
    coverage_dir = workspace / "target" / "coverage"
    coverage_dir.mkdir(parents=True, exist_ok=True)

    ignore = r"(crates/lsp/src/main\.rs|crates/cli/src/main\.rs)"
    threshold = os.environ.get("COVERAGE_THRESHOLD", "85")
    lcov = coverage_dir / "lcov.info"

    first = subprocess.run(
        [
            "cargo",
            "llvm-cov",
            "--workspace",
            "--all-features",
            "--lcov",
            "--output-path",
            str(lcov),
            "--ignore-filename-regex",
            ignore,
            "--fail-under-lines",
            threshold,
        ],
        cwd=workspace,
        check=False,
    )
    if first.returncode != 0:
        return first.returncode

    second = subprocess.run(
        [
            "cargo",
            "llvm-cov",
            "report",
            "--summary-only",
            "--ignore-filename-regex",
            ignore,
        ],
        cwd=workspace,
        check=False,
    )
    return second.returncode


if __name__ == "__main__":
    sys.exit(main())
