from __future__ import annotations

from collections.abc import Callable
from typing import cast

import pytest

import build

Command = list[str]
StubGit = Callable[[list[int], list[str]], list[Command]]
release_preflight = cast(Callable[[], None], vars(build)["release_preflight"])


@pytest.fixture(name="stub_git")
def stub_git_fixture(monkeypatch: pytest.MonkeyPatch) -> StubGit:
    def setup(statuses: list[int], outputs: list[str]) -> list[Command]:
        commands: list[Command] = []
        status_iter = iter(statuses)
        output_iter = iter(outputs)

        def run_or_null(cmd: str, args: list[str], **_kwargs: object) -> int:
            commands.append([cmd, *args])
            return next(status_iter)

        def run_capture(cmd: str, args: list[str], **_kwargs: object) -> str:
            commands.append([cmd, *args])
            return next(output_iter)

        def run(cmd: str, args: list[str], **_kwargs: object) -> None:
            commands.append([cmd, *args])

        monkeypatch.setattr(build, "run_or_null", run_or_null)
        monkeypatch.setattr(build, "run_capture", run_capture)
        monkeypatch.setattr(build, "run", run)
        return commands

    return setup


def test_refuses_untracked_paths(stub_git: StubGit) -> None:
    commands = stub_git([0], ["new-file.txt\nassets/new-file.txt\n"])

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


def test_reports_tracked_git_error(stub_git: StubGit) -> None:
    commands = stub_git([128], [])

    with pytest.raises(RuntimeError, match=r"git diff check failed \(128\)"):
        release_preflight()

    assert commands == [["git", "diff", "--quiet", "HEAD"]]


def test_refuses_head_missing_from_origin_main(stub_git: StubGit) -> None:
    commands = stub_git([0, 1], ["", "abc1234\n"])

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


def test_reports_merge_base_git_error(stub_git: StubGit) -> None:
    commands = stub_git([0, 128], [""])

    with pytest.raises(RuntimeError, match=r"git merge-base check failed \(128\)"):
        release_preflight()

    assert commands == [
        ["git", "diff", "--quiet", "HEAD"],
        ["git", "ls-files", "--others", "--exclude-standard"],
        ["git", "fetch", "--quiet", "origin", "main"],
        ["git", "merge-base", "--is-ancestor", "HEAD", "origin/main"],
    ]


def test_allows_a_clean_pushed_tree(stub_git: StubGit) -> None:
    commands = stub_git([0, 0], [""])

    release_preflight()

    assert commands == [
        ["git", "diff", "--quiet", "HEAD"],
        ["git", "ls-files", "--others", "--exclude-standard"],
        ["git", "fetch", "--quiet", "origin", "main"],
        ["git", "merge-base", "--is-ancestor", "HEAD", "origin/main"],
    ]
