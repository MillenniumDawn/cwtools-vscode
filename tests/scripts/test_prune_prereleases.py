from __future__ import annotations

import json
import subprocess
from types import ModuleType
from typing import Any


def release(
    prune_prereleases: ModuleType,
    tag: str,
    published_at: str,
    is_prerelease: bool = True,
) -> Any:
    return prune_prereleases.Release(tag, published_at, is_prerelease)


def test_select_releases_keeps_newest_workflow_tags_only(
    prune_prereleases: ModuleType,
) -> None:
    releases = [
        release(prune_prereleases, "v3.5.7-pre.1", "2026-09-20T12:00:00Z"),
        release(prune_prereleases, "v3.5.8-pre.1", "2026-09-21T12:00:00Z"),
        release(prune_prereleases, "v3.5.6-pre.1", "2026-09-19T12:00:00Z"),
        release(prune_prereleases, "v3.4.9", "2026-09-22T12:00:00Z", False),
        release(
            prune_prereleases,
            "v3.5.5-nightly.1",
            "2026-09-23T12:00:00Z",
        ),
        release(
            prune_prereleases,
            "v3.5.4-pre.1",
            "2026-09-24T12:00:00Z",
            False,
        ),
    ]

    kept, stale = prune_prereleases.select_releases(releases, keep_count=2)

    assert [item.tag for item in kept] == ["v3.5.8-pre.1", "v3.5.7-pre.1"]
    assert [item.tag for item in stale] == ["v3.5.6-pre.1"]


def test_parse_releases_leaves_drafts_untouched(
    prune_prereleases: ModuleType,
) -> None:
    payload = json.dumps(
        [
            {
                "tagName": "v3.5.7-pre.1",
                "publishedAt": "2026-09-20T12:00:00Z",
                "isPrerelease": True,
            },
            {
                "tagName": "v3.5.8-pre.1",
                "publishedAt": None,
                "isPrerelease": True,
            },
        ]
    )

    parsed = prune_prereleases.parse_releases(payload)

    assert [item.tag for item in parsed] == ["v3.5.7-pre.1"]


def test_main_is_dry_run_without_delete_flag(
    prune_prereleases: ModuleType, monkeypatch: Any, capsys: Any
) -> None:
    payload = json.dumps(
        [
            {
                "tagName": "v3.5.1-pre.1",
                "publishedAt": "2026-09-19T12:00:00Z",
                "isPrerelease": True,
            },
            {
                "tagName": "v3.5.2-pre.1",
                "publishedAt": "2026-09-20T12:00:00Z",
                "isPrerelease": True,
            },
        ]
    )
    calls: list[list[str]] = []

    def fake_run(command: list[str], **_kwargs: object) -> Any:
        calls.append(command)
        return subprocess.CompletedProcess(command, 0, stdout=payload)

    monkeypatch.setattr(prune_prereleases.subprocess, "run", fake_run)

    assert prune_prereleases.main(["--keep", "1"]) == 0

    assert len(calls) == 1
    assert "WOULD DELETE v3.5.1-pre.1" in capsys.readouterr().out


def test_main_deletes_only_selected_release_with_cleanup_tag(
    prune_prereleases: ModuleType, monkeypatch: Any
) -> None:
    payload = json.dumps(
        [
            {
                "tagName": "v3.5.1-pre.1",
                "publishedAt": "2026-09-19T12:00:00Z",
                "isPrerelease": True,
            },
            {
                "tagName": "v3.5.2-pre.1",
                "publishedAt": "2026-09-20T12:00:00Z",
                "isPrerelease": True,
            },
            {
                "tagName": "v3.4.9",
                "publishedAt": "2026-09-21T12:00:00Z",
                "isPrerelease": False,
            },
        ]
    )
    calls: list[list[str]] = []

    def fake_run(command: list[str], **_kwargs: object) -> Any:
        calls.append(command)
        return subprocess.CompletedProcess(command, 0, stdout=payload)

    monkeypatch.setattr(prune_prereleases.subprocess, "run", fake_run)

    assert prune_prereleases.main(["--keep", "1", "--delete"]) == 0

    assert calls[0][0:3] == ["gh", "release", "list"]
    assert calls[1] == [
        "gh",
        "release",
        "delete",
        "v3.5.1-pre.1",
        "--cleanup-tag",
        "--yes",
    ]
