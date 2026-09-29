from __future__ import annotations

import json
import os
import sys
from pathlib import Path

import pytest

import host_coverage
import hosttest


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


def test_runs_host_and_live_labels_in_one_coverage_command(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    coverage_dir = tmp_path / "coverage"
    summary_path = coverage_dir / "coverage-summary.json"
    labels_seen: list[list[str]] = []
    coverage_seen: list[bool] = []
    invoked_command: list[str] = []
    display = hosttest.Display("native", [], None)

    def make_test_cli_command(
        labels: list[str], *, coverage: bool = False
    ) -> list[str]:
        labels_seen.append(labels.copy())
        coverage_seen.append(coverage)
        command = ["node", "test-cli"]
        for label in labels:
            command.extend(["--label", label])
        if coverage:
            command.append("--coverage")
        return command

    def record_command(
        _name: str, command: str, args: list[str], **_kwargs: object
    ) -> None:
        invoked_command.extend([command, *args])
        coverage = {
            metric: {"total": 1, "covered": 1}
            for metric in ("lines", "statements", "branches", "functions")
        }
        summary_path.parent.mkdir(parents=True, exist_ok=True)
        summary_path.write_text(
            json.dumps({"/repo/extension/src/host/lspClient.ts": coverage}),
            encoding="utf-8",
        )

    monkeypatch.setattr(host_coverage, "COVERAGE_DIR", coverage_dir)
    monkeypatch.setattr(host_coverage, "SUMMARY_PATH", summary_path)
    monkeypatch.setattr(host_coverage, "resolve_display", lambda: display)
    monkeypatch.setattr(host_coverage, "_npm", lambda: "npm")
    monkeypatch.setattr(host_coverage, "_run", lambda *_args, **_kwargs: None)
    monkeypatch.setattr(host_coverage, "test_cli_command", make_test_cli_command)
    monkeypatch.setattr(host_coverage, "run_with_timeout", record_command)

    assert host_coverage.main() == 0
    assert labels_seen == [["host", "live"]]
    assert coverage_seen == [True]
    assert invoked_command == [
        "node",
        "test-cli",
        "--label",
        "host",
        "--label",
        "live",
        "--coverage",
    ]
