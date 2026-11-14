"""Fail-closed CI prerequisite for publication."""

from __future__ import annotations

import json
import os
import re
import sys
from datetime import datetime
from typing import Any

_SHA = re.compile(r"[0-9a-f]{40}\Z")


def _parse_run(item: Any) -> tuple[datetime, int, dict[str, Any]]:
    if not isinstance(item, dict):
        raise ValueError("invalid run")
    run_id = item.get("id")
    timestamp = item.get("created_at")
    if (
        not isinstance(run_id, int)
        or isinstance(run_id, bool)
        or not isinstance(timestamp, str)
    ):
        raise ValueError("invalid run identity")
    created = datetime.fromisoformat(timestamp.replace("Z", "+00:00"))
    if created.tzinfo is None:
        raise ValueError("run timestamp lacks timezone")
    if not all(
        isinstance(item.get(key), str)
        for key in ("head_sha", "event", "head_branch", "status")
    ):
        raise ValueError("invalid run fields")
    conclusion = item.get("conclusion")
    if conclusion is not None and not isinstance(conclusion, str):
        raise ValueError("invalid run conclusion")
    return created, run_id, item


def _parse_pages(pages: Any) -> list[tuple[datetime, int, dict[str, Any]]]:
    if not isinstance(pages, list) or not pages:
        raise ValueError("missing CI pages")
    runs: list[tuple[datetime, int, dict[str, Any]]] = []
    seen: set[int] = set()
    expected: int | None = None
    for page in pages:
        if not isinstance(page, dict):
            raise ValueError("invalid CI page")
        count = page.get("total_count")
        items = page.get("workflow_runs")
        if (
            not isinstance(count, int)
            or isinstance(count, bool)
            or count < 0
            or not isinstance(items, list)
        ):
            raise ValueError("invalid CI page fields")
        if expected is None:
            expected = count
        if count != expected:
            raise ValueError("inconsistent CI page counts")
        for item in items:
            run = _parse_run(item)
            if run[1] in seen:
                raise ValueError("duplicate CI run")
            seen.add(run[1])
            runs.append(run)
    if len(runs) != expected:
        raise ValueError("incomplete CI pages")
    return runs


def ci_status(pages: Any, source_sha: str) -> str:
    """Return success, pending, or failure for complete exact-commit results."""
    if not _SHA.fullmatch(source_sha):
        return "failure"
    try:
        runs = _parse_pages(pages)
    except ValueError:
        return "failure"
    applicable = [
        run
        for run in runs
        if run[2]["head_sha"] == source_sha
        and run[2]["event"] == "push"
        and run[2]["head_branch"] == "main"
    ]
    if not applicable:
        return "pending"
    latest = max(applicable, key=lambda run: (run[0], run[1]))[2]
    status = latest["status"]
    conclusion = latest.get("conclusion")
    if status in {"queued", "in_progress", "waiting", "pending", "requested"}:
        return "pending" if conclusion is None else "failure"
    if status != "completed":
        return "failure"
    return "success" if conclusion == "success" else "failure"


def ci_succeeded(pages: Any, source_sha: str) -> bool:
    """Require a complete successful main push for the exact SHA."""
    return ci_status(pages, source_sha) == "success"


def main() -> None:
    try:
        pages = json.load(sys.stdin)
    except (ValueError, OSError) as error:
        raise SystemExit("Unable to read CI results; retry later.") from error
    status = ci_status(pages, os.environ.get("GITHUB_SHA", ""))
    if status == "pending":
        print("CI is not yet complete for this SHA.", file=sys.stderr)
        raise SystemExit(2)
    if status != "success":
        raise SystemExit("CI failed or returned invalid results for this SHA.")


if __name__ == "__main__":
    main()
