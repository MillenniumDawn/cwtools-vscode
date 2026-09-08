from __future__ import annotations

import re
from collections.abc import Callable, Mapping
from typing import Any, cast

import pytest

import build

next_release_version = cast(
    Callable[[str, str], str], vars(build)["next_release_version"]
)
prerelease_identity_from = cast(
    Callable[[Mapping[str, str], str], dict[str, str]],
    vars(build)["prerelease_identity_from"],
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

# Pre-releases derive from the stable line, which lives on the even minors.
STABLE_CHANGELOG = """### Unreleased

* Work in progress.

### 3.4.0

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


def test_prerelease_identity_uses_the_odd_minor_above_stable() -> None:
    env = {"GITHUB_RUN_NUMBER": "42", "GITHUB_RUN_ATTEMPT": "3"}
    assert prerelease_identity_from(env, STABLE_CHANGELOG) == {
        "version": "3.5.42",
        "tag": "v3.5.42-pre.3",
    }


def test_prerelease_identity_defaults_the_run_attempt_to_one() -> None:
    env = {"GITHUB_RUN_NUMBER": "7"}
    assert prerelease_identity_from(env, "### 4.0.2\n\n* Stable.\n") == {
        "version": "4.1.7",
        "tag": "v4.1.7-pre.1",
    }


def test_prerelease_identity_uses_the_stable_part_of_a_prerelease_heading() -> None:
    env = {"GITHUB_RUN_NUMBER": "7"}
    changelog = "### 4.0.2-beta.2\n\n* Beta.\n"
    assert prerelease_identity_from(env, changelog) == {
        "version": "4.1.7",
        "tag": "v4.1.7-pre.1",
    }


def test_prerelease_identity_rejects_an_odd_stable_minor() -> None:
    env = {"GITHUB_RUN_NUMBER": "1"}
    with pytest.raises(RuntimeError, match="even stable minor"):
        prerelease_identity_from(env, CHANGELOG)


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
def test_prerelease_identity_rejects_invalid_run_identity(env: dict[str, str]) -> None:
    with pytest.raises(RuntimeError, match="pre-release"):
        prerelease_identity_from(env, STABLE_CHANGELOG)


@pytest.mark.parametrize(
    ("current", "bump", "expected"),
    [
        ("3.4.0", "patch", "3.4.1"),
        ("3.4.7", "patch", "3.4.8"),
        # A minor bump skips the odd line the pre-releases sit on.
        ("3.4.0", "minor", "3.6.0"),
        ("3.4.9", "major", "4.0.0"),
    ],
)
def test_next_release_version_keeps_releases_on_even_minors(
    current: str, bump: str, expected: str
) -> None:
    assert next_release_version(current, bump) == expected


def test_next_release_version_refuses_to_bump_a_prerelease_line() -> None:
    with pytest.raises(RuntimeError, match="pre-release line"):
        next_release_version("3.5.42", "patch")


def test_next_release_version_rejects_an_unknown_bump() -> None:
    with pytest.raises(RuntimeError, match="unknown bump"):
        next_release_version("3.4.0", "huge")


def test_next_release_version_rejects_a_malformed_version() -> None:
    with pytest.raises(RuntimeError, match=re.escape("not a x.y.z version")):
        next_release_version("3.4", "patch")
