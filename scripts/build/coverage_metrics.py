from __future__ import annotations

import argparse
import json
import subprocess
import sys
from collections.abc import Mapping
from pathlib import Path

# Include multi-root's settings safety tests in the shared coverage session.
HOST_COVERAGE_LABELS = ("host", "live", "restart", "multi-root")
HOST_COVERAGE_SCOPES = (
    "extension/src/host",
    "extension/src/common",
)
# Modules vitest.config.mts already covers with real node-side tests (its
# `coverage.include`). Keep this list in sync with that one: the host suite
# loads these transitively without exercising them the way the node tests do,
# so leaving them in would show a misleadingly low number next to the
# accurate one already in the "Node unit" section.
HOST_COVERAGE_DROPS = (
    "extension/src/host/baseGame.ts",
    "extension/src/host/commandProgress.ts",
    "extension/src/host/commands.ts",
    "extension/src/host/detectGame.ts",
    "extension/src/host/diagnosticsSignature.ts",
    "extension/src/host/engine.ts",
    "extension/src/host/executable.ts",
    "extension/src/host/fileExplorer.ts",
    "extension/src/host/fileListSignature.ts",
    "extension/src/host/fnv1a.ts",
    "extension/src/host/focusTracking.ts",
    "extension/src/host/gameContentRoots.ts",
    "extension/src/host/games.ts",
    "extension/src/host/graphAvailability.ts",
    "extension/src/host/reindexSettings.ts",
    "extension/src/host/restartWindow.ts",
    "extension/src/host/rulesManifest.ts",
    "extension/src/host/rulesSetup.ts",
    "extension/src/host/serverBlockedDialog.ts",
    "extension/src/host/setupReportText.ts",
    "extension/src/host/startupPreparation.ts",
    "extension/src/host/trustedPaths.ts",
    "extension/src/host/watchedFiles.ts",
)

COVERAGE_METRICS = ("lines", "statements", "branches", "functions")

# Per-metric floors for the host coverage gate, just under the baseline.
HOST_COVERAGE_THRESHOLDS: dict[str, float] = {
    "lines": 77,
    "statements": 77,
    "branches": 69,
    "functions": 76,
}

Metric = dict[str, float | int | None]
FileCoverage = dict[str, Metric]


def executable_source_ownership() -> tuple[set[str], set[str]]:
    root = Path(__file__).resolve().parents[2]
    result = subprocess.run(
        ["node", str(root / "scripts/build/coverage_inventory.mjs"), str(root)],
        check=True,
        capture_output=True,
        text=True,
    )
    inventory = json.loads(result.stdout)
    return source_ownership(set(inventory["sourceFiles"]), set(inventory["include"]))


def source_ownership(
    sources: set[str], included: set[str]
) -> tuple[set[str], set[str]]:
    host = {
        path
        for path in sources
        if path.startswith(("extension/src/host/", "extension/src/common/"))
        and path not in HOST_COVERAGE_DROPS
    }
    node = sources & included
    unknown_includes = sorted(included - sources)
    if unknown_includes:
        raise RuntimeError(
            "Vitest coverage.include contains non-executable or unknown modules: "
            + ", ".join(unknown_includes)
        )
    if host | node != sources:
        missing = sorted(sources - host - node)
        raise RuntimeError(
            "executable source modules have no coverage owner: " + ", ".join(missing)
        )
    if host & node:
        overlap = sorted(host & node)
        raise RuntimeError(
            "host and Node coverage owners overlap: " + ", ".join(overlap)
        )
    return host, node


def is_host_source(path: str) -> bool:
    normalized = path.replace("\\", "/")
    if any(item in normalized for item in HOST_COVERAGE_DROPS):
        return False
    return any(f"/{scope}/" in normalized for scope in HOST_COVERAGE_SCOPES)


def _as_mapping(value: object) -> Mapping[object, object] | None:
    if isinstance(value, Mapping):
        return value
    return None


