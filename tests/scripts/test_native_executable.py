"""Exercise executable discovery using this runner's native PATH rules."""

import os
from pathlib import Path

import pytest

from executables import require_executable


def test_native_executable_discovery(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    name = "cwtools-native-probe"
    suffix = ".CMD" if os.name == "nt" else ""
    executable = tmp_path / (name + suffix)
    executable.write_text("probe", encoding="utf-8")
    executable.chmod(0o755)
    monkeypatch.setenv("PATH", str(tmp_path))
    if os.name == "nt":
        monkeypatch.setenv("PATHEXT", ".CMD")
    assert Path(require_executable(name)) == executable
    with pytest.raises(RuntimeError, match="not on PATH"):
        require_executable("cwtools-missing-probe")
