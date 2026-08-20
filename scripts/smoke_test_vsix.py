#!/usr/bin/env python3

from __future__ import annotations

import json
import re
import sys
import tempfile
import zipfile
from pathlib import Path

TARGET_TO_PLATFORM = {
    "win32-x64": "win-x64",
    "linux-x64": "linux-x64",
    "linux-arm64": "linux-arm64",
    "darwin-x64": "osx-x64",
    "darwin-arm64": "osx-arm64",
}
TARGET_RE = re.compile(r"^.*-((win32|linux|darwin|alpine)-[a-z0-9]+)-[0-9].*\.vsix$")


def gh_error(message: str) -> None:
    print(f"::error::{message}")


def platform_files(root: Path) -> dict[str, int]:
    base = root / "bin" / "server" / "cwtools-server"
    present: dict[str, int] = {}
    if not base.is_dir():
        return present
    for directory in sorted(p for p in base.iterdir() if p.is_dir()):
        count = sum(1 for path in directory.rglob("*") if path.is_file())
        present[directory.name] = count
    return present


def check_vsix(vsix: Path) -> tuple[str | None, set[str]]:
    print(f"Checking {vsix.name} ...")
    with tempfile.TemporaryDirectory() as tmp:
        workdir = Path(tmp)
        with zipfile.ZipFile(vsix) as zf:
            zf.extractall(workdir)
        root = workdir / "extension"
        package_json = root / "package.json"
        try:
            json.loads(package_json.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            gh_error(f"{vsix.name}: extension package.json is not valid JSON")
            raise SystemExit(1) from None
        entry = root / "bin" / "client" / "extension" / "extension.js"
        if not entry.is_file():
            gh_error(f"{vsix.name}: missing extension entrypoint bin/client/extension/extension.js")
            raise SystemExit(1)

        present = platform_files(root)
        if not present:
            gh_error(f"{vsix.name}: no server binaries at all")
            raise SystemExit(1)
        for platform, count in present.items():
            if count == 0:
                gh_error(f"{vsix.name}: no files in server binary directory {platform}")
                raise SystemExit(1)

        match = TARGET_RE.match(vsix.name)
        target = match.group(1) if match else ""
        platform = TARGET_TO_PLATFORM.get(target, "")
        names = set(present)
        if platform:
            if names != {platform}:
                carried = " ".join(sorted(names))
                gh_error(
                    f"{vsix.name}: targets {target} but carries [{carried}] instead of just {platform}"
                )
                raise SystemExit(1)
            print(f"  {target}: {platform} only, OK")
            return platform, names
        carried = " ".join(sorted(names))
        print(f"  universal: [{carried}] OK")
        return None, names


def main(argv: list[str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if not argv:
        print(f"Usage: {Path(sys.argv[0]).name} <vsix-dir> [expected-platform ...]")
        return 1

    vsix_dir = Path(argv[0])
    expected = argv[1:]
    vsixes = sorted(vsix_dir.glob("*.vsix"))
    if not vsixes:
        gh_error(f"No .vsix file found in {vsix_dir}")
        return 1

    universal: set[str] = set()
    targeted: set[str] = set()
    for vsix in vsixes:
        platform, names = check_vsix(vsix)
        if platform is None:
            universal = names
        else:
            targeted.add(platform)

    if universal:
        missing = targeted - universal
        for platform in sorted(missing):
            gh_error(f"universal vsix is missing {platform}, which has its own package")
            return 1

    packaged = universal | targeted
    for platform in expected:
        if platform not in packaged:
            gh_error(f"expected platform {platform} was not packaged")
            return 1

    print(f"Smoke test passed ({len(vsixes)} vsix)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
