#!/usr/bin/env python3

from __future__ import annotations

import os
import shutil
import stat
import sys
from pathlib import Path


def main(argv: list[str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if len(argv) != 2:
        print(
            f"Usage: {Path(sys.argv[0]).name} <artifacts-dir> <release-dir>",
            file=sys.stderr,
        )
        return 1

    artifacts_dir = Path(argv[0])
    release_dir = Path(argv[1])
    base = release_dir / "bin" / "server" / "cwtools-server"

    found = False
    for directory in sorted(artifacts_dir.glob("server-*")):
        if not directory.is_dir():
            continue
        platform = directory.name[len("server-") :]
        print(f"Staging {platform} ...")
        dest = base / platform
        dest.mkdir(parents=True, exist_ok=True)
        for src in directory.iterdir():
            if not src.is_file():
                continue
            shutil.copy2(src, dest / src.name)
            found = True

    if not found:
        print(f"Error: no server-* artifacts found in {artifacts_dir}", file=sys.stderr)
        return 1

    if os.name != "nt":
        for path in base.glob("*/*"):
            if path.is_file():
                path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    print("Staged platforms:")
    for path in sorted(base.rglob("*")):
        print(path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
