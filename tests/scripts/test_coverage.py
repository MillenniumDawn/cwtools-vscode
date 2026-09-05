from __future__ import annotations

import json
import re
from pathlib import Path

import pytest

from coverage_metrics import (
    COVERAGE_METRICS,
    HOST_COVERAGE_DROPS,
    HOST_COVERAGE_LABELS,
    HOST_COVERAGE_THRESHOLDS,
    FileCoverage,
    Metric,
    validate_host_coverage_summary,
)
from coverage_summary import (
    SOURCES,
    CoverageSource,
    render_coverage_report,
    render_coverage_section,
    render_coverage_summary,
)

METRIC_NAMES = ("lines", "statements", "branches", "functions")

REPO_ROOT = Path(__file__).resolve().parents[2]


def metric(total: int) -> Metric:
    return {"total": total, "covered": total, "pct": 100}


def file_coverage(totals: dict[str, int] | None = None) -> FileCoverage:
    values = totals or dict.fromkeys(METRIC_NAMES, 1)
    return {name: metric(values[name]) for name in METRIC_NAMES}


def weighted(total: int, covered: int) -> FileCoverage:
    pct = 100 * covered / total
    return {
        "lines": {"total": total, "covered": covered, "pct": pct},
        "statements": {"total": total, "covered": covered, "pct": pct},
        "branches": {"total": 1, "covered": 1, "pct": 100},
        "functions": {"total": 1, "covered": 1, "pct": 100},
    }


def test_accepts_non_empty_host_and_common_source_coverage() -> None:
    validate_host_coverage_summary(
        {
            "total": file_coverage(),
            "/repo/extension/src/host/fileExplorer.ts": file_coverage(),
            r"C:\repo\extension\src\common\graphTypes.ts": file_coverage(),
        }
    )


def test_rejects_a_report_containing_only_dependencies() -> None:
    with pytest.raises(
        RuntimeError, match="no files under extension/src/host or extension/src/common"
    ):
        validate_host_coverage_summary(
            {
                "total": file_coverage(),
                "/repo/node_modules/example/index.js": file_coverage(),
            }
        )


@pytest.mark.parametrize("metric_name", METRIC_NAMES)
def test_rejects_a_zero_metric_total(metric_name: str) -> None:
    totals = dict.fromkeys(METRIC_NAMES, 1) | {metric_name: 0}
    with pytest.raises(
        RuntimeError, match=f"host coverage has a zero {metric_name} total"
    ):
        validate_host_coverage_summary(
            {
                "total": file_coverage(),
                "/repo/extension/src/host/fileExplorer.ts": file_coverage(totals),
            }
        )


@pytest.mark.parametrize("metric_name", METRIC_NAMES)
def test_rejects_a_missing_covered_value(metric_name: str) -> None:
    coverage = file_coverage()
    del coverage[metric_name]["covered"]

    with pytest.raises(RuntimeError, match=f"invalid {metric_name} covered value"):
        validate_host_coverage_summary(
            {
                "total": file_coverage(),
                "/repo/extension/src/host/fileExplorer.ts": coverage,
            }
        )


@pytest.mark.parametrize("metric_name", METRIC_NAMES)
@pytest.mark.parametrize("covered", [None, "1", 1.5, True])
def test_rejects_an_invalid_covered_value(metric_name: str, covered: object) -> None:
    with pytest.raises(RuntimeError, match=f"invalid {metric_name} covered value"):
        validate_host_coverage_summary(
            {
                "total": file_coverage(),
                "/repo/extension/src/host/fileExplorer.ts": {
                    **file_coverage(),
                    metric_name: {"total": 1, "covered": covered},
                },
            }
        )


@pytest.mark.parametrize("metric_name", METRIC_NAMES)
@pytest.mark.parametrize("covered", [-1, 2])
def test_rejects_an_out_of_range_covered_value(metric_name: str, covered: int) -> None:
    coverage = file_coverage()
    coverage[metric_name]["covered"] = covered

    with pytest.raises(RuntimeError, match=f"out-of-range {metric_name} covered value"):
        validate_host_coverage_summary(
            {
                "total": file_coverage(),
                "/repo/extension/src/host/fileExplorer.ts": coverage,
            }
        )


