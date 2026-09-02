from __future__ import annotations

import subprocess
import sys
from typing import cast

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


def test_extension_bundle_is_node_cjs() -> None:
    args = " ".join(esbuild.extension_args(watch=False))
    assert "--platform=node" in args
    assert "--format=cjs" in args
    assert "--external:vscode" in args
    assert "extension.ts" in args
    assert "--watch" not in args


def test_webview_bundle_is_browser_iife() -> None:
    args = " ".join(esbuild.webview_args(watch=False, dev=False))
    assert "--platform=browser" in args
    assert "--format=iife" in args
    assert "--global-name=cwtoolsgraph" in args
    assert 'process.env.NODE_ENV="production"' in args
    assert "graph.ts" in args


def test_dev_and_watch_flags() -> None:
    webview = " ".join(esbuild.webview_args(watch=True, dev=True))
    assert 'process.env.NODE_ENV="development"' in webview
    assert 'window.process = { env: { NODE_ENV: "development" } };' in webview
    assert "--watch" in webview
    assert "--watch" in " ".join(esbuild.extension_args(watch=True))


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
