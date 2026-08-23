from __future__ import annotations

import unittest

from load import load_build

changelog = load_build("changelog")

THREE_RELEASES = """### Unreleased

* Work in progress.

### 2.5.0

* Added the widget.
* Fixed the flange.

### 2.4.0

* Old notes.
"""


class ReleaseNotesTests(unittest.TestCase):
    def test_returns_the_section_body_for_a_present_version(self) -> None:
        self.assertEqual(
            changelog.release_notes(THREE_RELEASES, "2.5.0"),
            "* Added the widget.\n* Fixed the flange.",
        )

    def test_returns_the_full_tail_when_the_section_is_the_last_heading(self) -> None:
        self.assertEqual(
            changelog.release_notes(THREE_RELEASES, "2.4.0"), "* Old notes."
        )

    def test_throws_for_a_missing_version(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError, "refusing to publish a release with auto-generated notes"
        ):
            changelog.release_notes(THREE_RELEASES, "9.9.9")

    def test_does_not_match_a_non_version_heading(self) -> None:
        with self.assertRaises(RuntimeError):
            changelog.release_notes(THREE_RELEASES, "Unreleased")

    def test_requires_an_exact_version_not_a_prefix(self) -> None:
        with self.assertRaises(RuntimeError):
            changelog.release_notes(THREE_RELEASES, "2.5")
        with self.assertRaises(RuntimeError):
            changelog.release_notes(THREE_RELEASES, "2")

    def test_does_not_match_a_prerelease_section_with_the_bare_version(self) -> None:
        text = "## [1.0.0-beta.2]\n\n* Notes.\n"
        with self.assertRaises(RuntimeError):
            changelog.release_notes(text, "1.0.0")
        self.assertEqual(changelog.release_notes(text, "1.0.0-beta.2"), "* Notes.")

    def test_trims_blank_lines_around_the_body(self) -> None:
        text = "### 1.0.0\n\n\n* Notes.\n\n### 2.0.0\n\n* Two.\n"
        self.assertEqual(changelog.release_notes(text, "1.0.0"), "* Notes.")

    def test_throws_for_an_empty_section_body(self) -> None:
        text = "### 1.0.0\n\n### 2.0.0\n\n* Two.\n"
        with self.assertRaises(RuntimeError):
            changelog.release_notes(text, "1.0.0")

    def test_throws_when_the_last_heading_has_no_body(self) -> None:
        text = "### 2.0.0\n\n* Two.\n\n### 1.0.0"
        with self.assertRaises(RuntimeError):
            changelog.release_notes(text, "1.0.0")

    def test_does_not_treat_an_indented_heading_like_line_as_a_boundary(self) -> None:
        text = (
            "### 1.0.0\n\n* Example:\n  ### 2.5.0 not a real heading\n\n"
            "### 0.9.0\n\n* Old.\n"
        )
        self.assertEqual(
            changelog.release_notes(text, "1.0.0"),
            "* Example:\n  ### 2.5.0 not a real heading",
        )

    def test_finds_a_section_whose_heading_has_trailing_decoration(self) -> None:
        text = "### 2.5.0 - 2026-08-01\n\n* X.\n\n### 2.4.0\n\n* Y.\n"
        self.assertEqual(changelog.release_notes(text, "2.5.0"), "* X.")


class TopChangelogVersionTests(unittest.TestCase):
    def test_returns_the_first_version_heading(self) -> None:
        self.assertEqual(changelog.top_changelog_version(THREE_RELEASES), "2.5.0")

    def test_handles_bracketed_v_prefixed_and_prerelease_headings(self) -> None:
        self.assertEqual(
            changelog.top_changelog_version("## [1.0.0]\n\n* X.\n"), "1.0.0"
        )
        self.assertEqual(
            changelog.top_changelog_version("## v0.9.0\n\n* X.\n"), "0.9.0"
        )
        self.assertEqual(
            changelog.top_changelog_version("## [1.0.0-beta.2]\n\n* X.\n"),
            "1.0.0-beta.2",
        )

    def test_ignores_a_heading_that_is_not_at_the_start_of_a_line(self) -> None:
        text = "text ### 1.0.0\n\n### 2.0.0\n\n* Two.\n"
        self.assertEqual(changelog.top_changelog_version(text), "2.0.0")

    def test_tolerates_trailing_text_after_the_version(self) -> None:
        self.assertEqual(
            changelog.top_changelog_version("### 2.5.0 - 2026-08-01\n\n* X.\n"),
            "2.5.0",
        )

    def test_throws_when_there_is_no_version_heading(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "could not find a version heading"):
            changelog.top_changelog_version("# Title\n\nBody only.\n")


if __name__ == "__main__":
    unittest.main()
