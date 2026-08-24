from __future__ import annotations

import pytest

from changelog import release_notes, top_changelog_version

THREE_RELEASES = """### Unreleased

* Work in progress.

### 2.5.0

* Added the widget.
* Fixed the flange.

### 2.4.0

* Old notes.
"""


def test_returns_the_section_body_for_a_present_version() -> None:
    notes = release_notes(THREE_RELEASES, "2.5.0")
    assert notes == "* Added the widget.\n* Fixed the flange."


def test_returns_the_full_tail_when_the_section_is_the_last_heading() -> None:
    assert release_notes(THREE_RELEASES, "2.4.0") == "* Old notes."


def test_throws_for_a_missing_version() -> None:
    with pytest.raises(
        RuntimeError, match="refusing to publish a release with auto-generated notes"
    ):
        release_notes(THREE_RELEASES, "9.9.9")


def test_does_not_match_a_non_version_heading() -> None:
    with pytest.raises(RuntimeError):
        release_notes(THREE_RELEASES, "Unreleased")


@pytest.mark.parametrize("version", ["2.5", "2"])
def test_requires_an_exact_version_not_a_prefix(version: str) -> None:
    with pytest.raises(RuntimeError):
        release_notes(THREE_RELEASES, version)


def test_does_not_match_a_prerelease_section_with_the_bare_version() -> None:
    text = "## [1.0.0-beta.2]\n\n* Notes.\n"
    with pytest.raises(RuntimeError):
        release_notes(text, "1.0.0")
    assert release_notes(text, "1.0.0-beta.2") == "* Notes."


def test_trims_blank_lines_around_the_body() -> None:
    text = "### 1.0.0\n\n\n* Notes.\n\n### 2.0.0\n\n* Two.\n"
    assert release_notes(text, "1.0.0") == "* Notes."


def test_throws_for_an_empty_section_body() -> None:
    with pytest.raises(RuntimeError):
        release_notes("### 1.0.0\n\n### 2.0.0\n\n* Two.\n", "1.0.0")


def test_throws_when_the_last_heading_has_no_body() -> None:
    with pytest.raises(RuntimeError):
        release_notes("### 2.0.0\n\n* Two.\n\n### 1.0.0", "1.0.0")


def test_does_not_treat_an_indented_heading_like_line_as_a_boundary() -> None:
    text = (
        "### 1.0.0\n\n* Example:\n  ### 2.5.0 not a real heading\n\n"
        "### 0.9.0\n\n* Old.\n"
    )
    notes = release_notes(text, "1.0.0")
    assert notes == "* Example:\n  ### 2.5.0 not a real heading"


def test_finds_a_section_whose_heading_has_trailing_decoration() -> None:
    text = "### 2.5.0 - 2026-08-01\n\n* X.\n\n### 2.4.0\n\n* Y.\n"
    assert release_notes(text, "2.5.0") == "* X."


def test_top_version_returns_the_first_version_heading() -> None:
    assert top_changelog_version(THREE_RELEASES) == "2.5.0"


@pytest.mark.parametrize(
    ("text", "expected"),
    [
        ("## [1.0.0]\n\n* X.\n", "1.0.0"),
        ("## v0.9.0\n\n* X.\n", "0.9.0"),
        ("## [1.0.0-beta.2]\n\n* X.\n", "1.0.0-beta.2"),
    ],
)
def test_top_version_handles_bracketed_v_prefixed_and_prerelease_headings(
    text: str, expected: str
) -> None:
    assert top_changelog_version(text) == expected


def test_top_version_ignores_a_heading_that_is_not_at_the_start_of_a_line() -> None:
    assert top_changelog_version("text ### 1.0.0\n\n### 2.0.0\n\n* Two.\n") == "2.0.0"


def test_top_version_tolerates_trailing_text_after_the_version() -> None:
    assert top_changelog_version("### 2.5.0 - 2026-08-01\n\n* X.\n") == "2.5.0"


def test_top_version_throws_when_there_is_no_version_heading() -> None:
    with pytest.raises(RuntimeError, match="could not find a version heading"):
        top_changelog_version("# Title\n\nBody only.\n")
