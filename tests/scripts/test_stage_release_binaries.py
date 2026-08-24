from __future__ import annotations

from pathlib import Path
from types import ModuleType


def test_stages_a_platform_dir(
    stage_release_binaries: ModuleType, tmp_path: Path
) -> None:
    artifacts = tmp_path / "artifacts"
    binary = artifacts / "server-linux-x64" / "cwtools-server"
    binary.parent.mkdir(parents=True)
    binary.write_text("x\n", encoding="utf-8")
    extension = tmp_path / "extension"

    assert stage_release_binaries.main([str(artifacts), str(extension)]) == 0

    staged = (
        extension / "bin" / "server" / "cwtools-server" / "linux-x64" / "cwtools-server"
    )
    assert staged.is_file()
