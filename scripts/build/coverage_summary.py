from __future__ import annotations

import json
import os
import sys
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path

from coverage_metrics import (
    COVERAGE_METRICS,
    HOST_COVERAGE_DROPS,
    HOST_COVERAGE_LABELS,
    HOST_COVERAGE_SCOPES,
    FileCoverage,
    Metric,
)

GREEN = 80
AMBER = 50


@dataclass(frozen=True)
class CoverageSource:
    title: str
    path: str
    html_artifact: str
    drop: tuple[str, ...] = ()
    labels: tuple[str, ...] = ()
    scopes: tuple[str, ...] = ()
    scope_note: str = ""
    max_files: int | None = None
    required_metrics: tuple[str, ...] = COVERAGE_METRICS


@dataclass(frozen=True)
class RenderedCoverageSection:
    overview: str
    details: tuple[str, ...]


SOURCES: tuple[CoverageSource, ...] = (
    CoverageSource(
        title="Rust engine",
        path="engine/target/coverage/coverage-summary.json",
        html_artifact="rust-coverage",
        max_files=20,
        required_metrics=("lines", "statements", "functions"),
    ),
    CoverageSource(
        title="Extension-host client",
        path="coverage/coverage-summary.json",
        html_artifact="coverage-html",
        drop=HOST_COVERAGE_DROPS,
        labels=HOST_COVERAGE_LABELS,
        scopes=HOST_COVERAGE_SCOPES,
        scope_note="Modules measured only by Vitest are excluded.",
    ),
    CoverageSource(
        title="Node unit",
        path="coverage-node/coverage-summary.json",
        html_artifact="coverage-node",
    ),
)


def light(n: float | None) -> str:
    if not isinstance(n, int | float) or isinstance(n, bool):
        return "⚪"
    if n >= GREEN:
        return "🟢"
    if n >= AMBER:
        return "🟡"
    return "🔴"


def _pct(metric: Metric | None) -> float | None:
    if not metric:
        return None
    value = metric.get("pct")
    if isinstance(value, bool) or not isinstance(value, int | float):
        return None
    return value + 0.0


def num(metric: Metric | None) -> str:
    value = _pct(metric)
    return f"{value:.1f}" if value is not None else "-"


def cell(metric: Metric | None) -> str:
    return f"{light(_pct(metric))} {num(metric)}"


def row(name: str, metrics: FileCoverage) -> str:
    return (
        f"| {name} | {cell(metrics.get('lines'))} | {cell(metrics.get('statements'))} "
        f"| {cell(metrics.get('branches'))} | {cell(metrics.get('functions'))} |"
    )


def _count(metric: Metric | None, key: str) -> int:
    if not metric:
        return 0
    value = metric.get(key)
    return value if isinstance(value, int) else 0


def aggregate(
    files: Sequence[str], data: Mapping[str, FileCoverage], metric: str
) -> Metric:
    covered = sum(_count(data[file].get(metric), "covered") for file in files)
    total = sum(_count(data[file].get(metric), "total") for file in files)
    pct = (100 * covered / total) if total else None
    return {"covered": covered, "total": total, "pct": pct}


def is_source_file(key: str, drop: Sequence[str], scopes: Sequence[str]) -> bool:
    normalized = key.replace("\\", "/")
    if key == "total" or "node_modules" in normalized:
        return False
    if any(item in normalized for item in drop):
        return False
    return not scopes or any(f"/{scope}/" in normalized for scope in scopes)


def listed_files(files: Sequence[str], max_files: int | None) -> tuple[list[str], str]:
    if max_files is None or len(files) <= max_files:
        noun = "file" if len(files) == 1 else "files"
        return list(files), f"{len(files)} {noun}"
    noun = "file" if max_files == 1 else "files"
    return (
        list(files[:max_files]),
        f"{max_files} worst-covered {noun} of {len(files)}",
    )


def section_details(source: CoverageSource) -> str | None:
    if not source.labels or not source.scopes:
        return None
    labels = ", ".join(f"`{label}`" for label in source.labels)
    scope = ", ".join(f"`{path}`" for path in source.scopes)
    extra = f" {source.scope_note}" if source.scope_note else ""
    return f"Measured labels: {labels}. Source scope: {scope}.{extra}".strip()


