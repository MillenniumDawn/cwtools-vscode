#!/usr/bin/env python3
"""Resolve the guard baseline's corpus/rules revision pins to full SHAs.

`scripts/md-baseline.csv` pins the Millennium Dawn tier's corpus and rules
checkouts in its `#`-commented header (`# corpus: <name> @ <sha>` / `# rules:
<name> @ <sha>`). CI's `millennium-dawn` job needs full 40-character SHAs to
check those revisions out; git cannot fetch an abbreviated one. This script
reads the pins, resolves each to a full SHA via `gh api`, and writes
`md_rev`/`rules_rev` to `$GITHUB_OUTPUT` for that job to consume.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
from collections.abc import Callable, Mapping
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
MD_BASELINE = SCRIPT_DIR / "md-baseline.csv"

# Anchored on end-of-line so a `(dirty)` suffix does not match. A baseline
# blessed against a modified checkout pins nothing CI can reproduce, and
# failing here says so instead of reporting the drift as a code change later.
PIN_RE_TEMPLATE = r"^# {label}:[ \t]*[^@]*@ ([0-9a-f]{{7,40}})$"
SHA_RE = re.compile(r"^[0-9a-f]{40}$")

GhApi = Callable[[str, str], str]


class PinError(Exception):
    """A baseline pin is missing, dirty, or did not resolve to a full SHA."""

    def __init__(self, annotation: str, detail: str | None = None) -> None:
        super().__init__(annotation)
        self.annotation = annotation
        self.detail = detail


def parse_pin(baseline_text: str, label: str) -> str | None:
    """Return the pinned revision for `label` (corpus/rules), or None."""
    pattern = re.compile(PIN_RE_TEMPLATE.format(label=re.escape(label)), re.MULTILINE)
    match = pattern.search(baseline_text)
    return match.group(1) if match else None


def header_lines(baseline_text: str) -> str:
    return "\n".join(
        line for line in baseline_text.splitlines() if line.startswith("#")
    )


def check_sha(label: str, sha: str) -> None:
    """Raise PinError unless `sha` is a full 40-character hex SHA."""
    if not SHA_RE.fullmatch(sha):
        raise PinError(f"could not resolve the {label} pin to a full SHA (got '{sha}')")


def resolve_pins(
    baseline_text: str,
    baseline_path: Path,
    md_repo: str,
    rules_repo: str,
    gh_api: GhApi,
) -> tuple[str, str, list[str]]:
    """Resolve the md-baseline's pins to full SHAs via `gh_api`.

    Returns (md_sha, rules_sha, log_lines). Raises PinError on a missing,
    dirty, or unresolvable pin.
    """
    corpus_pin = parse_pin(baseline_text, "corpus")
    rules_pin = parse_pin(baseline_text, "rules")
    if not corpus_pin or not rules_pin:
        raise PinError(
            f"no clean revision pin in {baseline_path} (a '(dirty)' pin is "
            "not reproducible; re-bless from a clean checkout)",
            detail=header_lines(baseline_text),
        )

    lines = [f"md baseline pins: corpus {corpus_pin}, rules {rules_pin}"]

    md_sha = gh_api(md_repo, corpus_pin)
    check_sha("Millennium Dawn corpus", md_sha)
    rules_sha = gh_api(rules_repo, rules_pin)
    check_sha("Millennium Dawn ruleset", rules_sha)

    lines.append(f"resolved: md {md_sha}, rules {rules_sha}")
    return md_sha, rules_sha, lines


def gh_api_commit_sha(repo: str, pin: str) -> str:
    """Resolve `pin` to a commit SHA in `repo` via the `gh` CLI."""
    result = subprocess.run(
        ["gh", "api", f"repos/{repo}/commits/{pin}", "--jq", ".sha"],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"gh api repos/{repo}/commits/{pin} failed: {result.stderr.strip()}"
        )
    return result.stdout.strip()


def main(env: Mapping[str, str] | None = None) -> int:
    environ = os.environ if env is None else env
    md_repo = environ.get("MD_REPO")
    rules_repo = environ.get("RULES_REPO")
    github_output = environ.get("GITHUB_OUTPUT")
    if not md_repo or not rules_repo:
        print("resolve_pins: MD_REPO and RULES_REPO must be set", file=sys.stderr)
        return 1
    if not github_output:
        print("resolve_pins: GITHUB_OUTPUT must be set", file=sys.stderr)
        return 1

    baseline_text = MD_BASELINE.read_text(encoding="utf-8")

    try:
        md_sha, rules_sha, lines = resolve_pins(
            baseline_text, MD_BASELINE, md_repo, rules_repo, gh_api_commit_sha
        )
    except PinError as error:
        print(f"::error::{error.annotation}")
        if error.detail:
            print(error.detail, file=sys.stderr)
        return 1
    except RuntimeError as error:
        print(f"resolve_pins: {error}", file=sys.stderr)
        return 1

    for line in lines:
        print(line)

    with Path(github_output).open("a", encoding="utf-8") as output:
        output.write(f"md_rev={md_sha}\n")
        output.write(f"rules_rev={rules_sha}\n")

    return 0


if __name__ == "__main__":
    sys.exit(main())
