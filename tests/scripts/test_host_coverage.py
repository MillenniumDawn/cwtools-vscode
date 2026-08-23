from __future__ import annotations

import os
import sys
import tempfile
import time
import unittest
from pathlib import Path

from load import load_build

host_coverage = load_build("host_coverage")


def alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    return True


class RunWithTimeoutTests(unittest.TestCase):
    def test_resolves_when_the_process_exits_0(self) -> None:
        host_coverage.run_with_timeout(
            "ok",
            sys.executable,
            ["-c", "raise SystemExit(0)"],
            cwd=tempfile.gettempdir(),
            timeout_ms=5_000,
            grace_ms=100,
            stdio="ignore",
        )

    def test_rejects_a_non_zero_exit(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "fail failed with exit code 2"):
            host_coverage.run_with_timeout(
                "fail",
                sys.executable,
                ["-c", "raise SystemExit(2)"],
                cwd=tempfile.gettempdir(),
                timeout_ms=5_000,
                grace_ms=100,
                stdio="ignore",
            )

    def test_kills_a_process_that_does_not_exit(self) -> None:
        marker = Path(tempfile.gettempdir()) / (
            f"cwtools-host-coverage-{os.getpid()}-{time.time_ns()}.pid"
        )
        script = (
            "import pathlib, time\n"
            f"pathlib.Path({str(marker)!r}).write_text(str(__import__('os').getpid()))\n"
            "time.sleep(30)\n"
        )
        try:
            with self.assertRaisesRegex(RuntimeError, "hang timed out after 200ms"):
                host_coverage.run_with_timeout(
                    "hang",
                    sys.executable,
                    ["-c", script],
                    cwd=tempfile.gettempdir(),
                    timeout_ms=200,
                    grace_ms=100,
                    stdio="ignore",
                )
            if marker.is_file():
                pid = int(marker.read_text(encoding="utf-8"), 10)
                self.assertTrue(pid > 0)
                self.assertFalse(alive(pid))
        finally:
            if marker.is_file():
                marker.unlink()


class KillProcessTreeTests(unittest.TestCase):
    def test_does_not_throw_for_a_pid_that_is_already_gone(self) -> None:
        host_coverage.kill_process_tree(1_000_000_007, "SIGTERM")


if __name__ == "__main__":
    unittest.main()
