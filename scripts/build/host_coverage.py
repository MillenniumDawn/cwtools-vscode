from __future__ import annotations

import contextlib
import json
import os
import shutil
import signal
import subprocess
import sys
import time
from collections.abc import Iterator
from typing import NoReturn

from coverage_metrics import HOST_COVERAGE_LABELS, validate_host_coverage_summary
from hosttest import resolve_display, test_cli_command
from paths import REPO_ROOT

HOST_COVERAGE_TIMEOUT_MS = 12 * 60 * 1000
HOST_COVERAGE_KILL_GRACE_MS = 5_000

COVERAGE_DIR = REPO_ROOT / "coverage"
SUMMARY_PATH = COVERAGE_DIR / "coverage-summary.json"


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
    # The root leads its own session, so its pid names the group of descendants.
    signo = signal.SIGKILL if sig == "SIGKILL" else signal.SIGTERM
    with contextlib.suppress(ProcessLookupError, PermissionError):
        os.killpg(pid, signo)


def tree_alive(proc: subprocess.Popen[bytes]) -> bool:
    if proc.poll() is None:
        return True
    # taskkill finds the tree through the root's pid, which Windows reuses.
    if os.name == "nt":
        return False
    try:
        os.killpg(proc.pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        pass
    return True


def stop_process_tree(proc: subprocess.Popen[bytes], grace_ms: int) -> None:
    if tree_alive(proc):
        kill_process_tree(proc.pid, "SIGTERM")
    grace_deadline = time.monotonic() + (grace_ms / 1000)
    while tree_alive(proc) and time.monotonic() < grace_deadline:
        time.sleep(0.05)
    if tree_alive(proc):
        kill_process_tree(proc.pid, "SIGKILL")
    proc.wait()


def _exit_on_signal(signo: int, _frame: object) -> NoReturn:
    raise SystemExit(128 + signo)


@contextlib.contextmanager
def termination_raises() -> Iterator[None]:
    signals = [signal.SIGTERM]
    if hasattr(signal, "SIGHUP"):
        signals.append(signal.SIGHUP)
    previous = {signo: signal.signal(signo, _exit_on_signal) for signo in signals}
    try:
        yield
    finally:
        for signo, handler in previous.items():
            signal.signal(signo, handler)


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
    with subprocess.Popen(
        [command, *args],
        cwd=cwd,
        stdout=stdout,
        stderr=stdout,
        env=env,
        start_new_session=os.name != "nt",
    ) as proc:
        if proc.pid is None:
            raise RuntimeError(f"{name} failed to start")
        deadline = time.monotonic() + (timeout_ms / 1000)
        # The new session gets neither the terminal's Ctrl-C nor its hangup.
        try:
            with termination_raises():
                while proc.poll() is None and time.monotonic() < deadline:
                    time.sleep(0.05)
        except BaseException:
            stop_process_tree(proc, grace_ms)
            raise
        if proc.poll() is None:
            stop_process_tree(proc, grace_ms)
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
    with contextlib.suppress(FileNotFoundError):
        shutil.rmtree(COVERAGE_DIR)


def main() -> int:
    clean_coverage()
    try:
        display = resolve_display()
        if display.note is not None:
            sys.stderr.write(f"{display.note}\n")
        _run("extension compilation", _npm(), ["run", "compile"])
        # Run the separate live workspace alongside host so both contribute to
        # one coverage session. Host also exercises modules like graphPanel.ts.
        # This needs a built cwtools-server binary and network access for the
        # background rules fetch, same as `npm run test:host`.
        command = display.prefix + test_cli_command(
            list(HOST_COVERAGE_LABELS), coverage=True
        )
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
