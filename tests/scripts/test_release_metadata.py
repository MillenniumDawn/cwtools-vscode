"""Release metadata must be valid before any registry can publish."""

from pathlib import Path

import pytest

from build import validate_release_metadata


def test_accepts_matching_release_with_notes() -> None:
    validate_release_metadata("v4.0.8", "4.0.8", "### 4.0.8\n\n* Fix\n")


@pytest.mark.parametrize(
    ("tag", "manifest", "changelog"),
    [
        ("v4.0.8", "4.0.8", "### 4.0.7\n\n* Old\n"),
        ("v4.0.8", "4.0.7", "### 4.0.8\n\n* Fix\n"),
        ("v4.0.8", "4.0.8", "### 4.0.8\n\n### 4.0.7\n\n* Old\n"),
        ("v4.0.8-pre.1", "4.0.8", "### 4.0.8\n\n* Fix\n"),
        ("v4.0.8", None, "### 4.0.8\n\n* Fix\n"),
    ],
)
def test_rejects_invalid_metadata(tag: str, manifest: object, changelog: str) -> None:
    with pytest.raises(RuntimeError):
        validate_release_metadata(tag, manifest, changelog)


def test_preflight_precedes_release_output_and_downstream_jobs() -> None:
    workflow_path = (
        Path(__file__).resolve().parents[2] / ".github/workflows/publish.yml"
    )
    workflow = workflow_path.read_text(encoding="utf-8")
    tag_check = (
        'RELEASE_TAG="$GITHUB_REF_NAME" python3 scripts/build/build.py '
        "validate-release-metadata"
    )
    main_check = (
        'RELEASE_TAG="v$version" python3 scripts/build/build.py '
        "validate-release-metadata"
    )
    assert workflow.index(tag_check) < workflow.index('echo "release=true"')
    assert workflow.index(main_check) < workflow.index('echo "This push cuts release')
    assert "needs: check" in workflow
