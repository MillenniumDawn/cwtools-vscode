from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path
from unittest.mock import Mock

import pytest

import python_coverage
from coverage_summary import SOURCES, render_coverage_section
from coverage_summary import main as summary_main


def report() -> dict[str, object]:
    return {
        "meta": {"branch_coverage": True},
        "files": {
            "scripts/helper.py": {
                "summary": {
                    "num_statements": 10,
                    "covered_lines": 9,
                    "num_branches": 4,
                    "covered_branches": 3,
                },
                "missing_lines": [8],
                "missing_branches": [[7, 8]],
            }
        },
    }


@pytest.fixture(name="source_root")
def source_root_fixture(tmp_path: Path) -> Path:
    (tmp_path / "scripts").mkdir()
    (tmp_path / "scripts/helper.py").write_text("pass\n", encoding="utf-8")
    return tmp_path


def test_summary_uses_measured_lines_and_branches(source_root: Path) -> None:
    summary = python_coverage.summarize(report(), source_root)
    assert summary["scripts/helper.py"]["lines"] == {
        "total": 10,
        "covered": 9,
        "pct": 90,
    }
    assert summary["scripts/helper.py"]["branches"] == {
        "total": 4,
        "covered": 3,
        "pct": 75,
    }
    assert "functions" not in summary["scripts/helper.py"]


@pytest.mark.parametrize("metric", ["lines", "branches"])
def test_floor_rejects_a_measured_regression(source_root: Path, metric: str) -> None:
    summary = python_coverage.summarize(report(), source_root)
    with pytest.raises(RuntimeError, match=f"{metric}.*below"):
        python_coverage.enforce_floors(summary, {metric: 99})
    python_coverage.enforce_floors(summary, {"lines": 90, "branches": 75})


def test_unimported_production_helpers_cannot_disappear(source_root: Path) -> None:
    (source_root / "scripts/build").mkdir()
    (source_root / "scripts/build/unimported.py").write_text("pass\n", encoding="utf-8")
    with pytest.raises(RuntimeError, match=r"inventory.*unimported\.py"):
        python_coverage.summarize(report(), source_root)


def test_test_code_cannot_count_as_production(source_root: Path) -> None:
    raw = report()
    raw["files"] = {"tests/scripts/test_helper.py": {}}
    with pytest.raises(RuntimeError, match="inventory"):
        python_coverage.summarize(raw, source_root)


@pytest.mark.parametrize("raw", [{}, {"meta": {"branch_coverage": False}}])
def test_missing_branch_measurement_is_rejected(raw: object, source_root: Path) -> None:
    with pytest.raises(RuntimeError, match="branch measurement"):
        python_coverage.summarize(raw, source_root)


def test_failed_collection_removes_stale_success(
    source_root: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    output = source_root / "coverage-python"
    output.mkdir()
    (output / "coverage-summary.json").write_text("stale", encoding="utf-8")
    (output / ".coverage").write_text("stale", encoding="utf-8")
    monkeypatch.setattr(python_coverage, "ROOT", source_root)
    run = Mock(side_effect=subprocess.CalledProcessError(3, ["coverage"]))
    monkeypatch.setattr(subprocess, "run", run)
    with pytest.raises(subprocess.CalledProcessError):
        python_coverage.main()
    assert not list(output.iterdir())
    assert run.call_args.args[0][:4] == [sys.executable, "-m", "coverage", "run"]


def test_missing_data_cannot_publish_a_summary(
    source_root: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(python_coverage, "ROOT", source_root)
    monkeypatch.setattr(subprocess, "run", Mock())
    with pytest.raises(RuntimeError, match="did not create"):
        python_coverage.main()
    assert not (source_root / "coverage-python/coverage-summary.json").exists()


def test_python_summary_joins_existing_toolchains() -> None:
    source = next(item for item in SOURCES if item.title == "Python helpers")
    assert source.required_metrics == ("lines", "statements", "branches")
    summary = {
        "scripts/helper.py": {
            "lines": {"total": 10, "covered": 9, "pct": 90},
            "statements": {"total": 10, "covered": 9, "pct": 90},
            "branches": {"total": 4, "covered": 3, "pct": 75},
        }
    }
    # The file artifact retains missing lines and branch arcs; the shared
    # overview reports percentages and leaves functions unavailable.
    rendered = render_coverage_section(source, json.loads(json.dumps(summary)))
    assert "Python helpers" in rendered.overview
    assert "Production scripts only" in "\n".join(rendered.details)
    assert "75.0" in rendered.overview
    assert "⚪ -" in rendered.overview


@pytest.mark.parametrize("passes_floor", [True, False])
def test_only_successful_measurement_publishes_a_summary(
    source_root: Path, monkeypatch: pytest.MonkeyPatch, passes_floor: bool
) -> None:
    output = source_root / "coverage-python"
    monkeypatch.setattr(python_coverage, "ROOT", source_root)
    monkeypatch.setattr(
        python_coverage, "FLOORS", {"lines": 90, "branches": 75 if passes_floor else 76}
    )

    def run(*_args: object, **_kwargs: object) -> None:
        (output / ".coverage").write_text("fresh data", encoding="utf-8")

    def write_report(*, outfile: str) -> None:
        Path(outfile).write_text(json.dumps(report()), encoding="utf-8")

    measured = Mock()
    measured.json_report.side_effect = write_report
    monkeypatch.setattr(subprocess, "run", run)
    monkeypatch.setattr(python_coverage, "Coverage", Mock(return_value=measured))
    if passes_floor:
        assert python_coverage.main() == 0
        summary = json.loads((output / "coverage-summary.json").read_text())
        assert summary["scripts/helper.py"]["branches"]["covered"] == 3
    else:
        with pytest.raises(RuntimeError, match="below"):
            python_coverage.main()
        assert not (output / "coverage-summary.json").exists()
    measured.load.assert_called_once()
    measured.html_report.assert_called_once_with(directory=str(output / "html"))


def test_shared_collector_requires_python_when_ci_requests_it(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    with pytest.raises(
        RuntimeError, match="required Python coverage summary is missing"
    ):
        summary_main(["--require-python"])
