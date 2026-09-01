from __future__ import annotations

from collections.abc import Callable, Mapping
from typing import Any, cast

import pytest

import build

nightly_identity_from = cast(
    Callable[[Mapping[str, str], str], dict[str, str]],
    vars(build)["nightly_identity_from"],
)
resolve_version_from = cast(
    Callable[[Mapping[str, str], str], dict[str, Any]],
    vars(build)["resolve_version_from"],
)

CHANGELOG = """### Unreleased

* Work in progress.

### 2.5.0

* Added the widget.
"""


def test_prefixes_the_changelog_fallback_with_v() -> None:
    resolved = resolve_version_from({}, CHANGELOG)
    assert resolved == {"version": "2.5.0", "tag": "v2.5.0", "preRelease": False}


def test_ignores_github_ref_name_when_tag_release_is_unset() -> None:
    env = {"GITHUB_REF_NAME": "refs/heads/main"}
    assert resolve_version_from(env, CHANGELOG)["tag"] == "v2.5.0"


def test_build_version_override_sets_the_manifest_version_and_default_tag() -> None:
    env = {"CWTOOLS_BUILD_VERSION": "2.5.42"}
    assert resolve_version_from(env, CHANGELOG) == {
        "version": "2.5.42",
        "tag": "v2.5.42",
        "preRelease": False,
    }


def test_build_version_override_uses_the_release_tag_for_prerelease_status() -> None:
    env = {
        "CWTOOLS_BUILD_VERSION": "2.5.42",
        "CWTOOLS_RELEASE_TAG": "v2.5.42-nightly.3",
    }
    assert resolve_version_from(env, CHANGELOG) == {
        "version": "2.5.42",
        "tag": "v2.5.42-nightly.3",
        "preRelease": True,
    }


@pytest.mark.parametrize("flag", ["true", "TRUE", "1"])
def test_passes_the_pushed_tag_through_on_a_tag_release(flag: str) -> None:
    env = {"TAG_RELEASE": flag, "GITHUB_REF_NAME": "v3.1.0"}
    resolved = resolve_version_from(env, CHANGELOG)
    assert resolved == {"version": "3.1.0", "tag": "v3.1.0", "preRelease": False}


@pytest.mark.parametrize("flag", ["false", "0", ""])
def test_treats_any_other_tag_release_value_as_not_a_tag_release(flag: str) -> None:
    env = {"TAG_RELEASE": flag, "GITHUB_REF_NAME": "v3.1.0"}
    assert resolve_version_from(env, CHANGELOG)["tag"] == "v2.5.0"


def test_falls_back_to_the_changelog_when_the_ref_name_is_blank() -> None:
    env = {"TAG_RELEASE": "true", "GITHUB_REF_NAME": "  "}
    assert resolve_version_from(env, CHANGELOG)["tag"] == "v2.5.0"


def test_flags_a_prerelease_from_the_pushed_tag() -> None:
    env = {"TAG_RELEASE": "true", "GITHUB_REF_NAME": "v1.0.0-beta.2"}
    resolved = resolve_version_from(env, CHANGELOG)
    assert resolved == {
        "version": "1.0.0-beta.2",
        "tag": "v1.0.0-beta.2",
        "preRelease": True,
    }


def test_flags_a_prerelease_from_the_changelog() -> None:
    resolved = resolve_version_from({}, "## [1.0.0-beta.2]\n\n* X.\n")
    assert resolved == {
        "version": "1.0.0-beta.2",
        "tag": "v1.0.0-beta.2",
        "preRelease": True,
    }


def test_does_not_double_the_v_on_a_changelog_heading_that_carries_one() -> None:
    assert resolve_version_from({}, "## v0.9.0\n\n* X.\n")["tag"] == "v0.9.0"


def test_throws_when_the_changelog_has_no_version_heading() -> None:
    with pytest.raises(RuntimeError, match="could not find a version heading"):
        resolve_version_from({}, "# Title\n\nBody only.\n")


def test_nightly_identity_uses_numeric_version_and_unique_release_tag() -> None:
    env = {"GITHUB_RUN_NUMBER": "42", "GITHUB_RUN_ATTEMPT": "3"}
    assert nightly_identity_from(env, CHANGELOG) == {
        "version": "2.5.42",
        "tag": "v2.5.42-nightly.3",
    }


def test_nightly_identity_uses_the_stable_part_of_a_prerelease_heading() -> None:
    env = {"GITHUB_RUN_NUMBER": "7"}
    changelog = "### 4.0.2-beta.2\n\n* Beta.\n"
    assert nightly_identity_from(env, changelog) == {
        "version": "4.0.9",
        "tag": "v4.0.9-nightly.1",
    }


@pytest.mark.parametrize(
    "env",
    [
        {},
        {"GITHUB_RUN_NUMBER": "nope"},
        {"GITHUB_RUN_NUMBER": "1", "GITHUB_RUN_ATTEMPT": "nope"},
        {"GITHUB_RUN_NUMBER": "0"},
        {"GITHUB_RUN_NUMBER": "1", "GITHUB_RUN_ATTEMPT": "0"},
    ],
)
def test_nightly_identity_rejects_invalid_run_identity(env: dict[str, str]) -> None:
    with pytest.raises(RuntimeError, match="nightly"):
        nightly_identity_from(env, CHANGELOG)
