from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

import coverage_metrics
from coverage_metrics import main, source_ownership, validate_node_coverage_summary

HOST = "extension/src/host/lspClient.ts"
NODE = "extension/src/host/engine.ts"


@pytest.fixture(name="ownership")
def ownership_fixture(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(
        coverage_metrics, "executable_source_ownership", lambda: ({HOST}, {NODE})
    )


def test_inventory_subprocess_result(monkeypatch: pytest.MonkeyPatch) -> None:
    def inventory(
        command: list[str], **_kwargs: object
    ) -> subprocess.CompletedProcess[str]:
        assert command[0] == "node"
        assert Path(command[1]).name == "coverage_inventory.mjs"
        return subprocess.CompletedProcess(
            command, 0, json.dumps({"sourceFiles": [HOST, NODE], "include": [NODE]})
        )

    monkeypatch.setattr(subprocess, "run", inventory)
    assert coverage_metrics.executable_source_ownership() == ({HOST}, {NODE})


@pytest.mark.parametrize(
    "new_source", ["extension/src/webview/new.ts", "extension/src/other/new.ts"]
)
def test_new_source_without_owner_fails(new_source: str) -> None:
    with pytest.raises(RuntimeError, match="no coverage owner"):
        source_ownership({HOST, NODE, new_source}, {NODE})


def test_overlapping_owners_fail() -> None:
    with pytest.raises(RuntimeError, match="overlap"):
        source_ownership({HOST}, {HOST})


def test_unknown_include_fails() -> None:
    with pytest.raises(RuntimeError, match="unknown modules"):
        source_ownership({HOST}, {"extension/src/host/missing.ts"})


@pytest.mark.usefixtures("ownership")
@pytest.mark.parametrize("prefix", ["/repo/", "C:\\repo\\"])
def test_node_entry_point_checks_report_membership(tmp_path: Path, prefix: str) -> None:
    summary = tmp_path / "summary.json"
    summary.write_text(json.dumps({prefix + HOST: {}}), encoding="utf-8")
    with pytest.raises(RuntimeError, match="missing executable Node modules"):
        main([str(summary)])
    summary.write_text(json.dumps({prefix + NODE: {}}), encoding="utf-8")
    assert main([str(summary)]) == 0


@pytest.mark.usefixtures("ownership")
def test_node_report_requires_object() -> None:
    with pytest.raises(RuntimeError, match="not an object"):
        validate_node_coverage_summary([])


def test_node_coverage_command_requires_inventory_check() -> None:
    root = Path(__file__).resolve().parents[2]
    manifest = json.loads((root / "package.json").read_text(encoding="utf-8"))
    assert manifest["scripts"]["test:node:coverage"] == (
        "vitest run --coverage && node scripts/build/python.mjs "
        "scripts/build/coverage_metrics.py coverage-node/coverage-summary.json"
    )
