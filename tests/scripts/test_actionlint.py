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


def install_archives(
    monkeypatch: pytest.MonkeyPatch,
    payload: bytes,
    shellcheck: bytes = make_archive(actionlint.SHELLCHECK_MEMBER),
) -> tuple[MagicMock, MagicMock]:
    monkeypatch.setattr(actionlint, "SHA256", hashlib.sha256(payload).hexdigest())
    monkeypatch.setattr(
        actionlint, "SHELLCHECK_SHA256", hashlib.sha256(shellcheck).hexdigest()
    )
    archives = {actionlint.URL: payload, actionlint.SHELLCHECK_URL: shellcheck}
    download = MagicMock(side_effect=lambda url, **_: io.BytesIO(archives[url]))
    process = MagicMock(return_value=subprocess.CompletedProcess([], 2))
    monkeypatch.setattr(actionlint, "urlopen", download)
    monkeypatch.setattr(subprocess, "run", process)
    return download, process


def test_verified_archives_propagate_checker_exit(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    download, process = install_archives(monkeypatch, make_archive())
    assert actionlint.main() == 2
    assert [call.args[0] for call in download.call_args_list] == [
        actionlint.URL,
        actionlint.SHELLCHECK_URL,
    ]
    executable, shellcheck, pyflakes = process.call_args.args[0]
    assert Path(executable).name == "actionlint"
    assert shellcheck.startswith("-shellcheck=") and shellcheck.endswith("shellcheck")
    assert pyflakes == "-pyflakes="
    assert process.call_args.kwargs["check"] is False


def test_checksum_mismatch_prevents_execution(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(
        actionlint, "urlopen", MagicMock(return_value=io.BytesIO(b"bad"))
    )
    process = MagicMock()
    monkeypatch.setattr(subprocess, "run", process)
    assert actionlint.main() == 1
    process.assert_not_called()


def test_shellcheck_checksum_mismatch_prevents_execution(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _, process = install_archives(monkeypatch, make_archive())
    monkeypatch.setattr(actionlint, "SHELLCHECK_SHA256", "0" * 64)
    assert actionlint.main() == 1
    process.assert_not_called()


@pytest.mark.parametrize(
    "payload", [b"not a tarball", make_archive("other"), make_archive(directory=True)]
)
def test_invalid_archive_prevents_execution(
    monkeypatch: pytest.MonkeyPatch, payload: bytes
) -> None:
    _, process = install_archives(monkeypatch, payload)
    assert actionlint.main() == 1
    process.assert_not_called()


def test_network_error_prevents_execution(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(
        actionlint, "urlopen", MagicMock(side_effect=OSError("offline"))
    )
    process = MagicMock()
    monkeypatch.setattr(subprocess, "run", process)
    assert actionlint.main() == 1
    process.assert_not_called()


@pytest.mark.parametrize(
    ("returncode", "output", "expected"),
    [
        (1, "no_such_property missing_job invalid_fixture_input", 0),
        (0, "no_such_property missing_job invalid_fixture_input", 1),
        (1, "unrelated", 1),
    ],
)
def test_fixtures_require_each_expected_diagnostic(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    returncode: int,
    output: str,
    expected: int,
) -> None:
    monkeypatch.setattr(
        subprocess,
        "run",
        MagicMock(return_value=subprocess.CompletedProcess([], returncode, output, "")),
    )
    assert actionlint.check_fixtures(tmp_path / "actionlint", tmp_path) == expected
