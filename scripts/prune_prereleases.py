#!/usr/bin/env python3

# Keeps the most recent automated pre-release GitHub releases and their tags.
# The default is intentionally report-only; the scheduled workflow opts into
# deletion explicitly, while a manual dispatch remains a dry run unless its
# confirmation input is selected.

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections.abc import Iterable
from dataclasses import dataclass
from datetime import datetime

# Thirty keeps roughly a week of several-per-day builds for rollback while
# bounding the unexpired release assets and tags.
KEEP_COUNT = 30
# Publish now creates v<major>.<minor>.<run>-pre.<attempt>. The former nightly
# workflow used v*-nightly.* and is no longer the owner of automated releases.
WORKFLOW_PRERELEASE_TAG = re.compile(r"^v\d+\.\d+\.\d+-pre\.\d+$")


@dataclass(frozen=True)
class Release:
    tag: str
    published_at: str
    is_prerelease: bool


def parse_releases(payload: str) -> list[Release]:
    """Parse the JSON returned by ``gh release list``."""
    raw_releases: object = json.loads(payload)
    if not isinstance(raw_releases, list):
        raise ValueError("gh release list returned a non-list JSON value")

    releases: list[Release] = []
    for raw_release in raw_releases:
        if not isinstance(raw_release, dict):
            raise ValueError("gh release list returned a non-object release")
        tag = raw_release.get("tagName")
        published_at = raw_release.get("publishedAt")
        is_prerelease = raw_release.get("isPrerelease")
        if not isinstance(tag, str) or not isinstance(is_prerelease, bool):
            raise ValueError("gh release list returned an invalid release shape")
        # Drafts do not have a publication timestamp and cannot be part of a
        # keep-last-N policy. They are left untouched, even if their tag fits.
        if published_at is None:
            continue
        if not isinstance(published_at, str):
            raise ValueError("gh release list returned an invalid publication date")
        releases.append(Release(tag, published_at, is_prerelease))
    return releases


def is_workflow_prerelease(release: Release) -> bool:
    """Return whether a release is a published automated pre-release."""
    return (
        release.is_prerelease
        and WORKFLOW_PRERELEASE_TAG.fullmatch(release.tag) is not None
    )


def published_timestamp(release: Release) -> float:
    """Convert GitHub's ISO-8601 publication timestamp for chronological sorting."""
    try:
        published = datetime.fromisoformat(release.published_at.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError(
            f"invalid publication date for {release.tag}: {release.published_at}"
        ) from error
    if published.tzinfo is None:
        raise ValueError(f"publication date has no timezone for {release.tag}")
    return published.timestamp()


def select_releases(
    releases: Iterable[Release], keep_count: int = KEEP_COUNT
) -> tuple[list[Release], list[Release]]:
    """Return (kept, stale) workflow-owned releases, newest first."""
    if keep_count < 0:
        raise ValueError("keep count must not be negative")
    candidates = [release for release in releases if is_workflow_prerelease(release)]
    candidates.sort(key=published_timestamp, reverse=True)
    return candidates[:keep_count], candidates[keep_count:]


def list_releases() -> list[Release]:
    result = subprocess.run(
        [
            "gh",
            "release",
            "list",
            "--limit",
            "1000",
            "--json",
            "tagName,publishedAt,isPrerelease",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    return parse_releases(result.stdout)


def delete_release(tag: str) -> None:
    # --cleanup-tag deletes the release first and then its tag. Keeping this as
    # one explicit command avoids ever deleting a tag that is not a release.
    subprocess.run(
        ["gh", "release", "delete", tag, "--cleanup-tag", "--yes"],
        check=True,
    )


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Prune old automated GitHub pre-release releases."
    )
    parser.add_argument(
        "--keep",
        type=int,
        default=KEEP_COUNT,
        help=f"number of newest automated pre-releases to keep (default: {KEEP_COUNT})",
    )
    parser.add_argument(
        "--delete",
        action="store_true",
        help="delete stale releases and their tags (default: dry run)",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    releases = list_releases()
    kept, stale = select_releases(releases, args.keep)
    candidate_count = sum(is_workflow_prerelease(release) for release in releases)

    mode = "Deleting" if args.delete else "Dry run"
    print(
        f"{mode}: found {candidate_count} workflow-owned pre-releases; "
        f"keeping {len(kept)} and pruning {len(stale)} (keep {args.keep})."
    )
    for release in kept:
        print(f"KEEP {release.tag} ({release.published_at})")
    for release in stale:
        if args.delete:
            delete_release(release.tag)
            print(f"DELETE {release.tag} ({release.published_at})")
        else:
            print(f"WOULD DELETE {release.tag} ({release.published_at})")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        raise SystemExit(1) from error
