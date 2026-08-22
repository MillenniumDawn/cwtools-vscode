from __future__ import annotations

from collections.abc import Mapping

HOST_COVERAGE_LABELS = ("unit",)
HOST_COVERAGE_SCOPES = (
    "extension/src/host",
    "extension/src/common",
)
HOST_COVERAGE_DROPS = (
    "extension/src/host/engine.ts",
    "extension/src/host/executable.ts",
    "extension/src/host/games.ts",
    "extension/src/host/rulesManifest.ts",
    "extension/src/host/rulesSetup.ts",
)

COVERAGE_METRICS = ("lines", "statements", "branches", "functions")

Metric = dict[str, float | int | None]
FileCoverage = dict[str, Metric]


def is_record(value: object) -> bool:
    return isinstance(value, dict)


def is_host_source(path: str) -> bool:
    normalized = path.replace("\\", "/")
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


def validate_host_coverage_summary(value: object) -> None:
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