def test_renders_the_measured_label_and_source_scope() -> None:
    source = CoverageSource(
        title="Extension-host client",
        path="coverage/coverage-summary.json",
        html_artifact="coverage-html",
        labels=("unit",),
        scopes=("extension/src/host", "extension/src/common"),
        scope_note="Modules measured only by Vitest are excluded.",
    )

    section = render_coverage_section(
        source,
        {
            "total": file_coverage(),
            "/repo/extension/src/host/fileExplorer.ts": file_coverage(),
            "/repo/node_modules/example/index.js": file_coverage(),
        },
        "/repo",
    )

    rendered = "\n".join(section.details)
    assert "Measured labels: `unit`" in rendered
    assert "Source scope: `extension/src/host`, `extension/src/common`" in rendered
    assert "extension/src/host/fileExplorer.ts" in rendered
    assert "node_modules" not in rendered


def test_overview_uses_weighted_filtered_totals() -> None:
    section = render_coverage_section(
        CoverageSource(
            title="Extension-host client",
            path="host.json",
            html_artifact="coverage-html",
            scopes=("extension/src/host",),
        ),
        {
            "/repo/extension/src/host/small.ts": weighted(1, 1),
            "/repo/extension/src/host/large.ts": weighted(9, 0),
            "/repo/node_modules/example/index.js": weighted(90, 90),
        },
        "/repo",
    )

    assert section.overview == (
        "| **Extension-host client** | 🔴 10.0 | 🔴 10.0 | 🟢 100.0 | 🟢 100.0 |"
    )


def test_renders_one_overview_with_collapsed_file_sections() -> None:
    rust = render_coverage_section(
        CoverageSource(
            title="Rust engine", path="rust.json", html_artifact="rust-coverage"
        ),
        {"engine/crates/parser.rs": file_coverage()},
        "/repo",
    )
    node = render_coverage_section(
        CoverageSource(
            title="Node unit", path="node.json", html_artifact="coverage-node"
        ),
        {"extension/src/host/engine.ts": file_coverage()},
        "/repo",
    )

    rendered = render_coverage_report((rust, node))
    overview, details = rendered.split("<details>", 1)

    assert rendered.count("## Coverage") == 1
    assert "| **Rust engine** |" in overview
    assert "| **Node unit** |" in overview
    assert "| File |" not in overview
    assert "engine/crates/parser.rs" in details
    assert "extension/src/host/engine.ts" in details
    assert rendered.count("<details>") == 2
    assert rendered.count("</details>") == 2
    assert "**All files**" not in rendered


def test_summary_skips_missing_sources(tmp_path: Path) -> None:
    summary = tmp_path / "coverage-summary.json"
    summary.write_text(
        json.dumps({"extension/src/host/engine.ts": file_coverage()}), encoding="utf-8"
    )

    rendered = render_coverage_summary(
        (
            CoverageSource(
                title="Missing suite",
                path=str(tmp_path / "missing.json"),
                html_artifact="missing",
            ),
            CoverageSource(
                title="Node unit",
                path=str(summary),
                html_artifact="coverage-node",
            ),
        )
    )

    assert "| **Node unit** |" in rendered
    assert "Missing suite" not in rendered
    assert rendered.count("<details>") == 1


def test_rejects_an_empty_filtered_report() -> None:
    source = CoverageSource(
        title="Extension-host client",
        path="coverage/coverage-summary.json",
        html_artifact="coverage-html",
        labels=("unit",),
        scopes=("extension/src/host", "extension/src/common"),
    )

    with pytest.raises(RuntimeError, match="no source files"):
        render_coverage_section(
            source,
            {
                "total": file_coverage(),
                "/repo/node_modules/example/index.js": file_coverage(),
            },
        )


