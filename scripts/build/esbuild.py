from __future__ import annotations

import contextlib
import shutil
import subprocess
import sys
import time
from pathlib import Path

from paths import (
    EXTENSION_DIST_ROOT,
    EXTENSION_HOST_ROOT,
    EXTENSION_WEBVIEW_ROOT,
    REPO_ROOT,
)


def _node() -> str:
    found = shutil.which("node")
    if found is None:
        raise RuntimeError("node is not on PATH")
    return found


def esbuild_bin() -> Path:
    return REPO_ROOT / "node_modules" / "esbuild" / "bin" / "esbuild"


def extension_args(*, watch: bool, release: bool = False) -> list[str]:
    outfile = EXTENSION_DIST_ROOT / "bin" / "client" / "extension" / "extension.js"
    args = [
        str(esbuild_bin()),
        str(EXTENSION_HOST_ROOT / "extension.ts"),
        "--bundle",
        f"--outfile={outfile}",
        "--platform=node",
        "--format=cjs",
        "--target=node18",
        "--external:vscode",
        "--sourcemap",
        "--log-level=info",
    ]
    if release:
        args.extend(
            [
                "--define:process.env.CWTOOLS_TEST_HOI4_REPO=undefined",
                "--define:process.env.CWTOOLS_TEST_HOI4_REF=undefined",
                "--define:process.env.CWTOOLS_TEST_RULES_MANIFEST_URL=undefined",
                "--define:process.env.CWTOOLS_TEST_RULES_FOLDER=undefined",
            ]
        )
    if watch:
        args.append("--watch")
    return args


def webview_args(*, watch: bool, dev: bool) -> list[str]:
    node_env = "development" if dev else "production"
    args = [
        str(esbuild_bin()),
        str(EXTENSION_WEBVIEW_ROOT / "graph.ts"),
        "--bundle",
        f"--outfile={EXTENSION_DIST_ROOT / 'bin' / 'client' / 'webview' / 'graph.js'}",
        "--platform=browser",
        "--format=iife",
        "--global-name=cwtoolsgraph",
        f'--define:process.env.NODE_ENV="{node_env}"',
        f'--banner:js=window.process = {{ env: {{ NODE_ENV: "{node_env}" }} }};',
        "--sourcemap",
        "--log-level=info",
    ]
    if watch:
        args.append("--watch")
    return args


def bundle_commands(
    *, watch: bool, dev: bool, release: bool = False
) -> list[list[str]]:
    commands = [
        extension_args(watch=watch, release=release),
        webview_args(watch=watch, dev=dev),
    ]
    if sys.platform == "win32":
        return [[_node(), *command] for command in commands]
    return commands


def _run(cmd: list[str]) -> None:
    print(f"> {' '.join(cmd)}")
    result = subprocess.run(cmd, cwd=REPO_ROOT, check=False)
    if result.returncode != 0:
        raise RuntimeError(f"command failed ({result.returncode}): {' '.join(cmd)}")


def wait_for_watcher_exit(
    procs: list[subprocess.Popen[bytes]], *, poll_interval: float = 0.05
) -> int:
    while True:
        codes = [proc.poll() for proc in procs]
        exited = [code for code in codes if code is not None]
        if exited:
            if all(code is not None for code in codes):
                return next((code for code in exited if code != 0), 0)
            return exited[0] if exited[0] != 0 else 1
        time.sleep(poll_interval)


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    watch = "--watch" in args
    dev = "--dev" in args or watch
    release = "--release" in args
    commands = bundle_commands(watch=watch, dev=dev, release=release)
    if not esbuild_bin().is_file():
        raise RuntimeError(f"esbuild not found at {esbuild_bin()}; run npm install")
    if not watch:
        for command in commands:
            _run(command)
        return 0

    with contextlib.ExitStack() as stack:
        procs = [
            stack.enter_context(subprocess.Popen(command, cwd=REPO_ROOT))
            for command in commands
        ]
        print("[esbuild] watching extension + webview...")
        try:
            status = wait_for_watcher_exit(procs)
        except KeyboardInterrupt:
            status = 130
        finally:
            for proc in procs:
                if proc.poll() is None:
                    proc.terminate()
    return status


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except RuntimeError as error:
        sys.stderr.write(f"{error}\n")
        raise SystemExit(1) from error
