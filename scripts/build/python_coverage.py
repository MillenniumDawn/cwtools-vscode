from __future__ import annotations

import json
import shutil
import subprocess
import sys
from collections.abc import Mapping
from pathlib import Path
from typing import cast

from coverage import Coverage

from coverage_metrics import FileCoverage, Metric

ROOT = Path(__file__).resolve().parents[2]
# Initial Linux/Python 3.12 baseline: 1999/2357 lines (84.8112%) and
# 665/854 branch exits (77.8689%). See docs/CONTRIBUTING.md.
FLOORS = {"lines": 84.8, "branches": 77.8}


def _mapping(value: object) -> Mapping[str, object]:
    if not isinstance(value, dict):
        raise RuntimeError("Python coverage report contains a non-object record")
    return cast("Mapping[str, object]", value)


def _metric(record: Mapping[str, object], total_key: str, covered_key: str) -> Metric:
    total, covered = record.get(total_key), record.get(covered_key)
    if (
        isinstance(total, bool)
        or not isinstance(total, int)
        or isinstance(covered, bool)
        or not isinstance(covered, int)
        or not 0 <= covered <= total
    ):
        raise RuntimeError(f"Python coverage has invalid {total_key}/{covered_key}")
    return {
        "total": total,
        "covered": covered,
        "pct": 100 * covered / total if total else None,
    }


def summarize(report: object, root: Path) -> dict[str, FileCoverage]:
    raw = _mapping(report)
    if _mapping(raw.get("meta", {})).get("branch_coverage") is not True:
        raise RuntimeError("Python coverage report has no branch measurement")
    files = _mapping(raw.get("files", {}))
    expected = {
        path.relative_to(root).as_posix() for path in (root / "scripts").rglob("*.py")
    }
    if not expected or set(files) != expected:
        raise RuntimeError(
            "Python coverage inventory differs from production scripts: "
            f"missing={sorted(expected - set(files))}, "
            f"unexpected={sorted(set(files) - expected)}"
        )
    result: dict[str, FileCoverage] = {}
    for path, value in files.items():
        counts = _mapping(_mapping(value).get("summary"))
        lines = _metric(counts, "num_statements", "covered_lines")
        result[path] = {
            "lines": lines,
            "statements": lines,
            "branches": _metric(counts, "num_branches", "covered_branches"),
        }
    return result


def enforce_floors(
    summary: Mapping[str, FileCoverage], floors: Mapping[str, float]
) -> None:
    for name, floor in floors.items():
        metrics = [file[name] for file in summary.values()]
        covered = sum((metric["covered"] or 0) for metric in metrics)
        total = sum((metric["total"] or 0) for metric in metrics)
        if not total:
            raise RuntimeError(f"Python coverage has a zero {name} total")
        percent = 100 * covered / total
        print(f"Python {name}: {covered}/{total} = {percent:.4f}% (floor {floor}%)")
        if percent < floor:
            raise RuntimeError(
                f"Python {name} coverage {percent:.4f}% is below {floor}%"
            )


def main() -> int:
    output = ROOT / "coverage-python"
    # A failed test run, missing data or failed floor must never leave a
    # previous successful summary available to the shared report collector.
    shutil.rmtree(output, ignore_errors=True)
    output.mkdir()
    data_file = output / ".coverage"
    subprocess.run(
        [
            sys.executable,
            "-m",
            "coverage",
            "run",
            f"--rcfile={ROOT / 'pyproject.toml'}",
            f"--data-file={data_file}",
            "-m",
            "pytest",
        ],
        cwd=ROOT,
        check=True,
    )
    if not data_file.is_file():
        raise RuntimeError("Python test collection did not create coverage data")
    measured = Coverage(config_file=str(ROOT / "pyproject.toml"), data_file=data_file)
    measured.load()
    raw_path = output / "coverage.json"
    measured.json_report(outfile=str(raw_path))
    measured.html_report(directory=str(output / "html"))
    summary = summarize(json.loads(raw_path.read_text(encoding="utf-8")), ROOT)
    enforce_floors(summary, FLOORS)
    (output / "coverage-summary.json").write_text(
        json.dumps(summary, indent=2) + "\n", encoding="utf-8"
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, OSError, subprocess.CalledProcessError) as error:
        sys.stderr.write(f"{error}\n")
        raise SystemExit(1) from error
