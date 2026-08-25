from __future__ import annotations

import os
import shutil
import subprocess
import sys
from argparse import ArgumentParser
from typing import NamedTuple

from paths import REPO_ROOT

TEST_CLI = REPO_ROOT / "node_modules" / "@vscode" / "test-cli" / "out" / "bin.mjs"

BACKENDS = ("xvfb", "ozone", "native")
DEFAULT_LABEL = "unit"

XVFB_MISSING = """\
xvfb-run is not on PATH. The extension-host suites run under a virtual display
so they do not open a VS Code window on your desktop.

  Debian/Ubuntu  sudo apt install xvfb
  Fedora         sudo dnf install xorg-x11-server-Xvfb
  Arch           sudo pacman -S xorg-server-xvfb

Or pick another backend:
  npm run test:native          a visible window, on purpose
  CWTOOLS_TEST_DISPLAY=ozone   Electron's headless Ozone backend, no package\
"""


class Display(NamedTuple):
    name: str
    prefix: list[str]
    note: str | None

    def env(self) -> dict[str, str]:
        return {**os.environ, "CWTOOLS_TEST_DISPLAY": self.name}


def resolve_display(*, native: bool = False, platform: str | None = None) -> Display:
    system = sys.platform if platform is None else platform
    choice = "native" if native else os.environ.get("CWTOOLS_TEST_DISPLAY", "").strip()
    if choice and choice not in BACKENDS:
        raise RuntimeError(
            f"CWTOOLS_TEST_DISPLAY={choice} is not one of {', '.join(BACKENDS)}"
        )
    if choice == "native":
        return Display("native", [], None)
    if choice == "ozone":
        return Display("ozone", [], None)
    if choice == "xvfb" or system.startswith("linux"):
        xvfb_run = shutil.which("xvfb-run")
        if xvfb_run is None:
            raise RuntimeError(XVFB_MISSING)
        # -a picks a free server number, so concurrent runs do not collide.
        return Display("xvfb", [xvfb_run, "-a"], None)
    return Display(
        "native",
        [],
        f"no virtual display backend on {system}, so VS Code opens a window. "
        "Only Linux has one today (MillenniumDawn/cwtools-vscode#406).",
    )


def test_cli_command(
    labels: list[str],
    *,
    coverage: bool = False,
    watch: bool = False,
    extra: list[str] | None = None,
) -> list[str]:
    if not TEST_CLI.is_file():
        raise RuntimeError(f"{TEST_CLI} is missing. Run npm ci first.")
    node = shutil.which("node")
    if node is None:
        raise RuntimeError("node is not on PATH")
    command = [node, str(TEST_CLI)]
    for label in labels:
        command += ["--label", label]
    if coverage:
        command.append("--coverage")
    if watch:
        command.append("-w")
    return command + list(extra or [])


def main(argv: list[str] | None = None) -> int:
    parser = ArgumentParser(
        prog="hosttest.py",
        description=(
            "Run a @vscode/test-cli label without opening a window on the "
            "developer's desktop. Unrecognized arguments pass through."
        ),
    )
    parser.add_argument("--label", action="append", dest="labels")
    parser.add_argument("--coverage", action="store_true")
    parser.add_argument("-w", "--watch", action="store_true")
    parser.add_argument("--native", action="store_true")
    args, extra = parser.parse_known_args(argv)

    display = resolve_display(native=args.native)
    if display.note is not None:
        sys.stderr.write(f"{display.note}\n")
    command = display.prefix + test_cli_command(
        args.labels or [DEFAULT_LABEL],
        coverage=args.coverage,
        watch=args.watch,
        extra=extra,
    )
    result = subprocess.run(command, cwd=REPO_ROOT, check=False, env=display.env())
    return result.returncode


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except RuntimeError as error:
        sys.stderr.write(f"{error}\n")
        raise SystemExit(1) from error
