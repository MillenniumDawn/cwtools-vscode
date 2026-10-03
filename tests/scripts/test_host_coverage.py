from __future__ import annotations

import contextlib
import json
import os
import signal
import sys
import time
from collections.abc import Callable, Sequence
from pathlib import Path

import pytest

import host_coverage
import hosttest

requires_proc = pytest.mark.skipif(
    not Path("/proc/self/stat").is_file(),
    reason="needs /proc to tell a zombie from a running process",
)


def alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    return True


def running(pid: int) -> bool:
    try:
        stat = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8")
    except OSError:
        return False
    return stat.rpartition(")")[2].split()[0] != "Z"


def exited_within(pid: int, seconds: float = 2.0) -> bool:
    deadline = time.monotonic() + seconds
    while running(pid) and time.monotonic() < deadline:
        time.sleep(0.01)
    return not running(pid)


def read_pids(marker: Path) -> list[int]:
    try:
        return [int(part) for part in marker.read_text(encoding="utf-8").split()]
    except FileNotFoundError:
        return []


def kill_strays(pids: Sequence[int]) -> None:
    for pid in pids:
        if running(pid):
            with contextlib.suppress(ProcessLookupError):
                os.kill(pid, signal.SIGKILL)


def root_and_child_script(marker: Path, *, root_ignores_term: bool) -> str:
    # The child inherits the ignored SIGTERM across exec, so it cannot die
    # before it has had the chance to install a handler.
    lines = [
        "import os, pathlib, signal, subprocess, sys, time",
        "signal.signal(signal.SIGTERM, signal.SIG_IGN)",
        "sleeper = ['-c', 'import time; time.sleep(30)']",
        "child = subprocess.Popen([sys.executable, *sleeper])",
    ]
    if not root_ignores_term:
        lines.append("signal.signal(signal.SIGTERM, signal.SIG_DFL)")
    lines += [
        f"pathlib.Path({str(marker)!r}).write_text(f'{{os.getpid()}} {{child.pid}}')",
        "time.sleep(30)",
    ]
    return "\n".join(lines)


def interrupt_once(monkeypatch: pytest.MonkeyPatch, ready: Callable[[], bool]) -> None:
    real_sleep = time.sleep

    def interrupt(_seconds: float) -> None:
        monkeypatch.setattr(time, "sleep", real_sleep)
        deadline = time.monotonic() + 10
        while not ready() and time.monotonic() < deadline:
            real_sleep(0.01)
        raise KeyboardInterrupt

    monkeypatch.setattr(time, "sleep", interrupt)


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


@requires_proc
@pytest.mark.parametrize(
    "root_ignores_term",
    [False, True],
    ids=["root exits on SIGTERM", "root ignores SIGTERM"],
)
def test_timeout_kills_a_child_that_ignores_sigterm(
    tmp_path: Path, *, root_ignores_term: bool
) -> None:
    marker = tmp_path / "pids"
    script = root_and_child_script(marker, root_ignores_term=root_ignores_term)

    try:
        with pytest.raises(RuntimeError, match="hang timed out after 1000ms"):
            host_coverage.run_with_timeout(
                "hang",
                sys.executable,
                ["-c", script],
                cwd=str(tmp_path),
                timeout_ms=1_000,
                grace_ms=300,
                stdio="ignore",
            )
        pids = read_pids(marker)
        assert len(pids) == 2
        assert all(exited_within(pid) for pid in pids)
    finally:
        kill_strays(read_pids(marker))


@requires_proc
def test_an_interrupt_during_the_wait_stops_the_whole_group(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    marker = tmp_path / "pids"
    interrupt_once(monkeypatch, lambda: len(read_pids(marker)) == 2)

    try:
        with pytest.raises(KeyboardInterrupt):
            host_coverage.run_with_timeout(
                "interrupted",
                sys.executable,
                ["-c", root_and_child_script(marker, root_ignores_term=False)],
                cwd=str(tmp_path),
                timeout_ms=60_000,
                grace_ms=300,
                stdio="ignore",
            )
        pids = read_pids(marker)
        assert len(pids) == 2
        assert all(exited_within(pid) for pid in pids)
    finally:
        kill_strays(read_pids(marker))


@requires_proc
def test_a_child_that_exits_on_sigterm_gets_the_grace_period(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    marker = tmp_path / "pids"
    ready = tmp_path / "ready"
    done = tmp_path / "done"
    child = "\n".join(
        [
            "import pathlib, signal, sys, time",
            "def finish(*_):",
            "    time.sleep(0.2)",
            f"    pathlib.Path({str(done)!r}).touch()",
            "    sys.exit(0)",
            "signal.signal(signal.SIGTERM, finish)",
            f"pathlib.Path({str(ready)!r}).touch()",
            "time.sleep(30)",
        ]
    )
    root = "\n".join(
        [
            "import os, pathlib, subprocess, sys, time",
            f"child = subprocess.Popen([sys.executable, '-c', {child!r}])",
            "pids = f'{os.getpid()} {child.pid}'",
            f"pathlib.Path({str(marker)!r}).write_text(pids)",
            "time.sleep(30)",
        ]
    )
    interrupt_once(monkeypatch, lambda: ready.is_file() and len(read_pids(marker)) == 2)

    try:
        with pytest.raises(KeyboardInterrupt):
            host_coverage.run_with_timeout(
                "interrupted",
                sys.executable,
                ["-c", root],
                cwd=str(tmp_path),
                timeout_ms=60_000,
                grace_ms=5_000,
                stdio="ignore",
            )
        assert done.is_file()
    finally:
        kill_strays(read_pids(marker))


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