def test_lists_the_worst_covered_rust_files() -> None:
    def at_pct(pct: int) -> FileCoverage:
        return {
            "lines": {"total": 10, "covered": pct // 10, "pct": pct},
            "statements": {"total": 10, "covered": pct // 10, "pct": pct},
            "branches": {"total": 0, "covered": 0, "pct": None},
            "functions": {"total": 1, "covered": 1, "pct": 100},
        }

    section = render_coverage_section(
        CoverageSource(
            title="Rust engine",
            path="engine/target/coverage/coverage-summary.json",
            html_artifact="rust-coverage",
            max_files=2,
            required_metrics=("lines", "statements", "functions"),
        ),
        {
            "total": file_coverage(),
            "engine/crates/a.rs": at_pct(10),
            "engine/crates/b.rs": at_pct(20),
            "engine/crates/c.rs": at_pct(90),
        },
        "/repo",
    )

    rendered = "\n".join(section.details)
    assert "engine/crates/a.rs" in rendered
    assert "engine/crates/b.rs" in rendered
    assert "engine/crates/c.rs" not in rendered
    assert "2 worst-covered files of 3" in rendered
    assert "Full report is in the `rust-coverage` artifact" in rendered


def test_host_coverage_drops_match_vitest_owned_host_modules() -> None:
    # vitest.config.ts's `coverage.include` is the source of truth for which
    # host modules the node report already covers accurately. HOST_COVERAGE_
    # DROPS has to mirror the host/common-scoped entries in that list, or a
    # vitest-owned module leaks back into the host report with a misleading
    # partial number (the bug behind #220's commandProgress.ts example).
    config = (REPO_ROOT / "vitest.config.ts").read_text(encoding="utf-8")
    # Assumes double-quoted paths and no `]` (even in a comment) inside the
    # include array; a `]` there would silently truncate the captured list.
    match = re.search(r"coverage:\s*\{.*?include:\s*\[(.*?)\]", config, re.DOTALL)
    assert match is not None, "could not find coverage.include in vitest.config.ts"
    included = re.findall(r'"([^"]+)"', match.group(1))
    host_owned = {
        path
        for path in included
        if path.startswith(("extension/src/host/", "extension/src/common/"))
    }
    assert set(HOST_COVERAGE_DROPS) == host_owned


def test_host_report_omits_vitest_owned_modules() -> None:
    host = next(source for source in SOURCES if source.labels)
    assert host.drop == HOST_COVERAGE_DROPS
    section = render_coverage_section(
        host,
        {
            "/repo/extension/src/host/fileExplorer.ts": file_coverage(),
            **{f"/repo/{path}": file_coverage() for path in HOST_COVERAGE_DROPS},
        },
        "/repo",
    )
    rendered = "\n".join(section.details)
    assert "extension/src/host/fileExplorer.ts" in rendered
    for path in HOST_COVERAGE_DROPS:
        assert path not in rendered


def test_host_gate_ignores_vitest_owned_zero_coverage() -> None:
    dropped = {name: {"total": 1000, "covered": 0, "pct": 0} for name in METRIC_NAMES}
    validate_host_coverage_summary(
        {
            "total": file_coverage(),
            "/repo/extension/src/host/fileExplorer.ts": weighted(100, 80),
            f"/repo/{HOST_COVERAGE_DROPS[0]}": dropped,
        }
    )


def test_host_only_selects_the_labeled_source() -> None:
    selected = [source for source in SOURCES if source.labels]
    assert [source.title for source in selected] == ["Extension-host client"]
    assert selected[0].labels == HOST_COVERAGE_LABELS


# Host coverage floor wiring (#662, #526).
def test_host_thresholds_cover_every_metric() -> None:
    assert set(HOST_COVERAGE_THRESHOLDS) == set(COVERAGE_METRICS)


def test_host_thresholds_allow_a_fully_covered_summary() -> None:
    validate_host_coverage_summary(
        {
            "total": file_coverage(),
            "/repo/extension/src/host/fileExplorer.ts": file_coverage(),
        }
    )


@pytest.mark.parametrize("metric_name", METRIC_NAMES)
def test_host_threshold_rejects_a_summary_below_the_floor(
    metric_name: str,
) -> None:
    # Only the named metric sits below its floor; the rest are at 100%.
    threshold = HOST_COVERAGE_THRESHOLDS[metric_name]
    floor = int(threshold)
    # One count under the floor, still non-zero so only the floor can fail.
    total = 1000
    covered_below = floor * total // 100 - 1
    file_payload: FileCoverage = {}
    for metric_id in COVERAGE_METRICS:
        if metric_id == metric_name:
            file_payload[metric_id] = {
                "total": total,
                "covered": covered_below,
                "skipped": 0,
                "pct": 100 * covered_below / total,
            }
        else:
            file_payload[metric_id] = {
                "total": total,
                "covered": total,
                "skipped": 0,
                "pct": 100.0,
            }

    with pytest.raises(
        RuntimeError,
        match=rf"host coverage {metric_name} is .*below the {floor}% floor",
    ):
        validate_host_coverage_summary(
            {
                "total": {"lines": file_coverage()},
                "/repo/extension/src/host/fileExplorer.ts": file_payload,
            }
        )


# Pin the exact floors so a drive-by edit to the constant is caught.
def test_host_threshold_values_pin_the_intended_floors() -> None:
    assert HOST_COVERAGE_THRESHOLDS == {
        "lines": 80,
        "statements": 80,
        "branches": 71,
        "functions": 76,
    }
