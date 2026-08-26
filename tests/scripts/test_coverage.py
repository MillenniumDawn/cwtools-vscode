from __future__ import annotations

import json
import re
from pathlib import Path

import pytest

from coverage_metrics import (
    HOST_COVERAGE_DROPS,
    HOST_COVERAGE_LABELS,
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
    match = re.search(r"coverage:\s*\{.*?include:\s*\[(.*?)\]", config, re.DOTALL)
    assert match is not None, "could not find coverage.include in vitest.config.ts"
    included = re.findall(r'"([^"]+)"', match.group(1))
    host_owned = {
        path
        for path in included
        if path.startswith(("extension/src/host/", "extension/src/common/"))
    }
    assert set(HOST_COVERAGE_DROPS) == host_owned


def test_host_only_selects_the_labeled_source() -> None:
    selected = [source for source in SOURCES if source.labels]
    assert [source.title for source in selected] == ["Extension-host client"]
    assert selected[0].labels == HOST_COVERAGE_LABELS
