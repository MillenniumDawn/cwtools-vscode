from __future__ import annotations

import json
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

from coverage_metrics import validate_host_coverage_summary
from hosttest import resolve_display, test_cli_command
from paths import REPO_ROOT

HOST_COVERAGE_TIMEOUT_MS = 5 * 60 * 1000
HOST_COVERAGE_KILL_GRACE_MS = 5_000

COVERAGE_DIR = REPO_ROOT / "coverage"
SUMMARY_PATH = COVERAGE_DIR / "coverage-summary.json"


def _parse_pids(text: str) -> list[int]:
    pids: list[int] = []
    for part in text.split():
        try:
            value = int(part, 10)
        except ValueError:
            continue
        if value > 0:
            pids.append(value)
    return pids


def child_pids(pid: int) -> list[int]:
    children = Path(f"/proc/{pid}/task/{pid}/children")
    try:
        return _parse_pids(children.read_text(encoding="utf-8"))
    except OSError:
        pass
    pgrep = shutil.which("pgrep")
    if pgrep is None:
        return []
    result = subprocess.run(
        [pgrep, "-P", str(pid)],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode not in {0, 1}:
        return []
    return _parse_pids(result.stdout)


def kill_process_tree(pid: int, sig: str) -> None:
    if os.name == "nt":
        args = ["taskkill", "/PID", str(pid), "/T"]
        if sig == "SIGKILL":
            args.append("/F")
        subprocess.run(
            args,
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        return
    for child in child_pids(pid):
        kill_process_tree(child, sig)
    signo = (
        signal.SIGKILL
        if sig == "SIGKILL" and hasattr(signal, "SIGKILL")
        else signal.SIGTERM
    )
    try:
        os.kill(pid, signo)
    except ProcessLookupError:
        return
    except PermissionError:
        return


def run_with_timeout(
    name: str,
    command: str,
    args: list[str],
    *,
    cwd: str,
    timeout_ms: int,
    grace_ms: int,
    stdio: str = "inherit",
    env: dict[str, str] | None = None,
) -> None:
    stdout = None if stdio == "inherit" else subprocess.DEVNULL
    proc = subprocess.Popen(
        [command, *args],
        cwd=cwd,
        stdout=stdout,
        stderr=stdout,
        env=env,
    )
    if proc.pid is None:
        raise RuntimeError(f"{name} failed to start")
    deadline = time.monotonic() + (timeout_ms / 1000)
    while proc.poll() is None and time.monotonic() < deadline:
        time.sleep(0.05)
    if proc.poll() is None:
        kill_process_tree(proc.pid, "SIGTERM")
        grace_deadline = time.monotonic() + (grace_ms / 1000)
        while proc.poll() is None and time.monotonic() < grace_deadline:
            time.sleep(0.05)
        if proc.poll() is None:
            kill_process_tree(proc.pid, "SIGKILL")
            proc.wait()
        raise RuntimeError(f"{name} timed out after {timeout_ms}ms")
    if proc.returncode == 0:
        return
    if proc.returncode is None:
        raise RuntimeError(f"{name} failed with exit code unknown")
    if proc.returncode < 0:
        raise RuntimeError(f"{name} failed with signal {-proc.returncode}")
    raise RuntimeError(f"{name} failed with exit code {proc.returncode}")


def _run(name: str, command: str, args: list[str]) -> None:
    result = subprocess.run([command, *args], cwd=REPO_ROOT, check=False)
    if result.returncode != 0:
        raise RuntimeError(f"{name} failed with exit code {result.returncode}")


def _npm() -> str:
    found = shutil.which("npm")
    if found is None:
        raise RuntimeError("npm is not on PATH")
    return found


def clean_coverage() -> None:
    try:
        shutil.rmtree(COVERAGE_DIR)
    except FileNotFoundError:
        return


def main() -> int:
    clean_coverage()
    try:
        display = resolve_display()
        if display.note is not None:
            sys.stderr.write(f"{display.note}\n")
        _run("extension compilation", _npm(), ["run", "compile"])
        command = display.prefix + test_cli_command(["unit"], coverage=True)
        run_with_timeout(
            "extension-host coverage",
            command[0],
            command[1:],
            cwd=str(REPO_ROOT),
            timeout_ms=HOST_COVERAGE_TIMEOUT_MS,
            grace_ms=HOST_COVERAGE_KILL_GRACE_MS,
            env=display.env(),
        )
        raw = json.loads(SUMMARY_PATH.read_text(encoding="utf-8"))
        validate_host_coverage_summary(raw)
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError):
        clean_coverage()
        raise
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        detail = str(error) if str(error) else type(error).__name__
        sys.stderr.write(f"{detail}\n")
        raise SystemExit(1) from error
