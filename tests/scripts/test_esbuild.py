from __future__ import annotations

import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Self, cast

import pytest

import esbuild
from paths import EXTENSION_SOURCE_ROOT


# wait_for_watcher_exit only ever calls poll(), so a stub keeps the test from
# spawning two esbuild processes to watch them exit.
class FakeProcess:
    def __init__(self, code: int | None) -> None:
        self.code = code

    def poll(self) -> int | None:
        return self.code


def procs(*codes: int | None) -> list[subprocess.Popen[bytes]]:
    return cast("list[subprocess.Popen[bytes]]", [FakeProcess(code) for code in codes])


@pytest.fixture(name="test_env_names", scope="session")
def test_env_names_fixture() -> set[str]:
    names = {
        name
        for source in EXTENSION_SOURCE_ROOT.rglob("*.ts")
        for name in re.findall(
            r"\bprocess\.env\.(CWTOOLS_TEST_\w+)",
            source.read_text(encoding="utf-8"),
        )
    }
    assert names
    return names


def test_extension_bundle_is_node_cjs() -> None:
    args = " ".join(esbuild.extension_args(watch=False))
    assert "--platform=node" in args
    assert "--format=cjs" in args
    assert "--external:vscode" in args
    assert "extension.ts" in args
    assert "--watch" not in args
    assert "--define:process.env.CWTOOLS_TEST_" not in args


def test_release_folds_test_env_overrides(test_env_names: set[str]) -> None:
    args = esbuild.extension_args(watch=False, release=True)
    defines = {
        arg for arg in args if arg.startswith("--define:process.env.CWTOOLS_TEST_")
    }
    assert defines == {
        f"--define:process.env.{name}=undefined" for name in test_env_names
    }


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
    assert "--define:process.env.CWTOOLS_TEST_" not in " ".join(commands[0])


def test_release_flag_defines_test_env(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, test_env_names: set[str]
) -> None:
    stub_bin = tmp_path / "esbuild"
    stub_bin.touch()
    monkeypatch.setattr(esbuild, "esbuild_bin", lambda: stub_bin)
    commands: list[list[str]] = []
    monkeypatch.setattr(esbuild, "_run", commands.append)
    assert esbuild.main(["--release"]) == 0
    extension = " ".join(commands[0])
    for name in test_env_names:
        assert f"--define:process.env.{name}=undefined" in extension
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


def test_windows_bundles_use_the_resolved_node(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(sys, "platform", "win32")
    monkeypatch.setattr(shutil, "which", lambda _name: "resolved-node")

    commands = esbuild.bundle_commands(watch=False, dev=False)

    assert all(
        command[:2] == ["resolved-node", str(esbuild.esbuild_bin())]
        for command in commands
    )


def test_windows_bundles_reject_missing_node(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(sys, "platform", "win32")
    monkeypatch.setattr(shutil, "which", lambda _name: None)

    with pytest.raises(RuntimeError, match=r"^node is not on PATH$"):
        esbuild.bundle_commands(watch=False, dev=False)


@pytest.mark.parametrize("release", [False, True], ids=["development", "release"])
def test_bundle_test_env_reads_with_production_args(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    test_env_names: set[str],
    release: bool,
) -> None:
    if not esbuild.esbuild_bin().is_file():
        pytest.skip("esbuild is not installed")

    source = tmp_path / "extension.ts"
    reads = ",\n".join(
        f'process.env.{name} ?? "https://ok.example"' for name in sorted(test_env_names)
    )
    source.write_text(f"export const overrides = [{reads}];\n", encoding="utf-8")
    dist = tmp_path / "dist"
    monkeypatch.setattr(esbuild, "EXTENSION_HOST_ROOT", tmp_path)
    monkeypatch.setattr(esbuild, "EXTENSION_DIST_ROOT", dist)

    result = subprocess.run(
        esbuild.bundle_commands(watch=False, dev=False, release=release)[0],
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr

    bundled = (dist / "bin" / "client" / "extension" / "extension.js").read_text(
        encoding="utf-8"
    )
    assert "https://ok.example" in bundled
    if release:
        assert "CWTOOLS_TEST_" not in bundled
    else:
        for name in test_env_names:
            assert name in bundled
