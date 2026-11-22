from __future__ import annotations

import re
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path

import pytest

from workflow_helpers import job_scope, workflow_step

ROOT = Path(__file__).resolve().parents[2]


@pytest.mark.parametrize("mutation", ["stale", "missing", "incomplete"])
def test_locked_sync_rejects_changed_or_incomplete_lock_without_rewriting(
    tmp_path: Path, mutation: str
) -> None:
    uv = shutil.which("uv")
    assert uv is not None, "the documented locked environment requires uv"
    manifest = (ROOT / "pyproject.toml").read_text(encoding="utf-8")
    lock = (ROOT / "uv.lock").read_text(encoding="utf-8")
    # With no requirements left, uv can detect the mismatch without the index.
    manifest, count = re.subn(r"(?ms)^dev = \[.*?^\]", "dev = []", manifest)
    assert count == 1
    if mutation == "incomplete":
        lock = lock.split("\n[[package]]", maxsplit=1)[0]
    (tmp_path / "pyproject.toml").write_text(manifest, encoding="utf-8")
    lock_path = tmp_path / "uv.lock"
    if mutation != "missing":
        lock_path.write_text(lock, encoding="utf-8")
    result = subprocess.run(
        [uv, "sync", "--locked", "--offline", "--python", sys.executable],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode != 0
    assert "--locked" in result.stderr
    assert "lockfile" in result.stderr
    if mutation == "missing":
        assert not lock_path.exists()
    else:
        assert lock_path.read_text(encoding="utf-8") == lock


def test_ci_installs_from_the_lock_and_gates_python_coverage() -> None:
    workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
    manifest = tomllib.loads((ROOT / "pyproject.toml").read_text(encoding="utf-8"))
    version = manifest["tool"]["uv"]["required-version"].removeprefix(">=")
    for job_name in ("python", "non-ui-native"):
        job = job_scope(workflow, job_name)
        assert "uv sync --locked" in job
        assert f'version: "{version}"' in job
        assert "pip install" not in job
    python = job_scope(workflow, "python")
    assert "scripts/build/python_coverage.py" in workflow_step(
        python, "pytest with production branch coverage"
    )
    assert "name: coverage-python" in workflow_step(python, "Upload Python coverage")
    assert "uv run --locked pytest" in workflow_step(
        job_scope(workflow, "non-ui-native"), "Python helper tests"
    )
    collector = job_scope(workflow, "coverage-report")
    assert "needs: [build, coverage-rs, python]" in collector
    assert "name: coverage-python" in workflow_step(
        collector, "Download Python coverage"
    )
    assert "--require-python" in workflow_step(collector, "Coverage summary")
