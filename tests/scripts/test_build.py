from __future__ import annotations

from collections.abc import Callable
from pathlib import Path
from typing import cast

import pytest

import build

CopyPackageInputs = cast(Callable[[], None], vars(build)["copy_package_inputs"])


def test_copy_package_inputs_prunes_stale_content_and_preserves_generated_files(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    package_root = tmp_path / "package"
    dist_root = tmp_path / "dist"
    package_root.mkdir()
    dist_root.mkdir()

    (package_root / "package.json").write_text("current", encoding="utf-8")
    package_media = package_root / "media"
    package_media.mkdir()
    (package_media / "logo.png").write_text("logo", encoding="utf-8")

    (dist_root / "package.json").write_text("stale", encoding="utf-8")
    (dist_root / "removed.json").write_text("remove", encoding="utf-8")
    removed_dir = dist_root / "removed"
    removed_dir.mkdir()
    (removed_dir / "file").write_text("remove", encoding="utf-8")
    stale_media = dist_root / "media"
    stale_media.mkdir()
    (stale_media / "logo.png").write_text("stale logo", encoding="utf-8")
    (stale_media / "removed.png").write_text("remove", encoding="utf-8")

    generated_bin = dist_root / "bin" / "server"
    generated_bin.mkdir(parents=True)
    (generated_bin / "cwtools-server").write_text("binary", encoding="utf-8")
    for name in ("README.md", "LICENSE.md", "CHANGELOG.md"):
        (dist_root / name).write_text("generated", encoding="utf-8")

    monkeypatch.setattr(build, "EXTENSION_PACKAGE_ROOT", package_root)
    monkeypatch.setattr(build, "EXTENSION_DIST_ROOT", dist_root)

    CopyPackageInputs()

    assert (dist_root / "package.json").read_text(encoding="utf-8") == "current"
    assert (dist_root / "media" / "logo.png").read_text(encoding="utf-8") == "logo"
    assert not (dist_root / "removed.json").exists()
    assert not removed_dir.exists()
    assert not (dist_root / "media" / "removed.png").exists()
    assert (generated_bin / "cwtools-server").read_text(encoding="utf-8") == "binary"
    assert all(
        (dist_root / name).read_text(encoding="utf-8") == "generated"
        for name in ("README.md", "LICENSE.md", "CHANGELOG.md")
    )


def test_copy_package_inputs_replaces_stale_file_with_directory(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    package_root = tmp_path / "package"
    dist_root = tmp_path / "dist"
    package_root.mkdir()
    dist_root.mkdir()

    theme = package_root / "theme"
    theme.mkdir()
    (theme / "colors.json").write_text("current", encoding="utf-8")
    (dist_root / "theme").write_text("stale", encoding="utf-8")

    monkeypatch.setattr(build, "EXTENSION_PACKAGE_ROOT", package_root)
    monkeypatch.setattr(build, "EXTENSION_DIST_ROOT", dist_root)

    CopyPackageInputs()

    assert (dist_root / "theme").is_dir()
    assert (dist_root / "theme" / "colors.json").read_text(
        encoding="utf-8"
    ) == "current"


def test_copy_package_inputs_replaces_stale_directory_with_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    package_root = tmp_path / "package"
    dist_root = tmp_path / "dist"
    package_root.mkdir()
    dist_root.mkdir()

    (package_root / "theme").write_text("current", encoding="utf-8")
    stale_theme = dist_root / "theme"
    stale_theme.mkdir()
    (stale_theme / "colors.json").write_text("stale", encoding="utf-8")

    monkeypatch.setattr(build, "EXTENSION_PACKAGE_ROOT", package_root)
    monkeypatch.setattr(build, "EXTENSION_DIST_ROOT", dist_root)

    CopyPackageInputs()

    assert (dist_root / "theme").is_file()
    assert (dist_root / "theme").read_text(encoding="utf-8") == "current"
