"""Test campaign command and retention without launching a fuzzer."""

import json
import os
import signal
import subprocess
from pathlib import Path
from unittest.mock import MagicMock

import pytest

import fuzz_campaign


def test_failure_retains_timeout_reproducer_and_limits(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    output = tmp_path / "result"
    monkeypatch.setenv("GITHUB_SHA", "abc123")
    process = MagicMock(spec=subprocess.Popen)
    process.__enter__.return_value = process
    process.wait.return_value = 77
    spawn = MagicMock(return_value=process)
    monkeypatch.setattr(subprocess, "Popen", spawn)
    artifacts = output / "artifacts"
    artifacts.mkdir(parents=True)
    (artifacts / "timeout-deadbeef").write_bytes(b"exact input")

    assert fuzz_campaign.run_campaign("parse_string", tmp_path / "corpus", output) == 77

    command = spawn.call_args.args[0]
    assert "-max_total_time=60" in command
    assert all(limit in command for limit in fuzz_campaign.LIMITS)
    assert spawn.call_args.kwargs["start_new_session"] is True
    metadata = json.loads((output / "metadata.json").read_text(encoding="utf-8"))
    assert metadata["target"] == "parse_string"
    assert metadata["source_revision"] == "abc123"
    assert metadata["rust_toolchain"] == "nightly-2026-07-24"
    assert metadata["cargo_fuzz"] == "0.13.2"
    assert metadata["status"] == "exited 77"
    assert (artifacts / "timeout-deadbeef").read_bytes() == b"exact input"
    replay = (output / "replay.txt").read_text(encoding="utf-8")
    assert "fuzz-campaign/artifacts/timeout-deadbeef -- -runs=1" in replay
    assert all(limit in replay for limit in fuzz_campaign.LIMITS)


def test_wall_timeout_stops_group_and_preserves_partial_artifact(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    output = tmp_path / "result"
    artifacts = output / "artifacts"
    artifacts.mkdir(parents=True)
    (artifacts / "crash-partial").write_bytes(b"partial")
    monkeypatch.setenv("GITHUB_SHA", "abc123")
    process = MagicMock(spec=subprocess.Popen)
    process.__enter__.return_value = process
    process.pid = 12345
    process.wait.side_effect = subprocess.TimeoutExpired("cargo", 1200)
    monkeypatch.setattr(subprocess, "Popen", MagicMock(return_value=process))
    stop = MagicMock()
    monkeypatch.setattr(fuzz_campaign, "stop_group", stop)
    # Finalization must work on hosts that have no POSIX process-group APIs.
    monkeypatch.delattr(os, "killpg", raising=False)
    monkeypatch.delattr(signal, "SIGKILL", raising=False)

    assert fuzz_campaign.run_campaign("fix_edits", tmp_path / "corpus", output) == 124

    stop.assert_called_once_with(process)
    assert (artifacts / "crash-partial").read_bytes() == b"partial"
    assert "crash-partial" in (output / "replay.txt").read_text(encoding="utf-8")
    metadata = json.loads((output / "metadata.json").read_text(encoding="utf-8"))
    assert metadata["status"] == "wall timeout"


@pytest.mark.skipif(
    not hasattr(os, "killpg") or not hasattr(signal, "SIGKILL"),
    reason="process-group signaling requires POSIX killpg and SIGKILL",
)
def test_stop_group_signals_process_group(monkeypatch: pytest.MonkeyPatch) -> None:
    process = MagicMock(spec=subprocess.Popen)
    process.__enter__.return_value = process
    process.pid = 12345
    process.wait.return_value = 0
    kill = MagicMock()
    monkeypatch.setattr(os, "killpg", kill)

    fuzz_campaign.stop_group(process)

    assert kill.call_args_list == [
        ((12345, signal.SIGTERM),),
        ((12345, signal.SIGKILL),),
    ]
