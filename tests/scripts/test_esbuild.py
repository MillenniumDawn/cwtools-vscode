from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Self, cast

import pytest

import esbuild
from paths import (
    EXTENSION_HOST_ROOT,
    EXTENSION_SOURCE_ROOT,
    EXTENSION_WEBVIEW_ROOT,
    REPO_ROOT,
)


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


def _esbuild_test_args(
    source: Path,
    outfile: Path,
    *,
    release: bool = False,
    dev: bool = False,
    webview: bool = False,
) -> list[str]:
    commands = esbuild.bundle_commands(watch=False, dev=dev, release=release)
    index = 1 if webview else 0
    args = commands[index]
    entrypoint = str(
        (EXTENSION_WEBVIEW_ROOT / "graph.ts")
        if webview
        else (EXTENSION_HOST_ROOT / "extension.ts")
    )
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
    assert "--minify" in args
    assert "--sourcemap" not in args
    assert not _test_env_defines(extension_args)


def test_bundle_modes_minify_production_and_only_map_development() -> None:
    for args in (
        esbuild.extension_args(watch=False, dev=False),
        esbuild.webview_args(watch=False, dev=False),
    ):
        assert "--minify" in args
        assert "--sourcemap" not in args

    for args in (
        esbuild.extension_args(watch=False, dev=True),
        esbuild.webview_args(watch=False, dev=True),
    ):
        assert "--sourcemap" in args
        assert "--minify" not in args


def test_regular_compile_preserves_maps_but_release_compile_minifies() -> None:
    scripts = json.loads((REPO_ROOT / "package.json").read_text(encoding="utf-8"))[
        "scripts"
    ]
    assert scripts["compile:code"].endswith("scripts/build/esbuild.py --dev")
    assert scripts["compile:code:release"].endswith(
        "scripts/build/esbuild.py --release"
    )
    assert "--dev" not in scripts["compile:code:release"]


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
    assert "--minify" in args
    assert "--sourcemap" not in args


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
    extension = " ".join(commands[0])
    assert "--watch" in webview
    assert "--sourcemap" in extension
    assert "--minify" not in extension
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
    assert "--sourcemap" in commands[0]
    assert "--minify" not in commands[0]
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


@pytest.mark.parametrize("webview", [False, True], ids=["extension", "webview"])
def test_bundle_mode_controls_source_map_output(tmp_path: Path, webview: bool) -> None:
    binary = esbuild.esbuild_bin()
    if not binary.is_file():
        if os.environ.get("CI"):
            pytest.fail("esbuild is not installed in CI; real-esbuild smoke must run")
        pytest.skip("esbuild is not installed")

    source = tmp_path / "bundle.ts"
    source.write_text("export const answer = 40 + 2;\n", encoding="utf-8")
    production = tmp_path / "production.js"
    development = tmp_path / "development.js"

    for dev, outfile in ((False, production), (True, development)):
        result = subprocess.run(
            _esbuild_test_args(source, outfile, dev=dev, webview=webview),
            check=False,
            capture_output=True,
            text=True,
        )
        assert result.returncode == 0, result.stderr

    assert not production.with_suffix(".js.map").exists()
    assert development.with_suffix(".js.map").is_file()
    production_js = production.read_text(encoding="utf-8")
    development_js = development.read_text(encoding="utf-8")
    assert "sourceMappingURL" not in production_js
    assert len(production_js) < len(development_js)
