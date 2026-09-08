"""Guards on the publish workflow, one per bug it was written to fix.

Text assertions rather than a YAML parse: PyYAML is not a dev dependency, and
the repo already checks workflows this way (see test_publish_marketplace.py).
"""

from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = REPO_ROOT / ".github" / "workflows"
PUBLISH = WORKFLOWS / "publish.yml"

# The jobs release-failed guards on. Each must appear in both its `needs:` and
# its `if:`, or a failure in that job silently opens no fix pull request.
RELEASE_PATH_JOBS = (
    "verify",
    "build-server",
    "package",
    "marketplace",
    "open-vsx",
    "github",
)


def test_publish_replaced_the_three_workflows_it_merged() -> None:
    assert PUBLISH.is_file()
    for name in ("pre-release.yml", "release.yml", "tag-release.yml"):
        assert not (WORKFLOWS / name).exists(), f"{name} was merged into publish.yml"


def test_no_workflow_calls_another_workflow() -> None:
    # tag-release.yml called release.yml, which asked for contents: write from
    # a caller capped at contents: read -- every run died at startup. Nothing
    # here may reintroduce a caller/callee permission ceiling.
    for workflow in WORKFLOWS.glob("*.yml"):
        text = workflow.read_text(encoding="utf-8")
        assert "uses: ./.github/workflows/" not in text, workflow.name


def test_only_the_github_job_writes() -> None:
    workflow = PUBLISH.read_text(encoding="utf-8")

    assert workflow.count("contents: write") == 1
    github_job = workflow.split('name: "Publish: GitHub"', maxsplit=1)[1]
    assert "contents: write" in github_job.split("release-failed:", maxsplit=1)[0]


def test_each_registry_publishes_independently() -> None:
    workflow = PUBLISH.read_text(encoding="utf-8")

    # Both registries hang off `package`, not off each other: a Marketplace
    # timeout used to abort the job before Open VSX was ever reached.
    assert workflow.count("needs: [check, verify, package]") == 2
    # And re-running one of them must not trip over its own earlier upload.
    assert "skipDuplicate: true" in workflow
    assert "publish-prebuilt" not in workflow


def test_release_failed_guards_every_job_it_waits_on() -> None:
    workflow = PUBLISH.read_text(encoding="utf-8")
    block = workflow.split("release-failed:", maxsplit=1)[1]
    guard = block.split("runs-on:", maxsplit=1)[0]

    assert "needs.check.outputs.release == 'true'" in guard
    assert "always()" in guard
    for job in RELEASE_PATH_JOBS:
        assert f"- {job}\n" in guard, f"{job} missing from needs"
        assert f"needs.{job}.result == 'failure'" in guard, f"{job} missing from if"
    # A run someone cancelled by hand is not a release that broke.
    assert "cancelled" not in guard
