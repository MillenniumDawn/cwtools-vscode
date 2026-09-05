from __future__ import annotations

import json
import os
from collections.abc import Callable, Mapping
from pathlib import Path
from typing import Any, cast

import pytest

import build

resolve_version_from = cast(
    Callable[[Mapping[str, str], str], dict[str, Any]],
    vars(build)["resolve_version_from"],
)
top_changelog_version = cast(Callable[[str], str], vars(build)["top_changelog_version"])
REPO_ROOT = cast(Path, vars(build)["REPO_ROOT"])

MANIFEST_PATH = REPO_ROOT / "extension" / "package" / "package.json"
CHANGELOG_PATH = REPO_ROOT / "CHANGELOG.md"
RELEASE_CHANGELOG = """### Unreleased

* Work in progress.

### 3.3.0

* Released the widget.
"""


def _read_manifest_version() -> str:
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    if not isinstance(manifest, dict):
        raise AssertionError("source manifest is not a JSON object")
    version = manifest.get("version")
    if not isinstance(version, str):
        raise AssertionError("source manifest has no string version")
    return version


def _assert_versions_match(
    manifest_version: str, changelog: str, env: Mapping[str, str]
) -> None:
    changelog_version = top_changelog_version(changelog)
    if manifest_version != changelog_version:
        raise AssertionError(
            "source manifest version does not match latest changelog version: "
            f"{manifest_version} != {changelog_version}"
        )
    if env.get("TAG_RELEASE", "").lower() in {"1", "true"}:
        resolved = resolve_version_from(env, changelog)
        if manifest_version != resolved["version"]:
            raise AssertionError(
                "source manifest version does not match release tag: "
                f"{manifest_version} != {resolved['version']}"
            )


def test_source_manifest_matches_changelog_and_tag() -> None:
    env = {
        name: os.environ.get(name, "") for name in ("TAG_RELEASE", "GITHUB_REF_NAME")
    }
    _assert_versions_match(
        _read_manifest_version(), CHANGELOG_PATH.read_text(encoding="utf-8"), env
    )


def test_matching_versions_pass_on_a_tag_run() -> None:
    _assert_versions_match(
        "3.3.0",
        RELEASE_CHANGELOG,
        {"TAG_RELEASE": "true", "GITHUB_REF_NAME": "v3.3.0"},
    )


def test_manifest_changelog_mismatch_fails() -> None:
    with pytest.raises(AssertionError, match="latest changelog version"):
        _assert_versions_match("3.2.0", RELEASE_CHANGELOG, {})


def test_tag_mismatch_fails_on_a_tag_run() -> None:
    with pytest.raises(AssertionError, match="release tag"):
        _assert_versions_match(
            "3.3.0",
            RELEASE_CHANGELOG,
            {"TAG_RELEASE": "true", "GITHUB_REF_NAME": "v3.3.1"},
        )


def test_unreleased_heading_is_skipped() -> None:
    if top_changelog_version(RELEASE_CHANGELOG) != "3.3.0":
        raise AssertionError("Unreleased heading was treated as a release version")
    _assert_versions_match("3.3.0", RELEASE_CHANGELOG, {})
