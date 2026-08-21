#!/usr/bin/env python3

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import NoReturn

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent
SHARED_DEPS = ("tempfile", "assert_cmd", "predicates")
DEP_LINE = re.compile(r"^(tempfile|assert_cmd|predicates)\s*=")
DEP_WORKSPACE = re.compile(r"^(tempfile|assert_cmd|predicates) = \{ workspace = true")


def die(message: str) -> NoReturn:
    print(f"workspace-manifest-check: {message}", file=sys.stderr)
    raise SystemExit(2)


def fail(message: str) -> NoReturn:
    print(f"workspace-manifest-check: {message}", file=sys.stderr)
    raise SystemExit(1)


def has_lints_workspace(text: str) -> bool:
    in_lints = False
    for line in text.splitlines():
        if line == "[lints]":
            in_lints = True
            continue
        if line.startswith("["):
            in_lints = False
            continue
        if in_lints and line == "workspace = true":
            return True
    return False


def main() -> int:
    ws_root = Path(os.environ.get("CWTOOLS_RS") or (REPO_ROOT / "engine"))
    crates_dir = ws_root / "crates"
    if not crates_dir.is_dir():
        die(f"crates dir not found: {crates_dir}")
    root_manifest = ws_root / "Cargo.toml"
    if not root_manifest.is_file():
        die(f"workspace Cargo.toml not found: {root_manifest}")
    if shutil.which("cargo") is None:
        die("cargo not on PATH")

    errors = 0

    def note(message: str) -> None:
        nonlocal errors
        print(f"workspace-manifest-check: {message}", file=sys.stderr)
        errors += 1

    for manifest in sorted(crates_dir.glob("*/Cargo.toml")):
        crate = manifest.parent.name
        text = manifest.read_text(encoding="utf-8")
        if not re.search(r"^repository\.workspace = true$", text, re.MULTILINE):
            note(f"{crate}: missing repository.workspace = true")
        if not has_lints_workspace(text):
            note(f"{crate}: missing [lints] workspace = true")
        if DEP_LINE.search(text) and not DEP_WORKSPACE.search(text):
            note(
                f"{crate}: tempfile/assert_cmd/predicates must use {{ workspace = true }}"
            )

    meta = subprocess.run(
        [
            "cargo",
            "metadata",
            "--manifest-path",
            str(root_manifest),
            "--format-version",
            "1",
            "--no-deps",
        ],
        capture_output=True,
        text=True,
    )
    if meta.returncode != 0:
        die("cargo metadata failed")
    try:
        data = json.loads(meta.stdout)
    except json.JSONDecodeError as exc:
        die(f"cargo metadata returned invalid JSON: {exc}")
    for package in data.get("packages", []):
        if package.get("source") is None and package.get("repository") is None:
            note(f"cargo metadata: {package.get('name')} has repository=null")

    root_text = root_manifest.read_text(encoding="utf-8")
    for dep in SHARED_DEPS:
        if not re.search(rf"^{re.escape(dep)} = ", root_text, re.MULTILINE):
            note(f"workspace Cargo.toml missing [workspace.dependencies] {dep}")
    if "[workspace.lints.rust]" not in root_text:
        note("workspace Cargo.toml missing [workspace.lints.rust]")
    if "[workspace.lints.clippy]" not in root_text:
        note("workspace Cargo.toml missing [workspace.lints.clippy]")

    if errors:
        fail(f"{errors} inheritance check(s) failed")
    print("workspace-manifest-check: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