def _rel(cwd: str, file: str) -> str:
    base = Path(cwd)
    target = Path(file)
    if not target.is_absolute():
        target = base / target
    base_parts = base.parts
    target_parts = target.parts
    if target_parts[: len(base_parts)] == base_parts:
        return Path(*target_parts[len(base_parts) :]).as_posix()
    return target.as_posix()


def render_coverage_section(
    source: CoverageSource,
    data: Mapping[str, FileCoverage],
    cwd: str | None = None,
) -> RenderedCoverageSection:
    workdir = os.getcwd() if cwd is None else cwd
    files = sorted(
        (key for key in data if is_source_file(key, source.drop, source.scopes)),
        key=lambda key: _pct(data[key].get("lines")) or 0,
    )
    if not files:
        raise RuntimeError(f"{source.title} coverage contains no source files")

    total: FileCoverage = {
        "lines": aggregate(files, data, "lines"),
        "statements": aggregate(files, data, "statements"),
        "branches": aggregate(files, data, "branches"),
        "functions": aggregate(files, data, "functions"),
    }
    for metric in source.required_metrics:
        if _count(total[metric], "total") == 0:
            raise RuntimeError(f"{source.title} coverage has a zero {metric} total")

    details = section_details(source)
    shown, file_label = listed_files(files, source.max_files)
    lines = [
        "<details>",
        f"<summary><strong>{source.title} details</strong>: {file_label}</summary>",
        "",
    ]
    if details:
        lines.extend([details, ""])
    lines.extend(
        [
            "| File | % Lines | % Stmts | % Branch | % Funcs |",
            "| --- | --- | --- | --- | --- |",
            *(row(_rel(workdir, file), data[file]) for file in shown),
            "",
            f"<sub>Full report is in the `{source.html_artifact}` artifact.</sub>",
            "",
            "</details>",
        ]
    )
    return RenderedCoverageSection(
        overview=row(f"**{source.title}**", total),
        details=tuple(lines),
    )


def render_section(
    source: CoverageSource, cwd: str | None = None
) -> RenderedCoverageSection | None:
    path = Path(source.path)
    if not path.is_file():
        return None
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError(f"could not read {source.path}: {error}") from error
    if not isinstance(raw, dict):
        raise TypeError(f"could not read {source.path}: not an object")
    return render_coverage_section(source, raw, cwd)


def render_coverage_report(sections: Sequence[RenderedCoverageSection]) -> str:
    if not sections:
        return ""
    lines = [
        "## Coverage",
        "",
        "| Suite | % Lines | % Stmts | % Branch | % Funcs |",
        "| --- | --- | --- | --- | --- |",
        *(section.overview for section in sections),
        "",
        (
            "<sub>🟢 ≥80% · 🟡 ≥50% · 🔴 <50% · ⚪ unavailable. "
            "Expand a section for file coverage and the full-report artifact.</sub>"
        ),
        "",
    ]
    for section in sections:
        lines.extend(section.details)
        lines.append("")
    return "\n".join(lines).rstrip()


def render_coverage_summary(
    selected_sources: Iterable[CoverageSource] | None = None,
) -> str:
    sources = SOURCES if selected_sources is None else selected_sources
    sections: list[RenderedCoverageSection] = []
    for source in sources:
        section = render_section(source)
        if section is not None:
            sections.append(section)
    return render_coverage_report(sections)


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    selected = (
        [source for source in SOURCES if "unit" in source.labels]
        if "--host-only" in args
        else list(SOURCES)
    )
    markdown = render_coverage_summary(selected)
    if not markdown:
        raise RuntimeError("no coverage summaries found")
    coverage_dir = Path("coverage")
    coverage_dir.mkdir(parents=True, exist_ok=True)
    (coverage_dir / "summary.md").write_text(f"{markdown}\n", encoding="utf-8")
    step_summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if step_summary:
        with Path(step_summary).open("a", encoding="utf-8") as handle:
            handle.write(f"{markdown}\n")
    sys.stdout.write(f"{markdown}\n")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, TypeError, OSError) as error:
        sys.stderr.write(f"{error}\n")
        raise SystemExit(1) from error
