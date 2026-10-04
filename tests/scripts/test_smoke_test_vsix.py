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
    "bin/client/webview/graph.css",
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
    include_graph_css: bool = True,
    include_flat: bool = False,
    server_files: dict[str, str] | None = None,
    forbidden_paths: list[str] | None = None,
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
            if relative.endswith("graph.css") and not include_graph_css:
                continue
            archive.writestr(f"extension/{relative}", "x\n")
        for platform in platforms:
            exe = "cwtools-server.exe" if platform == "win-x64" else "cwtools-server"
            archive.writestr(
                f"extension/bin/server/cwtools-server/{platform}/{exe}", "binary\n"
            )
        for relative, content in (server_files or {}).items():
            archive.writestr(f"extension/bin/server/cwtools-server/{relative}", content)
        for relative in forbidden_paths or []:
            archive.writestr(f"extension/{relative}", "forbidden\n")


def test_accepts_targeted_and_universal_vsixes(
    smoke_test_vsix: ModuleType, tmp_path: Path
) -> None:
    targets = {
        "linux-x64": "linux-x64",
        "linux-arm64": "linux-arm64",
        "win32-x64": "win-x64",
        "darwin-x64": "osx-x64",
        "darwin-arm64": "osx-arm64",
    }
    for target, platform in targets.items():
        write_vsix(tmp_path / f"ext-{target}-1.0.0.vsix", [platform])
    write_vsix(tmp_path / "ext-1.0.0.vsix", list(targets.values()))

    assert smoke_test_vsix.main([str(tmp_path), *targets.values()]) == 0


def test_accepts_a_lone_universal_vsix_with_only_a_flat_binary(
    smoke_test_vsix: ModuleType, tmp_path: Path
) -> None:
    write_vsix(tmp_path / "ext-1.0.0.vsix", [], include_flat=True)

    assert smoke_test_vsix.main([str(tmp_path)]) == 0


def test_accepts_a_flat_windows_binary(
    smoke_test_vsix: ModuleType, tmp_path: Path
) -> None:
    write_vsix(
        tmp_path / "ext-1.0.0.vsix",
        [],
        server_files={"cwtools-server.exe": "binary\n"},
    )

    assert smoke_test_vsix.main([str(tmp_path)]) == 0


