from __future__ import annotations

import json
from pathlib import Path

import pytest

import release_pr

CHANGELOG = """### Unreleased

#### Tooling

* Promoted the widget. (#696)

### 3.4.0

* Added the widget.
"""

EMPTY_CHANGELOG = """### Unreleased

### 3.4.0

* Added the widget.
"""

MANIFEST = {"name": "cwtools-md-edition", "version": "3.4.0", "preview": False}


def _repo(tmp_path: Path, changelog: str) -> Path:
    (tmp_path / "CHANGELOG.md").write_text(changelog, encoding="utf-8")
    manifest = tmp_path / "extension" / "package" / "package.json"
    manifest.parent.mkdir(parents=True)
    manifest.write_text(json.dumps(MANIFEST, indent=2) + "\n", encoding="utf-8")
    return tmp_path


def _manifest_version(repo: Path) -> str:
    manifest = json.loads(
        (repo / "extension" / "package" / "package.json").read_text(encoding="utf-8")
    )
    assert isinstance(manifest, dict)
    version = manifest["version"]
    assert isinstance(version, str)
    return version


def test_an_unreleased_section_without_bullets_is_nothing_to_release() -> None:
    assert not release_pr.has_release_content(EMPTY_CHANGELOG)
    assert release_pr.has_release_content(CHANGELOG)


def test_a_dash_bullet_counts_as_release_content() -> None:
    assert release_pr.has_release_content("### Unreleased\n\n- Fixed it.\n")


def test_headings_alone_are_not_release_content() -> None:
    changelog = "### Unreleased\n\n#### Tooling\n\n### 3.4.0\n\n* Shipped.\n"
    assert not release_pr.has_release_content(changelog)


def test_promotion_leaves_a_fresh_unreleased_heading_above_the_version() -> None:
    promoted = release_pr.promote_unreleased(CHANGELOG, "3.4.1")
    assert promoted.startswith("### Unreleased\n\n### 3.4.1\n\n#### Tooling\n")
    assert "* Promoted the widget. (#696)" in promoted
    assert not release_pr.has_release_content(promoted)


def test_promotion_without_an_unreleased_heading_fails() -> None:
    with pytest.raises(RuntimeError, match="no '### Unreleased' heading"):
        release_pr.promote_unreleased("### 3.4.0\n\n* Shipped.\n", "3.4.1")


def test_manifest_bump_keeps_the_rest_of_the_manifest() -> None:
    bumped = json.loads(
        release_pr.bump_manifest(json.dumps(MANIFEST, indent=2) + "\n", "3.6.0")
    )
    assert bumped == {**MANIFEST, "version": "3.6.0"}


def test_manifest_bump_rejects_a_non_object_manifest() -> None:
    with pytest.raises(TypeError, match="not a JSON object"):
        release_pr.bump_manifest("[]", "3.6.0")


def test_main_promotes_the_changelog_and_the_manifest(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    repo = _repo(tmp_path, CHANGELOG)

    assert release_pr.main(["--bump", "patch", "--repo-root", str(repo)]) == 0

    out = capsys.readouterr().out
    assert "release=true" in out
    assert "version=3.4.1" in out
    assert "bump=patch" in out
    assert _manifest_version(repo) == "3.4.1"
    assert "### 3.4.1" in (repo / "CHANGELOG.md").read_text(encoding="utf-8")


def test_main_skips_the_odd_minor_line_on_a_minor_bump(tmp_path: Path) -> None:
    repo = _repo(tmp_path, CHANGELOG)

    assert release_pr.main(["--bump", "minor", "--repo-root", str(repo)]) == 0

    assert _manifest_version(repo) == "3.6.0"


def test_main_reports_nothing_to_release_on_an_empty_unreleased_section(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    repo = _repo(tmp_path, EMPTY_CHANGELOG)

    assert release_pr.main(["--repo-root", str(repo)]) == 0

    captured = capsys.readouterr()
    assert captured.out.strip() == "release=false"
    assert "nothing to release" in captured.err
    assert _manifest_version(repo) == "3.4.0"


def test_main_writes_nothing_on_a_dry_run(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    repo = _repo(tmp_path, CHANGELOG)

    argv = ["--bump", "minor", "--dry-run", "--repo-root", str(repo)]

    assert release_pr.main(argv) == 0

    assert "version=3.6.0" in capsys.readouterr().out
    assert (repo / "CHANGELOG.md").read_text(encoding="utf-8") == CHANGELOG
    assert _manifest_version(repo) == "3.4.0"
