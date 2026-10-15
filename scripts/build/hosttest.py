from __future__ import annotations

import os
import shutil
import subprocess
import sys
from argparse import ArgumentParser
from typing import NamedTuple

from executables import require_executable
from paths import REPO_ROOT

TEST_CLI = REPO_ROOT / "node_modules" / "@vscode" / "test-cli" / "out" / "bin.mjs"

BACKENDS = ("xvfb", "ozone", "native")
DEFAULT_LABEL = "smoke"

XVFB_MISSING = """\
xvfb-run is not on PATH. The extension-host suites run under a virtual display
so they do not open a VS Code window on your desktop.

  Debian/Ubuntu  sudo apt install xvfb
  Fedora         sudo dnf install xorg-x11-server-Xvfb
  Arch           sudo pacman -S xorg-server-xvfb

Or pick another backend:
  npm run test:native          an explicit visible window for local debugging
  CWTOOLS_TEST_DISPLAY=ozone   Electron's headless Ozone backend, no package\
"""
DISPLAY_ENV_VARS = (
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "WAYLAND_SOCKET",
    "XAUTHORITY",
    "ELECTRON_OZONE_PLATFORM_HINT",
)


class Display(NamedTuple):
    name: str
    prefix: list[str]
    note: str | None

    def env(self) -> dict[str, str]:
        env = {
            **os.environ,
            "CWTOOLS_TEST_DISPLAY": self.name,
            "CWTOOLS_TEST_LAUNCHER": "hosttest",
            "CWTOOLS_TEST_NATIVE_DEBUG": "1" if self.name == "native" else "0",
        }
        env.pop("CWTOOLS_TEST_CONFIG_DISCOVERY", None)
        if self.name != "native":
            for name in DISPLAY_ENV_VARS:
                env.pop(name, None)
        return env


def in_ci() -> bool:
    values = os.environ
    false_values = {"", "0", "false", "no", "off"}
    return any(
        values.get(key, "").strip().lower() not in false_values
        for key in ("CI", "GITHUB_ACTIONS")
    )


def resolve_display(*, native: bool = False, platform: str | None = None) -> Display:
    system = sys.platform if platform is None else platform
    choice = "native" if native else os.environ.get("CWTOOLS_TEST_DISPLAY", "").strip()
    if choice and choice not in BACKENDS:
        raise RuntimeError(
            f"CWTOOLS_TEST_DISPLAY={choice} is not one of {', '.join(BACKENDS)}"
        )
    if choice == "native" and not native:
        raise RuntimeError(
            "CWTOOLS_TEST_DISPLAY=native is not an automatic test backend; "
            "use `npm run test:native` for explicit local debugging"
        )
    if native:
        if in_ci():
            raise RuntimeError(
                "--native is disabled in CI; host tests must stay headless"
            )
        return Display("native", [], None)
    if choice in {"xvfb", "ozone"} and not system.startswith("linux"):
        raise RuntimeError(
            f"the {choice} headless backend is only supported on Linux, not {system}"
        )
    if choice == "ozone":
        return Display("ozone", [], None)
    if choice == "xvfb" or system.startswith("linux"):
        xvfb_run = shutil.which("xvfb-run")
        if xvfb_run is None:
            raise RuntimeError(XVFB_MISSING)
        # -a picks a free server number, so concurrent runs do not collide.
        return Display("xvfb", [xvfb_run, "-a"], None)
    raise RuntimeError(
        f"no headless extension-host backend is configured for {system}; "
        "automated host tests currently require Linux with Xvfb. "
        "Use `npm run test:native` only for explicit local debugging"
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
    command = [require_executable("node"), str(TEST_CLI)]
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
