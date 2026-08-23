from __future__ import annotations

import unittest

from load import load_build

build = load_build("build")

CHANGELOG = """### Unreleased

* Work in progress.

### 2.5.0

* Added the widget.
"""


class ResolveVersionFromTests(unittest.TestCase):
    def test_prefixes_the_changelog_fallback_with_v(self) -> None:
        self.assertEqual(
            build.resolve_version_from({}, CHANGELOG),
            {"version": "2.5.0", "tag": "v2.5.0", "preRelease": False},
        )

    def test_ignores_github_ref_name_when_tag_release_is_unset(self) -> None:
        env = {"GITHUB_REF_NAME": "refs/heads/main"}
        self.assertEqual(build.resolve_version_from(env, CHANGELOG)["tag"], "v2.5.0")

    def test_passes_the_pushed_tag_through_on_a_tag_release(self) -> None:
        for flag in ("true", "TRUE", "1"):
            self.assertEqual(
                build.resolve_version_from(
                    {"TAG_RELEASE": flag, "GITHUB_REF_NAME": "v3.1.0"},
                    CHANGELOG,
                ),
                {"version": "3.1.0", "tag": "v3.1.0", "preRelease": False},
            )

    def test_treats_any_other_tag_release_value_as_not_a_tag_release(self) -> None:
        for flag in ("false", "0", ""):
            self.assertEqual(
                build.resolve_version_from(
                    {"TAG_RELEASE": flag, "GITHUB_REF_NAME": "v3.1.0"},
                    CHANGELOG,
                )["tag"],
                "v2.5.0",
            )

    def test_falls_back_to_the_changelog_when_the_ref_name_is_blank(self) -> None:
        env = {"TAG_RELEASE": "true", "GITHUB_REF_NAME": "  "}
        self.assertEqual(build.resolve_version_from(env, CHANGELOG)["tag"], "v2.5.0")

    def test_flags_a_prerelease_from_either_source(self) -> None:
        env = {"TAG_RELEASE": "true", "GITHUB_REF_NAME": "v1.0.0-beta.2"}
        self.assertEqual(
            build.resolve_version_from(env, CHANGELOG),
            {
                "version": "1.0.0-beta.2",
                "tag": "v1.0.0-beta.2",
                "preRelease": True,
            },
        )
        self.assertEqual(
            build.resolve_version_from({}, "## [1.0.0-beta.2]\n\n* X.\n"),
            {
                "version": "1.0.0-beta.2",
                "tag": "v1.0.0-beta.2",
                "preRelease": True,
            },
        )

    def test_does_not_double_the_v_on_a_changelog_heading_that_carries_one(
        self,
    ) -> None:
        self.assertEqual(
            build.resolve_version_from({}, "## v0.9.0\n\n* X.\n")["tag"],
            "v0.9.0",
        )

    def test_throws_when_the_changelog_has_no_version_heading(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "could not find a version heading"):
            build.resolve_version_from({}, "# Title\n\nBody only.\n")


if __name__ == "__main__":
    unittest.main()
