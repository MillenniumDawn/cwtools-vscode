from __future__ import annotations

from collections.abc import Callable
from pathlib import Path
from typing import cast

import pytest

import build

Command = list[str]
cmd_publish_prebuilt = cast(Callable[[], None], vars(build)["cmd_publish_prebuilt"])
publish_github_release = cast(
    Callable[[str, str, bool, list[str]], None],
    vars(build)["publish_github_release"],
)

CHANGELOG = """### Unreleased

* Work in progress.

### 2.5.0

* Added the widget.
"""


def test_refuses_existing_release_when_non_tag_run_resolves_to_changelog_tag(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    vsix_root = tmp_path / "vsix"
    vsix_root.mkdir()
    vsix = str(vsix_root / "cwtools.vsix")
    commands: list[Command] = []
    marketplace_calls: list[list[str]] = []

    def run_or_null(cmd: str, args: list[str], **_kwargs: object) -> int:
        commands.append([cmd, *args])
        return 0

    def run(cmd: str, args: list[str], **_kwargs: object) -> None:
        commands.append([cmd, *args])

    def publish_to_marketplace(vsixes: list[str]) -> None:
        marketplace_calls.append(vsixes)

    monkeypatch.delenv("TAG_RELEASE", raising=False)
    monkeypatch.delenv("CWTOOLS_BUILD_VERSION", raising=False)
    monkeypatch.delenv("CWTOOLS_RELEASE_TAG", raising=False)
    monkeypatch.delenv("GITHUB_REF_NAME", raising=False)
    monkeypatch.setattr(build, "VSIX_ROOT", vsix_root)
    monkeypatch.setattr(build, "read_changelog", lambda: CHANGELOG)
    monkeypatch.setattr(build, "release_notes", lambda _changelog, _version: "notes")
    monkeypatch.setattr(build, "find_vsixes", lambda: [vsix])
    monkeypatch.setattr(build, "publish_to_marketplace", publish_to_marketplace)
    monkeypatch.setattr(build, "run_or_null", run_or_null)
    monkeypatch.setattr(build, "run", run)

    with pytest.raises(RuntimeError) as error:
        cmd_publish_prebuilt()

    assert str(error.value) == (
        "release v2.5.0 already exists; refusing to delete it on a non-tag run"
    )
    assert commands == [["gh", "release", "view", "v2.5.0"]]
    assert not marketplace_calls
    assert not (vsix_root / "release-notes.md").exists()


def test_deletes_existing_release_on_a_tag_push(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    vsix_root = tmp_path / "vsix"
    vsix_root.mkdir()
    vsix = str(vsix_root / "cwtools.vsix")
    commands: list[Command] = []

    def run_or_null(cmd: str, args: list[str], **_kwargs: object) -> int:
        commands.append([cmd, *args])
        return 0

    def run(cmd: str, args: list[str], **_kwargs: object) -> None:
        commands.append([cmd, *args])

    monkeypatch.setenv("TAG_RELEASE", "true")
    monkeypatch.setattr(build, "VSIX_ROOT", vsix_root)
    monkeypatch.setattr(build, "read_changelog", lambda: CHANGELOG)
    monkeypatch.setattr(build, "release_notes", lambda _changelog, _version: "notes")
    monkeypatch.setattr(build, "run_or_null", run_or_null)
    monkeypatch.setattr(build, "run", run)

    publish_github_release("v3.1.0", "3.1.0", False, [vsix])

    notes_file = str(vsix_root / "release-notes.md")
    assert commands == [
        ["gh", "release", "view", "v3.1.0"],
        ["gh", "release", "delete", "v3.1.0", "--yes"],
        [
            "gh",
            "release",
            "create",
            "v3.1.0",
            vsix,
            "--title",
            "v3.1.0",
            "--notes-file",
            notes_file,
        ],
    ]
    assert (vsix_root / "release-notes.md").read_text(encoding="utf-8") == "notes"


def test_creates_release_when_non_tag_run_does_not_find_existing_release(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    vsix_root = tmp_path / "vsix"
    vsix_root.mkdir()
    vsix = str(vsix_root / "cwtools.vsix")
    commands: list[Command] = []
    marketplace_calls: list[list[str]] = []

    def run_or_null(cmd: str, args: list[str], **_kwargs: object) -> int:
        commands.append([cmd, *args])
        return 1

    def run(cmd: str, args: list[str], **_kwargs: object) -> None:
        commands.append([cmd, *args])

    def publish_to_marketplace(vsixes: list[str]) -> None:
        marketplace_calls.append(vsixes)

    monkeypatch.delenv("TAG_RELEASE", raising=False)
    monkeypatch.delenv("CWTOOLS_BUILD_VERSION", raising=False)
    monkeypatch.delenv("CWTOOLS_RELEASE_TAG", raising=False)
    monkeypatch.delenv("GITHUB_REF_NAME", raising=False)
    monkeypatch.setattr(build, "VSIX_ROOT", vsix_root)
    monkeypatch.setattr(build, "read_changelog", lambda: CHANGELOG)
    monkeypatch.setattr(build, "release_notes", lambda _changelog, _version: "notes")
    monkeypatch.setattr(build, "find_vsixes", lambda: [vsix])
    monkeypatch.setattr(build, "publish_to_marketplace", publish_to_marketplace)
    monkeypatch.setattr(build, "run_or_null", run_or_null)
    monkeypatch.setattr(build, "run", run)

    cmd_publish_prebuilt()

    notes_file = str(vsix_root / "release-notes.md")
    assert commands == [
        ["gh", "release", "view", "v2.5.0"],
        [
            "gh",
            "release",
            "create",
            "v2.5.0",
            vsix,
            "--title",
            "v2.5.0",
            "--notes-file",
            notes_file,
        ],
    ]
    assert ["release", "delete"] not in (command[1:3] for command in commands)
    assert (vsix_root / "release-notes.md").exists()
    assert marketplace_calls == [[vsix]]
