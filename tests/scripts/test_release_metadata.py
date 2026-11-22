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


def test_preflight_gates_downstream_jobs_and_reports_its_own_failure() -> None:
    workflow_path = (
        Path(__file__).resolve().parents[2] / ".github/workflows/publish.yml"
    )
    workflow = workflow_path.read_text(encoding="utf-8")
    check_job = workflow[
        workflow.index("\n  check:\n") : workflow.index("\n  verify:\n")
    ]
    preflight = check_job.index("validate-release-metadata")
    # The step runs in the job every other job needs, after the outputs that
    # name the release, and only for a release.
    assert check_job.index('echo "release=true"') < preflight
    assert "if: steps.check.outputs.release == 'true'" in check_job
    assert "RELEASE_TAG: ${{ steps.check.outputs.tag }}" in check_job
    verify_job = workflow[workflow.index("\n  verify:\n") :]
    assert "needs: check" in verify_job[: verify_job.index("steps:")]
    # A failed preflight opens the fix pull request like any other failure.
    release_failed = workflow[workflow.index("\n  release-failed:\n") :]
    condition = release_failed[: release_failed.index("runs-on:")]
    assert "needs.check.result == 'failure'" in condition
