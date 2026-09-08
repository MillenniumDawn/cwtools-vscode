#!/usr/bin/env python3

# Prepares the release commit the release PR carries: promote the changelog's
# `### Unreleased` section to a version heading and move the extension manifest
# to the same version. Everything here is a pure function over strings so the
# tests need no git repo; main() is the only part that touches disk.

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from build import next_release_version
from changelog import HEADING_RE, top_changelog_version
from paths import REPO_ROOT

BUMPS = ("patch", "minor", "major")
UNRELEASED = "### Unreleased"


def unreleased_section(changelog: str) -> list[str]:
    lines = changelog.split("\n")
    start = next(
        (
            index
            for index, line in enumerate(lines)
            if line.strip().lower() == UNRELEASED.lower()
        ),
        -1,
    )
    if start == -1:
        return []
    rest = lines[start + 1 :]
    end = next(
        (index for index, line in enumerate(rest) if HEADING_RE.match(line)),
        len(rest),
    )
    return rest[:end]


def has_release_content(changelog: str) -> bool:
    return any(
        line.lstrip().startswith(("*", "-")) for line in unreleased_section(changelog)
    )


def promote_unreleased(changelog: str, version: str) -> str:
    lines = changelog.split("\n")
    for index, line in enumerate(lines):
        if line.strip().lower() == UNRELEASED.lower():
            lines[index : index + 1] = [UNRELEASED, "", f"### {version}"]
            return "\n".join(lines)
    raise RuntimeError(f"CHANGELOG.md has no '{UNRELEASED}' heading to promote")


def bump_manifest(manifest_text: str, version: str) -> str:
    manifest = json.loads(manifest_text)
    if not isinstance(manifest, dict):
        raise TypeError("extension manifest is not a JSON object")
    manifest["version"] = version
    return json.dumps(manifest, indent=2) + "\n"


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Promote the changelog's Unreleased section to a release."
    )
    parser.add_argument("--bump", choices=BUMPS, default="patch")
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="report the version without writing any file",
    )
    parser.add_argument(
        "--repo-root",
        type=Path,
        default=REPO_ROOT,
        help="repository to read and rewrite (default: this checkout)",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    repo_root = Path(args.repo_root)
    changelog_path = repo_root / "CHANGELOG.md"
    manifest_path = repo_root / "extension" / "package" / "package.json"

    changelog = changelog_path.read_text(encoding="utf-8")
    if not has_release_content(changelog):
        # The push that merges the release PR itself lands an empty Unreleased
        # section, so this is the workflow's own no-op guard, not an error.
        print(
            f"No bullets under '{UNRELEASED}' in {changelog_path}; nothing to release.",
            file=sys.stderr,
        )
        print("release=false")
        return 0

    version = next_release_version(top_changelog_version(changelog), args.bump)
    if not args.dry_run:
        changelog_path.write_text(
            promote_unreleased(changelog, version), encoding="utf-8"
        )
        manifest_path.write_text(
            bump_manifest(manifest_path.read_text(encoding="utf-8"), version),
            encoding="utf-8",
        )
        print(f"promoted {UNRELEASED} to {version}", file=sys.stderr)
    else:
        print(f"dry run: would release {version}", file=sys.stderr)
    print("release=true")
    print(f"version={version}")
    print(f"bump={args.bump}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, TypeError, OSError, json.JSONDecodeError) as error:
        sys.stderr.write(f"{error}\n")
        raise SystemExit(1) from error
