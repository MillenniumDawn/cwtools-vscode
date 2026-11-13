"""Exercise the pinned checker wrapper without network access."""

import hashlib
import io
import subprocess
import tarfile
from pathlib import Path
from unittest.mock import MagicMock

import pytest

import actionlint


def make_archive(member_name: str = "actionlint", *, directory: bool = False) -> bytes:
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w:gz") as tar:
        info = tarfile.TarInfo(member_name)
        if directory:
            info.type = tarfile.DIRTYPE
            tar.addfile(info)
        else:
            payload = b"checker"
            info.size = len(payload)
            tar.addfile(info, io.BytesIO(payload))
    return output.getvalue()


def install_archive(
    monkeypatch: pytest.MonkeyPatch, payload: bytes
) -> tuple[MagicMock, MagicMock]:
    monkeypatch.setattr(actionlint, "SHA256", hashlib.sha256(payload).hexdigest())
    download = MagicMock(return_value=io.BytesIO(payload))
    process = MagicMock(return_value=subprocess.CompletedProcess([], 2))
    monkeypatch.setattr(actionlint, "urlopen", download)
    monkeypatch.setattr(actionlint.subprocess, "run", process)
    return download, process


def test_verified_archive_propagates_checker_exit(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    download, process = install_archive(monkeypatch, make_archive())
    assert actionlint.main() == 2
    download.assert_called_once_with(actionlint.URL, timeout=60)
    command = process.call_args.args[0]
    assert len(command) == 1
    assert Path(command[0]).name == "actionlint"
    assert process.call_args.kwargs["check"] is False


def test_checksum_mismatch_prevents_execution(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(
        actionlint, "urlopen", MagicMock(return_value=io.BytesIO(b"bad"))
    )
    process = MagicMock()
    monkeypatch.setattr(actionlint.subprocess, "run", process)
    assert actionlint.main() == 1
    process.assert_not_called()


@pytest.mark.parametrize(
    "payload", [b"not a tarball", make_archive("other"), make_archive(directory=True)]
)
def test_invalid_archive_prevents_execution(
    monkeypatch: pytest.MonkeyPatch, payload: bytes
) -> None:
    _, process = install_archive(monkeypatch, payload)
    assert actionlint.main() == 1
    process.assert_not_called()


def test_network_error_prevents_execution(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(
        actionlint, "urlopen", MagicMock(side_effect=OSError("offline"))
    )
    process = MagicMock()
    monkeypatch.setattr(actionlint.subprocess, "run", process)
    assert actionlint.main() == 1
    process.assert_not_called()
