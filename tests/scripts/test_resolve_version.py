from __future__ import annotations

import pytest

from build import resolve_version_from

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
