#!/usr/bin/env python3

from __future__ import annotations

import os
import shutil
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent
OWNED_GRAMMAR = "cwt.tmLanguage.json"


def main() -> int:
    src = Path(os.environ.get("PARADOX_SYNTAX_SRC") or "../paradox-syntax")
    syntaxes = src / "syntaxes"
    dst = Path(
        os.environ.get("PARADOX_SYNTAX_DST")
        or (REPO_ROOT / "extension" / "package" / "syntaxes")
    )
    if not syntaxes.is_dir():
        print(f"error: '{syntaxes}' not found.", file=sys.stderr)
        print(
            "       Clone the upstream repo next to this one, or set PARADOX_SYNTAX_SRC.",
            file=sys.stderr,
        )
        return 1

    dst.mkdir(parents=True, exist_ok=True)
    copied: list[str] = []
    for src_file in sorted(syntaxes.glob("*.tmLanguage.json")):
        if src_file.name == OWNED_GRAMMAR:
            continue
        shutil.copy2(src_file, dst / src_file.name)
        copied.append(src_file.name)

    print(f"Updated {len(copied)} grammar file(s) in extension/package/syntaxes/:")
    for name in copied:
        print(f"  {name}")
    print()
    print("Reminder: do not add 'tboby.paradox-syntax' to extensionPack again.")
    print("this repo ships its own grammars. Themes live in extension/package/themes/")
    print("and are owned here, not mirrored.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
