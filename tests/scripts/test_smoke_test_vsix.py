from __future__ import annotations

import json
import zipfile
from pathlib import Path
from types import ModuleType

import pytest

PACKAGE = {
    "main": "./bin/client/extension/extension.js",
    "icon": "media/icon.png",
    "l10n": "./l10n",
    "contributes": {
        "languages": [],
        "grammars": [],
        "themes": [],
        "snippets": [],
    },
}

CONTENTS = [
    "bin/client/extension/extension.js",
    "bin/client/webview/graph.js",
    "bin/client/webview/site.css",
    "media/icon.png",
    "l10n/bundle.l10n.test.json",
    "readme.md",
    "changelog.md",
    "LICENSE.md",
]


def write_vsix(
    path: Path,
    platforms: list[str],
    *,
    include_entrypoint: bool = True,
    include_flat: bool = False,
) -> None:
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr("extension/package.json", json.dumps(PACKAGE))
        if include_flat:
            archive.writestr(
                "extension/bin/server/cwtools-server/cwtools-server", "binary\n"
            )
        for relative in CONTENTS:
            if relative.endswith("extension.js") and not include_entrypoint:
                continue
            archive.writestr(f"extension/{relative}", "x\n")
        for platform in platforms:
            archive.writestr(
                f"extension/bin/server/cwtools-server/{platform}/cwtools-server",
                "binary\n",
            )


def test_accepts_targeted_and_universal_vsixes(
    smoke_test_vsix: ModuleType, tmp_path: Path
) -> None:
    write_vsix(tmp_path / "ext-linux-x64-1.0.0.vsix", ["linux-x64"])
    write_vsix(tmp_path / "ext-1.0.0.vsix", ["linux-x64", "win-x64"])
    write_vsix(tmp_path / "ext-win32-x64-1.0.0.vsix", ["win-x64"])

    assert smoke_test_vsix.main([str(tmp_path), "linux-x64", "win-x64"]) == 0


def test_rejects_a_flat_binary_in_a_universal_vsix(
    smoke_test_vsix: ModuleType, tmp_path: Path
) -> None:
    write_vsix(
        tmp_path / "ext-1.0.0.vsix",
        ["linux-x64", "win-x64"],
        include_flat=True,
    )

    with pytest.raises(SystemExit) as caught:
        smoke_test_vsix.main([str(tmp_path)])
    assert caught.value.code == 1


def test_rejects_a_missing_entrypoint(
    smoke_test_vsix: ModuleType, tmp_path: Path
) -> None:
    write_vsix(tmp_path / "ext-1.0.0.vsix", ["linux-x64"], include_entrypoint=False)

    with pytest.raises(SystemExit) as caught:
        smoke_test_vsix.main([str(tmp_path)])
    assert caught.value.code == 1


def test_rejects_a_binary_for_the_wrong_platform(
    smoke_test_vsix: ModuleType, tmp_path: Path
) -> None:
    write_vsix(tmp_path / "ext-linux-x64-1.0.0.vsix", ["win-x64"])

    with pytest.raises(SystemExit) as caught:
        smoke_test_vsix.main([str(tmp_path)])
    assert caught.value.code == 1
