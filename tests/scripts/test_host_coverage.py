from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

import host_coverage


def alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    return True


def test_resolves_when_the_process_exits_0(tmp_path: Path) -> None:
    host_coverage.run_with_timeout(
        "ok",
        sys.executable,
        ["-c", "raise SystemExit(0)"],
        cwd=str(tmp_path),
        timeout_ms=5_000,
        grace_ms=100,
        stdio="ignore",
    )


def test_rejects_a_non_zero_exit(tmp_path: Path) -> None:
    with pytest.raises(RuntimeError, match="fail failed with exit code 2"):
        host_coverage.run_with_timeout(
            "fail",
            sys.executable,
            ["-c", "raise SystemExit(2)"],
            cwd=str(tmp_path),
            timeout_ms=5_000,
            grace_ms=100,
            stdio="ignore",
        )


def test_kills_a_process_that_does_not_exit(tmp_path: Path) -> None:
    marker = tmp_path / "child.pid"
    script = (
        "import os, pathlib, time\n"
        f"pathlib.Path({str(marker)!r}).write_text(str(os.getpid()))\n"
        "time.sleep(30)\n"
    )

    with pytest.raises(RuntimeError, match="hang timed out after 200ms"):
        host_coverage.run_with_timeout(
            "hang",
            sys.executable,
            ["-c", script],
            cwd=str(tmp_path),
            timeout_ms=200,
            grace_ms=100,
            stdio="ignore",
        )

    if marker.is_file():
        pid = int(marker.read_text(encoding="utf-8"), 10)
        assert pid > 0
        assert not alive(pid)


def test_killing_a_pid_that_is_already_gone_does_not_throw() -> None:
    host_coverage.kill_process_tree(1_000_000_007, "SIGTERM")


def test_an_unavailable_display_backend_fails_before_the_compile(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    coverage_dir = tmp_path / "coverage"
    coverage_dir.mkdir()
    (coverage_dir / "coverage-summary.json").write_text("{}", encoding="utf-8")
    compiled = False

    def compile_step(*_args: object, **_kwargs: object) -> None:
        nonlocal compiled
        compiled = True

    def unavailable() -> None:
        raise RuntimeError("xvfb-run is not on PATH")

    monkeypatch.setattr(host_coverage, "COVERAGE_DIR", coverage_dir)
    monkeypatch.setattr(host_coverage, "resolve_display", unavailable)
    monkeypatch.setattr(host_coverage, "_run", compile_step)

    with pytest.raises(RuntimeError, match="xvfb-run is not on PATH"):
        host_coverage.main()

    assert not compiled
    assert not coverage_dir.exists()
