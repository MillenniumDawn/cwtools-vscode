"""Guards on the publish and release-PR workflows, one per bug each was
written to fix.

Text assertions rather than a YAML parse: PyYAML is not a dev dependency.
"""

from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = REPO_ROOT / ".github" / "workflows"
PUBLISH = WORKFLOWS / "publish.yml"
RELEASE_PR = WORKFLOWS / "release-pr.yml"

# The jobs that talk to a registry or create the tag. Each serializes on its
# own per-channel group; nothing above them does.
PUBLISH_JOBS = ("marketplace", "open-vsx", "github")

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


def test_publish_replaced_the_workflows_it_merged() -> None:
    assert PUBLISH.is_file()
    # publish-marketplace.yml was the hand-dispatched fallback: it published
    # without --pre-release, so a pre-release tag would have gone out on the
    # stable channel. Dispatching publish.yml against the tag replaces it.
    for name in (
        "pre-release.yml",
        "release.yml",
        "tag-release.yml",
        "publish-marketplace.yml",
    ):
        assert not (WORKFLOWS / name).exists(), f"{name} was merged into publish.yml"
    for workflow in WORKFLOWS.glob("*.yml"):
        text = workflow.read_text(encoding="utf-8")
        assert "publish-marketplace.yml" not in text, workflow.name


def test_publish_serializes_per_channel_not_per_workflow() -> None:
    # A workflow-level group keeps one pending run and cancels the older one,
    # so a release queued behind a pre-release build was cancelled by the next
    # push to main. Only the publish jobs serialize, each on a group keyed on
    # the channel, so a release never shares a queue with a pre-release.
    workflow = PUBLISH.read_text(encoding="utf-8")
    before_jobs = workflow.split("\njobs:", maxsplit=1)[0]
    assert "concurrency:" not in before_jobs

    channel = "needs.check.outputs.release == 'true' && 'release' || 'pre-release'"
    for job in PUBLISH_JOBS:
        block = workflow.split(f"\n  {job}:\n", maxsplit=1)[1]
        header = block.split("    steps:", maxsplit=1)[0]
        assert f"publish-{job}-${{{{\n        {channel} }}}}" in header, job
        assert "cancel-in-progress: false" in header, job
    assert workflow.count("cancel-in-progress: false") == len(PUBLISH_JOBS)


def test_github_publish_reruns_skip_an_existing_release() -> None:
    # The GitHub job used to delete and recreate a release that already
    # existed whenever TAG_RELEASE was set -- which it was, on every release,
    # so a re-run after a partial failure destroyed the first attempt's
    # release. The job no longer passes the flag and build.py no longer reads
    # it there (see test_publish_prebuilt.py).
    workflow = PUBLISH.read_text(encoding="utf-8")
    github_job = workflow.split('name: "Publish: GitHub"', maxsplit=1)[1]
    github_job = github_job.split("release-failed:", maxsplit=1)[0]
    assert "TAG_RELEASE" not in github_job


def test_release_pr_yields_when_main_moved_and_closes_a_stale_pr() -> None:
    # The run for the push before a release merge checks out a main that still
    # has the Unreleased bullets and, by the time it looks for an open PR, the
    # release PR has merged -- so it opened a second "Release x.y.z". The push
    # step re-checks origin/main against the checkout and yields, and the
    # "nothing to release" path closes whatever PR the race left behind.
    workflow = RELEASE_PR.read_text(encoding="utf-8")
    assert 'echo "base=$(git rev-parse origin/main)"' in workflow
    assert 'if [ "$(git rev-parse origin/main)" != "$BASE" ]; then' in workflow
    assert "steps.push.outputs.pushed == 'true'" in workflow

    close = workflow.split("Close a stale release pull request", maxsplit=1)[1]
    assert "steps.prepare.outputs.release != 'true'" in close
    assert "gh pr close" in close
    assert "--delete-branch" in close
    assert "tag-release.yml" not in workflow


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