def test_rejects_an_unexpected_flat_file_in_a_targeted_vsix(
    smoke_test_vsix: ModuleType,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    write_vsix(
        tmp_path / "ext-linux-x64-1.0.0.vsix",
        ["linux-x64"],
        server_files={"cwtools-server.pdb": "debug symbols\n"},
    )

    with pytest.raises(SystemExit) as caught:
        smoke_test_vsix.main([str(tmp_path)])
    assert caught.value.code == 1
    assert (
        "::error::ext-linux-x64-1.0.0.vsix: unexpected flat server files "
        "[cwtools-server.pdb] in bin/server/cwtools-server\n"
    ) in capsys.readouterr().out


@pytest.mark.parametrize("empty_name", [None, "cwtools-server", "cwtools-server.exe"])
def test_checks_both_flat_executables(
    smoke_test_vsix: ModuleType,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
    empty_name: str | None,
) -> None:
    files = {"cwtools-server": "binary\n", "cwtools-server.exe": "binary\n"}
    if empty_name is not None:
        files[empty_name] = ""
    write_vsix(tmp_path / "ext-1.0.0.vsix", [], server_files=files)

    if empty_name is None:
        assert smoke_test_vsix.main([str(tmp_path)]) == 0
    else:
        with pytest.raises(SystemExit) as caught:
            smoke_test_vsix.main([str(tmp_path)])
        assert caught.value.code == 1
        assert (
            "::error::ext-1.0.0.vsix: server executable "
            f"bin/server/cwtools-server/{empty_name} is empty\n"
        ) in capsys.readouterr().out


@pytest.mark.parametrize(
    ("server_files", "message"),
    [
        pytest.param(
            {"linux-x64/README.txt": "readme\n"},
            "missing server executable "
            "bin/server/cwtools-server/linux-x64/cwtools-server",
            id="readme-only-platform-directory",
        ),
        pytest.param(
            {"linux-x64/cwtools-server": ""},
            "server executable "
            "bin/server/cwtools-server/linux-x64/cwtools-server is empty",
            id="empty-executable",
        ),
        pytest.param(
            {"win-x64/cwtools-server.exe": ""},
            "server executable "
            "bin/server/cwtools-server/win-x64/cwtools-server.exe is empty",
            id="empty-windows-executable",
        ),
        pytest.param(
            {"linux-x64/cwtools-server": "binary\n", "win-x64/cwtools-server": "x\n"},
            "missing server executable "
            "bin/server/cwtools-server/win-x64/cwtools-server.exe",
            id="windows-executable-without-exe-suffix",
        ),
        pytest.param(
            {"linux-x64/cwtools-server.exe": "binary\n"},
            "missing server executable "
            "bin/server/cwtools-server/linux-x64/cwtools-server",
            id="posix-executable-with-exe-suffix",
        ),
        pytest.param(
            {"cwtools-server": ""},
            "server executable bin/server/cwtools-server/cwtools-server is empty",
            id="empty-flat-executable",
        ),
        pytest.param(
            {"cwtools-server.exe": ""},
            "server executable bin/server/cwtools-server/cwtools-server.exe is empty",
            id="empty-flat-windows-executable",
        ),
        pytest.param(
            {"README.txt": "readme\n"},
            "no server binaries at all",
            id="readme-only-flat-directory",
        ),
    ],
)
def test_rejects_a_server_directory_without_its_executable(
    smoke_test_vsix: ModuleType,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
    server_files: dict[str, str],
    message: str,
) -> None:
    write_vsix(tmp_path / "ext-1.0.0.vsix", [], server_files=server_files)

    with pytest.raises(SystemExit) as caught:
        smoke_test_vsix.main([str(tmp_path)])
    assert caught.value.code == 1
    assert f"::error::ext-1.0.0.vsix: {message}\n" in capsys.readouterr().out


def test_rejects_a_flat_universal_alongside_targeted(
    smoke_test_vsix: ModuleType, tmp_path: Path
) -> None:
    write_vsix(tmp_path / "ext-1.0.0.vsix", [], include_flat=True)
    write_vsix(tmp_path / "ext-linux-x64-1.0.0.vsix", ["linux-x64"])

    assert smoke_test_vsix.main([str(tmp_path)]) == 1


def test_rejects_targeted_only_vsixes(
    smoke_test_vsix: ModuleType, tmp_path: Path
) -> None:
    write_vsix(tmp_path / "ext-linux-x64-1.0.0.vsix", ["linux-x64"])

    assert smoke_test_vsix.main([str(tmp_path), "linux-x64"]) == 1


def test_rejects_a_missing_generated_graph_stylesheet(
    smoke_test_vsix: ModuleType, tmp_path: Path
) -> None:
    write_vsix(tmp_path / "ext-1.0.0.vsix", ["linux-x64"], include_graph_css=False)

    with pytest.raises(SystemExit) as caught:
        smoke_test_vsix.main([str(tmp_path)])
    assert caught.value.code == 1


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


@pytest.mark.parametrize(
    "forbidden_path",
    [
        "bin/client/test/workspaces/stellaris/common/example.txt",
        "out/extension.js",
        "bin/client/extension/extension.js.map",
    ],
)
def test_rejects_forbidden_package_content(
    smoke_test_vsix: ModuleType, tmp_path: Path, forbidden_path: str
) -> None:
    write_vsix(
        tmp_path / "ext-1.0.0.vsix",
        ["linux-x64"],
        forbidden_paths=[forbidden_path],
    )

    with pytest.raises(SystemExit) as caught:
        smoke_test_vsix.main([str(tmp_path)])
    assert caught.value.code == 1


def test_reports_all_missing_expected_platforms(
    smoke_test_vsix: ModuleType, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    write_vsix(tmp_path / "ext-1.0.0.vsix", ["linux-x64"])

    assert smoke_test_vsix.main([str(tmp_path), "win-x64", "osx-arm64"]) == 1

    output = capsys.readouterr().out
    assert "expected platform win-x64 was not packaged" in output
    assert "expected platform osx-arm64 was not packaged" in output
