#!/usr/bin/env python3

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent

Metric = dict[str, float | int | None]
FileCoverage = dict[str, Metric]


def _pct(covered: int, total: int) -> float | None:
    if total == 0:
        return None
    return 100.0 * covered / total


def _int(value: str) -> int | None:
    try:
        return int(value)
    except ValueError:
        return None


def _count(metric: Metric, key: str) -> int:
    value = metric[key]
    return value if isinstance(value, int) else 0


def _metric(covered: int, total: int) -> Metric:
    return {
        "covered": covered,
        "total": total,
        "skipped": 0,
        "pct": _pct(covered, total),
    }


def _relpath(sf: str, repo_root: Path, workspace: Path) -> str:
    raw = Path(sf)
    abs_path = raw if raw.is_absolute() else workspace / raw
    try:
        return abs_path.resolve().relative_to(repo_root.resolve()).as_posix()
    except ValueError:
        return raw.as_posix().replace("\\", "/")


def lcov_to_summary(text: str, repo_root: Path, workspace: Path) -> dict[str, Any]:
    files: dict[str, FileCoverage] = {}
    current: str | None = None
    line_hits: list[int] = []
    fn_hits: list[int] = []
    br_hits: list[bool] = []
    lf = lh = fnf = fnh = brf = brh = -1

    def flush() -> None:
        nonlocal current, lf, lh, fnf, fnh, brf, brh
        if current is None:
            return
        lines_total = lf if lf >= 0 else len(line_hits)
        lines_covered = lh if lh >= 0 else sum(1 for n in line_hits if n > 0)
        fn_total = fnf if fnf >= 0 else len(fn_hits)
        fn_covered = fnh if fnh >= 0 else sum(1 for n in fn_hits if n > 0)
        br_total = brf if brf >= 0 else len(br_hits)
        br_covered = brh if brh >= 0 else sum(1 for hit in br_hits if hit)
        if lines_total == 0:
            current = None
            line_hits.clear()
            fn_hits.clear()
            br_hits.clear()
            lf = lh = fnf = fnh = brf = brh = -1
            return
        files[current] = {
            "lines": _metric(lines_covered, lines_total),
            "statements": _metric(lines_covered, lines_total),
            "functions": _metric(fn_covered, fn_total),
            "branches": _metric(br_covered, br_total),
        }
        current = None
        line_hits.clear()
        fn_hits.clear()
        br_hits.clear()
        lf = lh = fnf = fnh = brf = brh = -1

    for raw_line in text.splitlines():
        line = raw_line.strip()
        if line == "end_of_record":
            flush()
            continue
        if ":" not in line:
            continue
        tag, value = line.split(":", 1)
        if tag == "SF":
            flush()
            current = _relpath(value, repo_root, workspace)
        elif tag == "DA":
            count = _int(value.split(",")[1] if "," in value else "0")
            if count is not None:
                line_hits.append(count)
        elif tag == "FNDA":
            count = _int(value.split(",")[0] if "," in value else "0")
            if count is not None:
                fn_hits.append(count)
        elif tag == "BRDA":
            taken = value.rsplit(",", 1)[-1]
            br_hits.append(taken not in {"-", "0"})
        elif tag == "LF":
            parsed = _int(value)
            if parsed is not None:
                lf = parsed
        elif tag == "LH":
            parsed = _int(value)
            if parsed is not None:
                lh = parsed
        elif tag == "FNF":
            parsed = _int(value)
            if parsed is not None:
                fnf = parsed
        elif tag == "FNH":
            parsed = _int(value)
            if parsed is not None:
                fnh = parsed
        elif tag == "BRF":
            parsed = _int(value)
            if parsed is not None:
                brf = parsed
        elif tag == "BRH":
            parsed = _int(value)
            if parsed is not None:
                brh = parsed
    flush()

    totals = {
        "lines": _metric(0, 0),
        "statements": _metric(0, 0),
        "functions": _metric(0, 0),
        "branches": _metric(0, 0),
    }
    if files:
        for name in ("lines", "statements", "functions", "branches"):
            covered = sum(_count(item[name], "covered") for item in files.values())
            total = sum(_count(item[name], "total") for item in files.values())
            totals[name] = _metric(covered, total)

    return {"total": totals, **files}


def write_summary(lcov: Path, dest: Path, repo_root: Path, workspace: Path) -> None:
    summary = lcov_to_summary(lcov.read_text(encoding="utf-8"), repo_root, workspace)
    dest.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")


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
    summary = coverage_dir / "coverage-summary.json"

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
    if lcov.is_file():
        write_summary(lcov, summary, REPO_ROOT, workspace)
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
