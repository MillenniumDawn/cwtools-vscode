from __future__ import annotations

import json
from collections.abc import Callable
from pathlib import Path
from typing import cast
from zipfile import ZipFile

import pytest

import build

Command = list[str]
cmd_publish_prebuilt = cast(Callable[[], None], vars(build)["cmd_publish_prebuilt"])
cmd_publish_marketplace = cast(
    Callable[[], None], vars(build)["cmd_publish_marketplace"]
)
cmd_publish_github = cast(Callable[[], None], vars(build)["cmd_publish_github"])
find_vsixes = cast(Callable[[str], list[str]], vars(build)["find_vsixes"])
publish_github_release = cast(
    Callable[[str, str, bool, list[str]], None],
    vars(build)["publish_github_release"],
)
publish_to_marketplace = cast(
    Callable[[list[str], bool], None],
    vars(build)["publish_to_marketplace"],
)

CHANGELOG = """### Unreleased

* Work in progress.

### 2.5.0

* Added the widget.
"""


def _write_vsix(path: Path, version: str) -> str:
    with ZipFile(path, "w") as archive:
        archive.writestr("extension/package.json", json.dumps({"version": version}))
    return str(path)


# A re-run of `Publish: GitHub` after a partial failure finds the release its
# first attempt created. It is skipped, never deleted and recreated: the same
# --skip-duplicate contract the registries have, whichever way the run was
# triggered.
@pytest.mark.parametrize("tag_release", [None, "true"])
def test_an_existing_release_is_skipped_never_deleted(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, tag_release: str | None
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

    def record_marketplace(vsixes: list[str], _pre_release: bool = False) -> None:
        marketplace_calls.append(vsixes)

    if tag_release is None:
        monkeypatch.delenv("TAG_RELEASE", raising=False)
    else:
        monkeypatch.setenv("TAG_RELEASE", tag_release)
    monkeypatch.delenv("CWTOOLS_BUILD_VERSION", raising=False)
    monkeypatch.delenv("CWTOOLS_RELEASE_TAG", raising=False)
    monkeypatch.delenv("GITHUB_REF_NAME", raising=False)
    monkeypatch.setattr(build, "VSIX_ROOT", vsix_root)
    monkeypatch.setattr(build, "read_changelog", lambda: CHANGELOG)
    monkeypatch.setattr(build, "release_notes", lambda _changelog, _version: "notes")
    monkeypatch.setattr(build, "find_vsixes", lambda _version: [vsix])
    monkeypatch.setattr(build, "publish_to_marketplace", record_marketplace)
    monkeypatch.setattr(build, "run_or_null", run_or_null)
    monkeypatch.setattr(build, "run", run)

    cmd_publish_prebuilt()

    assert commands == [["gh", "release", "view", "v2.5.0"]]
    assert not (vsix_root / "release-notes.md").exists()
    # The GitHub half being done is no reason to skip the Marketplace half.
    assert marketplace_calls == [[vsix]]


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

    def record_marketplace(vsixes: list[str], _pre_release: bool = False) -> None:
        marketplace_calls.append(vsixes)

    monkeypatch.delenv("TAG_RELEASE", raising=False)
    monkeypatch.delenv("CWTOOLS_BUILD_VERSION", raising=False)
    monkeypatch.delenv("CWTOOLS_RELEASE_TAG", raising=False)
    monkeypatch.delenv("GITHUB_REF_NAME", raising=False)
    monkeypatch.delenv("GITHUB_SHA", raising=False)
    monkeypatch.setattr(build, "VSIX_ROOT", vsix_root)
    monkeypatch.setattr(build, "read_changelog", lambda: CHANGELOG)
    monkeypatch.setattr(build, "release_notes", lambda _changelog, _version: "notes")
    monkeypatch.setattr(build, "find_vsixes", lambda _version: [vsix])
    monkeypatch.setattr(build, "publish_to_marketplace", record_marketplace)
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


def _captured_publish(
    monkeypatch: pytest.MonkeyPatch, vsixes: list[str], pre_release: bool
) -> tuple[list[Command], list[dict[str, str]]]:
    commands: list[Command] = []
    environments: list[dict[str, str]] = []

    def capture_run(cmd: str, args: list[str], **kwargs: object) -> None:
        commands.append([cmd, *args])
        env = kwargs["env"]
        assert isinstance(env, dict)
        environments.append(env)

    monkeypatch.setenv("VSCE_TOKEN", "pat")
    monkeypatch.setenv("INHERITED_ENV", "inherited")
    monkeypatch.setattr(build, "run", capture_run)
    publish_to_marketplace(vsixes, pre_release)
    return commands, environments


def test_marketplace_publish_uploads_one_package_at_a_time(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    commands, environments = _captured_publish(
        monkeypatch, ["one.vsix", "two.vsix"], True
    )

    assert commands == [
        [
            "npx",
            "--no-install",
            "vsce",
            "publish",
            "--skip-duplicate",
            "--pre-release",
            "--packagePath",
            vsix,
        ]
        for vsix in ("one.vsix", "two.vsix")
    ]
    assert all("--pat" not in command for command in commands)
    assert all("pat" not in argument for command in commands for argument in command)
    assert all(environment["VSCE_PAT"] == "pat" for environment in environments)
    assert all("VSCE_TOKEN" not in environment for environment in environments)
    assert all(
        environment["INHERITED_ENV"] == "inherited" for environment in environments
    )


def test_marketplace_publish_leaves_a_stable_package_unflagged(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    commands, _environments = _captured_publish(monkeypatch, ["one.vsix"], False)

    assert len(commands) == 1
    assert "--pre-release" not in commands[0]
    assert "--skip-duplicate" in commands[0]
    assert commands[0][-2:] == ["--packagePath", "one.vsix"]


def _flaky_publish(
    monkeypatch: pytest.MonkeyPatch, failures: dict[str, int]
) -> tuple[list[str], list[float]]:
    """Fail each vsix `failures[vsix]` times before letting it through."""
    attempts: list[str] = []
    sleeps: list[float] = []
    seen: dict[str, int] = {}

    def run(_cmd: str, args: list[str], **_kwargs: object) -> None:
        vsix = args[-1]
        attempts.append(vsix)
        seen[vsix] = seen.get(vsix, 0) + 1
        if seen[vsix] <= failures.get(vsix, 0):
            raise RuntimeError("command failed (1): npx vsce publish")

    monkeypatch.setenv("VSCE_TOKEN", "pat")
    monkeypatch.setattr(build, "run", run)
    monkeypatch.setattr(build, "_sleep", sleeps.append)
    return attempts, sleeps


def test_marketplace_publish_retries_a_transient_failure(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    attempts, sleeps = _flaky_publish(monkeypatch, {"one.vsix": 1})

    publish_to_marketplace(["one.vsix"], False)

    assert attempts == ["one.vsix", "one.vsix"]
    assert sleeps == [15]


def test_marketplace_publish_reports_only_what_stayed_broken(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    attempts, sleeps = _flaky_publish(monkeypatch, {"one.vsix": 99})

    with pytest.raises(RuntimeError) as error:
        publish_to_marketplace(["one.vsix", "two.vsix"], False)

    assert str(error.value) == "Marketplace publish failed for: one.vsix"
    # One vsix exhausting its attempts must not strand the next one.
    assert attempts == ["one.vsix", "one.vsix", "one.vsix", "two.vsix"]
    assert sleeps == [15, 45]


def test_publish_marketplace_command_skips_the_github_release(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    vsix = str(tmp_path / "cwtools.vsix")
    marketplace_calls: list[tuple[list[str], bool]] = []

    def publish_to_marketplace_stub(vsixes: list[str], pre_release: bool) -> None:
        marketplace_calls.append((vsixes, pre_release))

    def fail(*_args: object, **_kwargs: object) -> None:
        raise AssertionError("publish-marketplace must not touch GitHub releases")

    monkeypatch.setenv("CWTOOLS_BUILD_VERSION", "3.5.42")
    monkeypatch.setenv("CWTOOLS_RELEASE_TAG", "v3.5.42-pre.1")
    monkeypatch.setattr(build, "read_changelog", lambda: CHANGELOG)
    monkeypatch.setattr(build, "find_vsixes", lambda _version: [vsix])
    monkeypatch.setattr(build, "publish_to_marketplace", publish_to_marketplace_stub)
    monkeypatch.setattr(build, "publish_github_release", fail)

    cmd_publish_marketplace()

    assert marketplace_calls == [([vsix], True)]


def test_publish_github_command_skips_the_marketplace(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    vsix = str(tmp_path / "cwtools.vsix")
    release_calls: list[tuple[str, str, bool, list[str]]] = []

    def publish_github_release_stub(
        tag: str, version: str, pre_release: bool, vsixes: list[str]
    ) -> None:
        release_calls.append((tag, version, pre_release, vsixes))

    def fail(*_args: object, **_kwargs: object) -> None:
        raise AssertionError("publish-github must not touch the Marketplace")

    monkeypatch.setenv("CWTOOLS_BUILD_VERSION", "3.5.42")
    monkeypatch.setenv("CWTOOLS_RELEASE_TAG", "v3.5.42-pre.1")
    monkeypatch.setattr(build, "read_changelog", lambda: CHANGELOG)
    monkeypatch.setattr(build, "find_vsixes", lambda _version: [vsix])
    monkeypatch.setattr(build, "publish_github_release", publish_github_release_stub)
    monkeypatch.setattr(build, "publish_to_marketplace", fail)

    cmd_publish_github()

    assert release_calls == [("v3.5.42-pre.1", "3.5.42", True, [vsix])]


def test_find_vsixes_keeps_matching_platform_and_universal_packages(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    vsix_root = tmp_path / "vsix"
    vsix_root.mkdir()
    platform = _write_vsix(vsix_root / "cwtools-3.5.42-linux-x64.vsix", "3.5.42")
    universal = _write_vsix(vsix_root / "cwtools-3.5.42.vsix", "3.5.42")
    monkeypatch.setattr(build, "VSIX_ROOT", vsix_root)

    assert find_vsixes("3.5.42") == [platform, universal]


@pytest.mark.parametrize(
    "command",
    [cmd_publish_prebuilt, cmd_publish_marketplace, cmd_publish_github],
    ids=["prebuilt", "marketplace", "github"],
)
def test_standalone_publish_commands_reject_off_version_vsixes_before_publishing(
    command: Callable[[], None], tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    vsix_root = tmp_path / "vsix"
    vsix_root.mkdir()
    _write_vsix(vsix_root / "cwtools-3.5.42.vsix", "3.5.42")
    stale = _write_vsix(vsix_root / "cwtools-3.5.42-linux-x64.vsix", "3.4.99")
    publishers: list[str] = []

    def record_github(
        _tag: str, _version: str, _pre_release: bool, _vsixes: list[str]
    ) -> None:
        publishers.append("github")

    def record_marketplace(_vsixes: list[str], _pre_release: bool) -> None:
        publishers.append("marketplace")

    monkeypatch.setenv("CWTOOLS_BUILD_VERSION", "3.5.42")
    monkeypatch.setenv("CWTOOLS_RELEASE_TAG", "v3.5.42-pre.1")
    monkeypatch.setattr(build, "VSIX_ROOT", vsix_root)
    monkeypatch.setattr(build, "read_changelog", lambda: CHANGELOG)
    monkeypatch.setattr(build, "publish_github_release", record_github)
    monkeypatch.setattr(build, "publish_to_marketplace", record_marketplace)

    with pytest.raises(RuntimeError) as error:
        command()

    assert str(error.value) == (
        f"VSIX manifest version mismatch: {stale} has '3.4.99'; expected '3.5.42'"
    )
    assert not publishers


def test_prerelease_notes_replace_the_missing_changelog_section(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    vsix_root = tmp_path / "vsix"
    vsix_root.mkdir()
    vsix = str(vsix_root / "cwtools.vsix")
    commands: list[Command] = []

    def refuse(_changelog: str, version: str) -> str:
        raise RuntimeError(f"no CHANGELOG section for version {version}")

    monkeypatch.setenv("GITHUB_SHA", "cafef00d")
    monkeypatch.delenv("TAG_RELEASE", raising=False)
    monkeypatch.setattr(build, "VSIX_ROOT", vsix_root)
    monkeypatch.setattr(build, "read_changelog", lambda: CHANGELOG)
    # A pre-release version has no section, so reaching for one is the bug.
    monkeypatch.setattr(build, "release_notes", refuse)
    monkeypatch.setattr(build, "run_or_null", lambda *_a, **_k: 1)
    monkeypatch.setattr(
        build, "run", lambda cmd, args, **_kwargs: commands.append([cmd, *args])
    )

    publish_github_release("v3.5.42-pre.1", "3.5.42", True, [vsix])

    notes = (vsix_root / "release-notes.md").read_text(encoding="utf-8")
    assert "3.5.42" in notes
    assert "cafef00d" in notes
    assert commands[-1][-3:] == ["--target", "cafef00d", "--prerelease"]
