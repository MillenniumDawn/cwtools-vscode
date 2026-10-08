from __future__ import annotations

import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Self, cast

import pytest

import esbuild
from paths import EXTENSION_HOST_ROOT, EXTENSION_SOURCE_ROOT


# wait_for_watcher_exit only ever calls poll(), so a stub keeps the test from
# spawning two esbuild processes to watch them exit.
class FakeProcess:
    def __init__(self, code: int | None) -> None:
        self.code = code

    def poll(self) -> int | None:
        return self.code


def procs(*codes: int | None) -> list[subprocess.Popen[bytes]]:
    return cast("list[subprocess.Popen[bytes]]", [FakeProcess(code) for code in codes])


# `*` (not `+`) so a bare CWTOOLS_TEST_ prefix is captured: a computed read
# such as process.env[`CWTOOLS_TEST_${name}`] or process.env["CWTOOLS_TEST_"
# + name] builds the name at run time, which no `--define:` can fold.
_TEST_ENV_NAME = re.compile(r"\b(CWTOOLS_TEST_[A-Za-z0-9_]*)\b")
_TEST_ENV_READ = re.compile(
    r"(?<![A-Za-z0-9_$.])process\.env\.(CWTOOLS_TEST_[A-Za-z0-9_]+)(?![A-Za-z0-9_$])"
)


def _test_env_names_in_source(source: str) -> tuple[set[str], set[str]]:
    direct_reads = {
        match.start(1): match.group(1) for match in _TEST_ENV_READ.finditer(source)
    }
    names: set[str] = set()
    unsupported: set[str] = set()
    for match in _TEST_ENV_NAME.finditer(source):
        name = match.group(1)
        if direct_reads.get(match.start(1)) == name:
            names.add(name)
        else:
            unsupported.add(name)
    return names, unsupported


def _source_test_env_names() -> set[str]:
    names: set[str] = set()
    unsupported: list[str] = []
    for path in EXTENSION_SOURCE_ROOT.rglob("*"):
        if not path.is_file() or path.suffix not in {
            ".ts",
            ".tsx",
            ".js",
            ".jsx",
            ".mjs",
            ".cjs",
        }:
            continue
        source_names, invalid_names = _test_env_names_in_source(
            path.read_text(encoding="utf-8")
        )
        names.update(source_names)
        if invalid_names:
            relative_path = path.relative_to(EXTENSION_SOURCE_ROOT)
            unsupported.append(f"{relative_path}: {', '.join(sorted(invalid_names))}")
    assert not unsupported, (
        "Every CWTOOLS_TEST_* occurrence in extension/src must be a direct "
        "process.env.NAME read; unsupported references:\n" + "\n".join(unsupported)
    )
    return names


_TEST_ENV_DEFINE = re.compile(
    r"--define:process\.env\.(CWTOOLS_TEST_[A-Za-z0-9_]+)=(.*)"
)


def _test_env_defines(args: list[str]) -> dict[str, str]:
    defines: dict[str, str] = {}
    for arg in args:
        match = _TEST_ENV_DEFINE.fullmatch(arg)
        if match:
            defines[match.group(1)] = match.group(2)
    return defines


def _esbuild_test_args(source: Path, outfile: Path, *, release: bool) -> list[str]:
    args = esbuild.bundle_commands(watch=False, dev=False, release=release)[0]
    entrypoint = str(EXTENSION_HOST_ROOT / "extension.ts")
    args[args.index(entrypoint)] = str(source)
    outfile_index = next(
        index for index, arg in enumerate(args) if arg.startswith("--outfile=")
    )
    args[outfile_index] = f"--outfile={outfile}"
    return args


@pytest.mark.parametrize(
    ("source", "direct_reads", "unsupported"),
    [
        (
            "const rules = process.env.CWTOOLS_TEST_RULES_FOLDER;",
            {"CWTOOLS_TEST_RULES_FOLDER"},
            set(),
        ),
        (
            'const rules = process.env["CWTOOLS_TEST_BRACKET"];',
            set(),
            {"CWTOOLS_TEST_BRACKET"},
        ),
        (
            "const { CWTOOLS_TEST_DESTRUCTURED } = process.env;",
            set(),
            {"CWTOOLS_TEST_DESTRUCTURED"},
        ),
        (
            "const env = process.env; const rules = env.CWTOOLS_TEST_ALIASED;",
            set(),
            {"CWTOOLS_TEST_ALIASED"},
        ),
        (
            "const rules = process.env[`CWTOOLS_TEST_${name}`];",
            set(),
            {"CWTOOLS_TEST_"},
        ),
        (
            'const rules = process.env["CWTOOLS_TEST_" + name];',
            set(),
            {"CWTOOLS_TEST_"},
        ),
    ],
)
def test_test_env_reads_must_use_direct_process_env_properties(
    source: str,
    direct_reads: set[str],
    unsupported: set[str],
) -> None:
    assert _test_env_names_in_source(source) == (direct_reads, unsupported)


def test_extension_bundle_is_node_cjs() -> None:
    extension_args = esbuild.extension_args(watch=False)
    args = " ".join(extension_args)
    assert "--platform=node" in args
    assert "--format=cjs" in args
    assert "--external:vscode" in args
    assert "extension.ts" in args
    assert "--watch" not in args
    assert not _test_env_defines(extension_args)


def test_release_defines_match_test_env_reads() -> None:
    expected = dict.fromkeys(_source_test_env_names(), "undefined")
    release_args = esbuild.extension_args(watch=False, release=True)
    assert _test_env_defines(release_args) == expected


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
    assert not _test_env_defines(commands[0])


def test_release_flag_defines_test_env(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    stub_bin = tmp_path / "esbuild"
    stub_bin.touch()
    monkeypatch.setattr(esbuild, "esbuild_bin", lambda: stub_bin)
    commands: list[list[str]] = []
    monkeypatch.setattr(esbuild, "_run", commands.append)
    assert esbuild.main(["--release"]) == 0
    assert _test_env_defines(commands[0]) == _test_env_defines(
        esbuild.extension_args(watch=False, release=True)
    )
    assert "--watch" not in " ".join(commands[0])


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


def test_release_define_drops_cwtools_test_from_js(tmp_path: Path) -> None:
    binary = esbuild.esbuild_bin()
    if not binary.is_file():
        if os.environ.get("CI"):
            pytest.fail("esbuild is not installed in CI; real-esbuild smoke must run")
        pytest.skip("esbuild is not installed")

    source = tmp_path / "repo.ts"
    source.write_text(
        "export const repo = process.env.CWTOOLS_TEST_RULES_FOLDER ?? "
        '"https://ok.example";\n',
        encoding="utf-8",
    )
    dropped = tmp_path / "dropped.js"
    kept = tmp_path / "kept.js"
    folded = subprocess.run(
        _esbuild_test_args(source, dropped, release=True),
        check=False,
        capture_output=True,
        text=True,
    )
    assert folded.returncode == 0, folded.stderr

    live = subprocess.run(
        _esbuild_test_args(source, kept, release=False),
        check=False,
        capture_output=True,
        text=True,
    )
    assert live.returncode == 0, live.stderr

    dropped_js = dropped.read_text(encoding="utf-8")
    kept_js = kept.read_text(encoding="utf-8")
    assert "CWTOOLS_TEST" not in dropped_js
    assert "https://ok.example" in dropped_js
    assert "CWTOOLS_TEST" in kept_js
