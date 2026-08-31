from __future__ import annotations

from collections.abc import Callable
from typing import cast

import pytest

import build

Command = list[str]
release_preflight = cast(Callable[[], None], vars(build)["release_preflight"])


def test_refuses_untracked_paths(monkeypatch: pytest.MonkeyPatch) -> None:
    commands: list[Command] = []

    def run_or_null(cmd: str, args: list[str], **_kwargs: object) -> int:
        commands.append([cmd, *args])
        return 0

    def run_capture(cmd: str, args: list[str], **_kwargs: object) -> str:
        commands.append([cmd, *args])
        return "new-file.txt\nassets/new-file.txt\n"

    monkeypatch.setattr(build, "run_or_null", run_or_null)
    monkeypatch.setattr(build, "run_capture", run_capture)

    with pytest.raises(RuntimeError) as error:
        release_preflight()

    assert str(error.value) == (
        "working tree has untracked files; commit or remove them before tagging "
        "a release:\n  new-file.txt\n  assets/new-file.txt"
    )
    assert commands == [
        ["git", "diff", "--quiet", "HEAD"],
        ["git", "ls-files", "--others", "--exclude-standard"],
    ]


def test_reports_tracked_git_error(monkeypatch: pytest.MonkeyPatch) -> None:
    commands: list[Command] = []

    def run_or_null(cmd: str, args: list[str], **_kwargs: object) -> int:
        commands.append([cmd, *args])
        return 128

    monkeypatch.setattr(build, "run_or_null", run_or_null)

    with pytest.raises(RuntimeError, match=r"git diff check failed \(128\)"):
        release_preflight()

    assert commands == [["git", "diff", "--quiet", "HEAD"]]


def test_refuses_head_missing_from_origin_main(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    commands: list[Command] = []
    statuses = iter([0, 1])
    outputs = iter(["", "abc1234\n"])

    def run_or_null(cmd: str, args: list[str], **_kwargs: object) -> int:
        commands.append([cmd, *args])
        return next(statuses)

    def run_capture(cmd: str, args: list[str], **_kwargs: object) -> str:
        commands.append([cmd, *args])
        return next(outputs)

    def run(cmd: str, args: list[str], **_kwargs: object) -> None:
        commands.append([cmd, *args])

    monkeypatch.setattr(build, "run_or_null", run_or_null)
    monkeypatch.setattr(build, "run_capture", run_capture)
    monkeypatch.setattr(build, "run", run)

    with pytest.raises(
        RuntimeError, match="HEAD abc1234 is not present on origin/main"
    ):
        release_preflight()

    assert commands == [
        ["git", "diff", "--quiet", "HEAD"],
        ["git", "ls-files", "--others", "--exclude-standard"],
        ["git", "fetch", "--quiet", "origin", "main"],
        ["git", "merge-base", "--is-ancestor", "HEAD", "origin/main"],
        ["git", "rev-parse", "--short", "HEAD"],
    ]


def test_reports_merge_base_git_error(monkeypatch: pytest.MonkeyPatch) -> None:
    commands: list[Command] = []
    statuses = iter([0, 128])

    def run_or_null(cmd: str, args: list[str], **_kwargs: object) -> int:
        commands.append([cmd, *args])
        return next(statuses)

    def run_capture(cmd: str, args: list[str], **_kwargs: object) -> str:
        commands.append([cmd, *args])
        return ""

    def run(cmd: str, args: list[str], **_kwargs: object) -> None:
        commands.append([cmd, *args])

    monkeypatch.setattr(build, "run_or_null", run_or_null)
    monkeypatch.setattr(build, "run_capture", run_capture)
    monkeypatch.setattr(build, "run", run)

    with pytest.raises(RuntimeError, match=r"git merge-base check failed \(128\)"):
        release_preflight()

    assert commands == [
        ["git", "diff", "--quiet", "HEAD"],
        ["git", "ls-files", "--others", "--exclude-standard"],
        ["git", "fetch", "--quiet", "origin", "main"],
        ["git", "merge-base", "--is-ancestor", "HEAD", "origin/main"],
    ]


def test_allows_a_clean_pushed_tree(monkeypatch: pytest.MonkeyPatch) -> None:
    commands: list[Command] = []

    def run_or_null(cmd: str, args: list[str], **_kwargs: object) -> int:
        commands.append([cmd, *args])
        return 0

    def run_capture(cmd: str, args: list[str], **_kwargs: object) -> str:
        commands.append([cmd, *args])
        return ""

    def run(cmd: str, args: list[str], **_kwargs: object) -> None:
        commands.append([cmd, *args])

    monkeypatch.setattr(build, "run_or_null", run_or_null)
    monkeypatch.setattr(build, "run_capture", run_capture)
    monkeypatch.setattr(build, "run", run)

    release_preflight()

    assert commands == [
        ["git", "diff", "--quiet", "HEAD"],
        ["git", "ls-files", "--others", "--exclude-standard"],
        ["git", "fetch", "--quiet", "origin", "main"],
        ["git", "merge-base", "--is-ancestor", "HEAD", "origin/main"],
    ]
