from __future__ import annotations

import re

HEADING_RE = re.compile(r"^#+\s*\[?v?(\d+\.\d+\.\d+[^\]\s]*)\]?", re.MULTILINE)


def top_changelog_version(changelog: str) -> str:
    match = HEADING_RE.search(changelog)
    if not match:
        raise RuntimeError("could not find a version heading in CHANGELOG.md")
    return match.group(1)


def release_notes(changelog: str, version: str) -> str:
    lines = changelog.split("\n")
    start = next(
        (
            index
            for index, line in enumerate(lines)
            if (match := HEADING_RE.match(line)) and match.group(1) == version
        ),
        -1,
    )
    rest = [] if start == -1 else lines[start + 1 :]
    end = next(
        (index for index, line in enumerate(rest) if HEADING_RE.match(line)),
        -1,
    )
    notes = "\n".join(rest if end == -1 else rest[:end]).strip()
    if not notes:
        raise RuntimeError(
            f"no CHANGELOG section for version {version}; "
            "refusing to publish a release with auto-generated notes"
        )
    return notes
