from __future__ import annotations

from collections.abc import Callable
from pathlib import Path
from typing import cast

import pytest

import build

PackageOne = Callable[[str | None], list[str]]
RunPlatformPackaging = Callable[[Path, Path, list[str], PackageOne], list[str]]
package_vsix = cast(Callable[[str | None], list[str]], vars(build)["package_vsix"])
run_platform_packaging = cast(
    RunPlatformPackaging, vars(build)["run_platform_packaging"]
)

PLATFORMS = ["linux-x64", "osx-arm64", "win32-x64"]


def stage(server_bin_dir: Path, platforms: list[str]) -> None:
    for platform in platforms:
        directory = server_bin_dir / platform
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "cwtools-server").write_text(
            f"binary-{platform}", encoding="utf-8"
        )


def dirs_present(directory: Path) -> list[str]:
    if not directory.is_dir():
        return []
    return sorted(entry.name for entry in directory.iterdir() if entry.is_dir())


def has_binary(directory: Path, platform: str) -> bool:
    return (directory / platform / "cwtools-server").is_file()


@pytest.fixture(name="staged")
def staged_fixture(tmp_path: Path) -> tuple[Path, Path]:
    server_bin_dir = tmp_path / "server" / "cwtools-server"
    stage(server_bin_dir, PLATFORMS)
    return server_bin_dir, tmp_path / "temp" / "server-staging"


@pytest.mark.parametrize("pre_release", [False, True])
def test_package_marks_prerelease_vsixes(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, pre_release: bool
) -> None:
    commands: list[tuple[str, list[str], Path | None]] = []

    def run(
        cmd: str, args: list[str], *, cwd: Path | None = None, **_kwargs: object
    ) -> None:
        commands.append((cmd, args, cwd))

    extension_root = tmp_path / "extension"
    monkeypatch.setattr(build, "EXTENSION_DIST_ROOT", extension_root)
    monkeypatch.setattr(build, "VSIX_ROOT", tmp_path / "vsix")
    monkeypatch.setattr(build, "resolve_version", lambda: {"preRelease": pre_release})
    monkeypatch.setattr(build, "run", run)
    extension_root.mkdir()

    assert package_vsix("linux-x64") == []
    args = ["--no-install", "vsce", "package", "--no-dependencies"]
    if pre_release:
        args.append("--pre-release")
    args.extend(["--target", "linux-x64"])
    assert commands == [("npx", args, extension_root)]


def test_each_per_platform_pass_sees_only_its_own_dir(
    staged: tuple[Path, Path],
) -> None:
    server_bin_dir, holding = staged
    seen: list[tuple[str | None, list[str]]] = []

    def package_one(platform: str | None) -> list[str]:
        seen.append((platform, dirs_present(server_bin_dir)))
        return [platform or "universal"]

    run_platform_packaging(server_bin_dir, holding, PLATFORMS, package_one)

    per_platform = [item for item in seen if item[0] is not None]
    assert len(per_platform) == len(PLATFORMS)
    for platform, present in per_platform:
        assert present == [platform]


def test_restores_the_full_set_when_a_mid_platform_pass_throws(
    staged: tuple[Path, Path],
) -> None:
    server_bin_dir, holding = staged

    def package_one(platform: str | None) -> list[str]:
        if platform == "osx-arm64":
            raise RuntimeError("vsce boom")
        return [platform or "universal"]

    with pytest.raises(RuntimeError, match="vsce boom"):
        run_platform_packaging(server_bin_dir, holding, PLATFORMS, package_one)

    assert dirs_present(server_bin_dir) == PLATFORMS
    assert all(has_binary(server_bin_dir, platform) for platform in PLATFORMS)
    assert not holding.exists()


def test_leaves_the_full_set_when_the_universal_pass_throws(
    staged: tuple[Path, Path],
) -> None:
    server_bin_dir, holding = staged

    def package_one(platform: str | None) -> list[str]:
        if platform is None:
            raise RuntimeError("universal boom")
        return [platform]

    with pytest.raises(RuntimeError, match="universal boom"):
        run_platform_packaging(server_bin_dir, holding, PLATFORMS, package_one)

    assert dirs_present(server_bin_dir) == PLATFORMS
    assert all(has_binary(server_bin_dir, platform) for platform in PLATFORMS)
    assert not holding.exists()


def test_deletes_the_holding_dir_on_full_success(staged: tuple[Path, Path]) -> None:
    server_bin_dir, holding = staged

    vsixes = run_platform_packaging(
        server_bin_dir, holding, PLATFORMS, lambda platform: [platform or "universal"]
    )

    assert vsixes == ["linux-x64", "osx-arm64", "win32-x64", "universal"]
    assert dirs_present(server_bin_dir) == PLATFORMS
    assert not holding.exists()


def test_restores_a_single_staged_set_on_mid_failure(tmp_path: Path) -> None:
    server_bin_dir = tmp_path / "server" / "cwtools-server"
    holding = tmp_path / "temp" / "server-staging"
    stage(server_bin_dir, ["linux-x64"])

    def package_one(_platform: str | None) -> list[str]:
        raise RuntimeError("vsce boom")

    with pytest.raises(RuntimeError, match="vsce boom"):
        run_platform_packaging(server_bin_dir, holding, ["linux-x64"], package_one)

    assert dirs_present(server_bin_dir) == ["linux-x64"]
    assert has_binary(server_bin_dir, "linux-x64")
    assert not holding.exists()