def metric_total(file: str, coverage: object, metric: str) -> int:
    record = _as_mapping(coverage)
    metric_value = None if record is None else record.get(metric)
    metric_record = _as_mapping(metric_value)
    if metric_record is None:
        raise RuntimeError(f"coverage for {file} has no {metric} metric")
    total = metric_record.get("total")
    if isinstance(total, bool) or not isinstance(total, int) or total < 0:
        raise RuntimeError(f"coverage for {file} has an invalid {metric} total")
    return total


def metric_covered(file: str, coverage: object, metric: str) -> int:
    record = _as_mapping(coverage)
    metric_value = None if record is None else record.get(metric)
    metric_record = _as_mapping(metric_value)
    if metric_record is None:
        raise RuntimeError(f"coverage for {file} has no {metric} metric")
    covered = metric_record.get("covered")
    if isinstance(covered, bool) or not isinstance(covered, int):
        raise RuntimeError(f"coverage for {file} has an invalid {metric} covered value")
    total = metric_total(file, coverage, metric)
    if covered < 0 or covered > total:
        raise RuntimeError(
            f"coverage for {file} has an out-of-range {metric} covered value"
        )
    return covered


def validate_node_coverage_summary(value: object) -> None:
    record = _as_mapping(value)
    if record is None:
        raise RuntimeError("Node coverage summary is not an object")
    _, expected = executable_source_ownership()
    actual = {str(path).replace("\\", "/") for path in record}
    missing = sorted(
        source
        for source in expected
        if not any(path.endswith("/" + source) for path in actual)
    )
    if missing:
        raise RuntimeError(
            "Node coverage is missing executable Node modules: " + ", ".join(missing)
        )


def validate_host_coverage_summary(value: object) -> None:
    record = _as_mapping(value)
    if record is None:
        raise RuntimeError("host coverage summary is not an object")
    expected, _ = executable_source_ownership()
    actual = {str(path).replace("\\", "/") for path in record}
    missing = sorted(
        source
        for source in expected
        if not any(path.endswith("/" + source) for path in actual)
    )
    if missing:
        raise RuntimeError(
            "host coverage is missing executable host modules: " + ", ".join(missing)
        )
    validate_host_coverage_metrics(value)


def validate_host_coverage_metrics(value: object) -> None:
    record = _as_mapping(value)
    if record is None:
        raise RuntimeError("host coverage summary is not an object")

    files = [
        (str(file), coverage)
        for file, coverage in record.items()
        if is_host_source(str(file))
    ]
    if not files:
        scopes = " or ".join(HOST_COVERAGE_SCOPES)
        raise RuntimeError(f"host coverage contains no files under {scopes}")
    for metric in COVERAGE_METRICS:
        total = sum(metric_total(file, coverage, metric) for file, coverage in files)
        if total == 0:
            raise RuntimeError(f"host coverage has a zero {metric} total")

    _enforce_host_coverage_thresholds(files)


def _enforce_host_coverage_thresholds(
    files: list[tuple[str, object]],
) -> None:
    threshold_map = HOST_COVERAGE_THRESHOLDS
    for metric in COVERAGE_METRICS:
        threshold = threshold_map.get(metric)
        covered = 0
        total = 0
        for file, coverage in files:
            covered += metric_covered(file, coverage, metric)
            total += metric_total(file, coverage, metric)
        if threshold is None or total == 0:
            continue
        pct = 100 * covered / total
        if pct < threshold:
            raise RuntimeError(
                f"host coverage {metric} is {pct:.2f}% which is below the "
                f"{threshold}% floor (lower the floor in "
                f"coverage_metrics.HOST_COVERAGE_THRESHOLDS to relax it)"
            )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Validate Node coverage inventory")
    parser.add_argument("summary", type=Path)
    args = parser.parse_args(argv)
    validate_node_coverage_summary(json.loads(args.summary.read_text(encoding="utf-8")))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        sys.stderr.write(f"{error}\n")
        raise SystemExit(1) from error
