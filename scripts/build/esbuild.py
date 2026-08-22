from __future__ import annotations

import shutil
import subprocess
import sys
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


def extension_args(*, watch: bool) -> list[str]:
    args = [
        str(esbuild_bin()),
        str(EXTENSION_HOST_ROOT / "extension.ts"),
        "--bundle",
        f"--outfile={EXTENSION_DIST_ROOT / 'bin' / 'client' / 'extension' / 'extension.js'}",
        "--platform=node",
        "--format=cjs",
        "--target=node18",
        "--external:vscode",
        "--sourcemap",
        "--log-level=info",
    ]
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
        '--banner:js=window.process = { env: { NODE_ENV: "production" } };',
        "--sourcemap",
        "--log-level=info",
    ]
    if watch:
        args.append("--watch")
    return args


def bundle_commands(*, watch: bool, dev: bool) -> list[list[str]]:
    return [
        [_node(), *extension_args(watch=watch)],
        [_node(), *webview_args(watch=watch, dev=dev)],
    ]


def _run(cmd: list[str]) -> None:
    print(f"> {' '.join(cmd)}")
    result = subprocess.run(cmd, cwd=REPO_ROOT, check=False)
    if result.returncode != 0:
        raise RuntimeError(f"command failed ({result.returncode}): {' '.join(cmd)}")


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    watch = "--watch" in args
    dev = "--dev" in args or watch
    commands = bundle_commands(watch=watch, dev=dev)
    if not esbuild_bin().is_file():
        raise RuntimeError(f"esbuild not found at {esbuild_bin()}; run npm install")
    if not watch:
        for command in commands:
            _run(command)
        return 0

    procs = [subprocess.Popen(command, cwd=REPO_ROOT) for command in commands]
    print("[esbuild] watching extension + webview...")
    status = 0
    try:
        for proc in procs:
            code = proc.wait()
            if code != 0 and status == 0:
                status = code
    except KeyboardInterrupt:
        status = 130
    finally:
        for proc in procs:
            if proc.poll() is None:
                proc.terminate()
        for proc in procs:
            proc.wait()
    return status


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except RuntimeError as error:
        sys.stderr.write(f"{error}\n")
        raise SystemExit(1) from error
