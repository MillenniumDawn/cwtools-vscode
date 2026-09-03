from __future__ import annotations

import subprocess
import sys
from pathlib import Path
from typing import Self, cast

import pytest

import esbuild


# wait_for_watcher_exit only ever calls poll(), so a stub keeps the test from
# spawning two esbuild processes to watch them exit.
class FakeProcess:
    def __init__(self, code: int | None) -> None:
        self.code = code

    def poll(self) -> int | None:
        return self.code


def procs(*codes: int | None) -> list[subprocess.Popen[bytes]]:
    return cast("list[subprocess.Popen[bytes]]", [FakeProcess(code) for code in codes])


TEST_ENV_DEFINES = (
    "--define:process.env.CWTOOLS_TEST_HOI4_REPO=undefined",
    "--define:process.env.CWTOOLS_TEST_HOI4_REF=undefined",
    "--define:process.env.CWTOOLS_TEST_RULES_MANIFEST_URL=undefined",
)


def test_extension_bundle_is_node_cjs() -> None:
    args = " ".join(esbuild.extension_args(watch=False))
    assert "--platform=node" in args
    assert "--format=cjs" in args
    assert "--external:vscode" in args
    assert "extension.ts" in args
    assert "--watch" not in args
    for define in TEST_ENV_DEFINES:
        assert define not in args


def test_release_folds_test_env_overrides() -> None:
    args = " ".join(esbuild.extension_args(watch=False, release=True))
    for define in TEST_ENV_DEFINES:
        assert define in args


def test_webview_bundle_is_browser_iife() -> None:
    args = " ".join(esbuild.webview_args(watch=False, dev=False))
    assert "--platform=browser" in args
    assert "--format=iife" in args
    assert "--global-name=cwtoolsgraph" in args
    assert 'process.env.NODE_ENV="production"' in args
    assert 'window.process = { env: { NODE_ENV: "production" } };' in args
    assert "graph.ts" in args


def test_dev_and_watch_flags() -> None:
    webview = " ".join(esbuild.webview_args(watch=True, dev=True))
    assert 'process.env.NODE_ENV="development"' in webview
    assert 'window.process = { env: { NODE_ENV: "development" } };' in webview
    assert "--watch" in webview
    assert "--watch" in " ".join(esbuild.extension_args(watch=True))


def test_watch_implies_dev(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    stub_bin = tmp_path / "esbuild"
    stub_bin.touch()
    monkeypatch.setattr(esbuild, "esbuild_bin", lambda: stub_bin)
    commands: list[list[str]] = []

    class StubProcess:
        def __init__(self, cmd: list[str], **_kwargs: object) -> None:
            commands.append(cmd)

        def poll(self) -> int:
            return 0

        def __enter__(self) -> Self:
            return self

        def __exit__(self, *_args: object) -> None:
            return None

        def terminate(self) -> None:
            return None

    monkeypatch.setattr(subprocess, "Popen", StubProcess)
    assert esbuild.main(["--watch"]) == 0
    webview = " ".join(commands[1])
    assert "--watch" in webview
    assert 'window.process = { env: { NODE_ENV: "development" } };' in webview


def test_dev_flag_sets_development_without_watch(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    stub_bin = tmp_path / "esbuild"
    stub_bin.touch()
    monkeypatch.setattr(esbuild, "esbuild_bin", lambda: stub_bin)
    commands: list[list[str]] = []
    monkeypatch.setattr(esbuild, "_run", commands.append)
    assert esbuild.main(["--dev"]) == 0
    assert 'window.process = { env: { NODE_ENV: "development" } };' in " ".join(
        commands[1]
    )
    assert "--watch" not in " ".join(commands[0])
    assert "--watch" not in " ".join(commands[1])
    for define in TEST_ENV_DEFINES:
        assert define not in " ".join(commands[0])


def test_release_flag_defines_test_env(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    stub_bin = tmp_path / "esbuild"
    stub_bin.touch()
    monkeypatch.setattr(esbuild, "esbuild_bin", lambda: stub_bin)
    commands: list[list[str]] = []
    monkeypatch.setattr(esbuild, "_run", commands.append)
    assert esbuild.main(["--release"]) == 0
    extension = " ".join(commands[0])
    for define in TEST_ENV_DEFINES:
        assert define in extension
    assert "--watch" not in extension


def test_bundle_commands_use_the_platform_esbuild_entrypoint() -> None:
    commands = esbuild.bundle_commands(watch=False, dev=False)
    assert len(commands) == 2
    index = 1 if sys.platform == "win32" else 0
    for command in commands:
        assert command[index] == str(esbuild.esbuild_bin())


def test_watch_returns_when_the_second_bundle_exits() -> None:
    assert esbuild.wait_for_watcher_exit(procs(None, 7), poll_interval=0) == 7


def test_watch_treats_an_early_clean_exit_as_failure() -> None:
    assert esbuild.wait_for_watcher_exit(procs(None, 0), poll_interval=0) == 1
