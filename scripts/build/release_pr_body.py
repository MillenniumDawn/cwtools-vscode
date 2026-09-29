#!/usr/bin/env python3

# Writes the release PR's body: what merging it does, and every pull request
# merged since the last stable tag, so each one is linked from the release.
# The PRs come from the commits API rather than the `(#N)` in a squash subject,
# which also covers a merge commit. render_body() is pure; the git and gh calls
# are the only part that needs a checkout and a token.

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass

from build import run_capture

RELEASE_BRANCH = "release/version-bump"
DEPENDABOT = "dependabot[bot]"


@dataclass(frozen=True)
class PullRequest:
    number: int
    title: str


def last_release_tag(head: str) -> str:
    # Pre-release tags (v3.5.73-pre.1) sit between the stable ones.
    return run_capture(
        "git",
        [
            "describe",
            "--tags",
            "--abbrev=0",
            "--match",
            "v[0-9]*",
            "--exclude",
            "*-pre.*",
            head,
        ],
    ).strip()


def _pulls_for(repo: str, sha: str) -> list[dict[str, object]]:
    try:
        output = run_capture("gh", ["api", f"repos/{repo}/commits/{sha}/pulls"])
    except RuntimeError as error:
        # A flaky API costs one line of the list, never the release PR.
        print(f"::warning::no pull requests for {sha}: {error}", file=sys.stderr)
        return []
    pulls = json.loads(output)
    return pulls if isinstance(pulls, list) else []


def _listed(pull: dict[str, object]) -> bool:
    user = pull.get("user")
    head = pull.get("head")
    login = user.get("login") if isinstance(user, dict) else None
    ref = head.get("ref") if isinstance(head, dict) else None
    # Dependabot bumps carry no changelog bullet, and a release PR whose tag
    # is not created yet is the previous release, not part of this one.
    return login != DEPENDABOT and ref != RELEASE_BRANCH


def merged_pull_requests(repo: str, tag: str, head: str) -> list[PullRequest]:
    shas = run_capture(
        "git", ["log", f"{tag}..{head}", "--first-parent", "--reverse", "--pretty=%H"]
    ).split()
    seen: set[int] = set()
    pulls: list[PullRequest] = []
    for sha in shas:
        for pull in _pulls_for(repo, sha):
            number = pull.get("number")
            if not isinstance(number, int) or number in seen or not _listed(pull):
                continue
            seen.add(number)
            pulls.append(
                PullRequest(number, " ".join(str(pull.get("title", "")).split()))
            )
    return pulls


def render_body(version: str, bump: str, pulls: list[PullRequest]) -> str:
    listed = [f"- #{pull.number} {pull.title}".rstrip() for pull in pulls]
    return "\n".join(
        [
            "Promotes the changelog's `### Unreleased` section to",
            f"`### {version}` and moves",
            "`extension/package/package.json` to the same version.",
            "",
            "Merging this PR cuts the release: the `Publish` workflow",
            "builds every platform, smoke-tests the VSIX files, and",
            "publishes them to the VS Code Marketplace, Open VSX, and",
            "GitHub Releases as three independent jobs. The",
            f"`v{version}` tag is created by the GitHub one, so a release",
            "that fails to publish is retried by the next push to main.",
            "",
            "This branch is regenerated from `main` on every push, so",
            "edits made here are discarded. Correct the release notes in",
            "`main`'s `### Unreleased` section instead.",
            "",
            f"Bump: `{bump}`. Re-run `release-pr.yml` with a different",
            "`release_type` to change it.",
            "",
            "### Pull requests in this release",
            "",
            *(listed or ["- None found."]),
            "",
        ]
    )


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Write the release PR's body.")
    parser.add_argument("--version", required=True)
    parser.add_argument("--bump", required=True)
    parser.add_argument("--repo", required=True, help="owner/name on GitHub")
    parser.add_argument(
        "--head",
        default="origin/main",
        help="where the release range ends (default: origin/main)",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    tag = last_release_tag(args.head)
    pulls = merged_pull_requests(args.repo, tag, args.head)
    sys.stdout.write(render_body(args.version, args.bump, pulls))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, json.JSONDecodeError) as error:
        sys.stderr.write(f"{error}\n")
        raise SystemExit(1) from error
