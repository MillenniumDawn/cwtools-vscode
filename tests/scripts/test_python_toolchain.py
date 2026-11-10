from __future__ import annotations

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
    if mutation == "stale":
        requirement = next(
            item
            for item in tomllib.loads(manifest)["dependency-groups"]["dev"]
            if item.startswith("pytest==")
        )
        manifest = manifest.replace(requirement, requirement.replace("==", ">="))
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


def test_ci_runs_native_non_ui_matrices_and_one_python_coverage_artifact() -> None:
    workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
    for job_name in ("check", "python"):
        job = job_scope(workflow, job_name)
        assert "os: [ubuntu-latest, windows-latest, macos-latest]" in job
        assert "runs-on: ${{ matrix.os }}" in job
        assert "fail-fast: false" in job
    python = job_scope(workflow, "python")
    assert "uv sync --locked" in python
    manifest = tomllib.loads((ROOT / "pyproject.toml").read_text(encoding="utf-8"))
    version = manifest["tool"]["uv"]["required-version"].removeprefix("==")
    assert f'version: "{version}"' in python
    assert "if: runner.os == 'Linux'" in workflow_step(
        python, "Install virtual display test dependencies"
    )
    assert "if: runner.os != 'Linux'" in workflow_step(python, "pytest (native non-UI)")
    assert "uv run --locked pytest" in python
    assert "if: runner.os == 'Linux'" in workflow_step(
        python, "pytest with production branch coverage"
    )
    assert "if: runner.os == 'Linux'" in workflow_step(python, "Upload Python coverage")
    collector = job_scope(workflow, "coverage-report")
    assert "needs: [build, coverage-rs, python]" in collector
    assert "name: coverage-python" in workflow_step(
        collector, "Download Python coverage"
    )
    assert "--require-python" in workflow_step(collector, "Coverage summary")
    assert "runs-on: ubuntu-latest" in job_scope(workflow, "build")
