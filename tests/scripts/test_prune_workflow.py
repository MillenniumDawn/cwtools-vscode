from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = REPO_ROOT / ".github" / "workflows" / "prune-prereleases.yml"
SCRIPT = REPO_ROOT / "scripts" / "prune_prereleases.py"


def test_prune_workflow_is_separate_and_scheduled() -> None:
    workflow = WORKFLOW.read_text(encoding="utf-8")

    assert 'cron: "0 3 * * *"' in workflow
    assert "workflow_dispatch:" in workflow
    assert "confirm_delete:" in workflow
    assert "contents: write" in workflow
    assert "python3 scripts/prune_prereleases.py" in workflow
    assert "args+=(--delete)" in workflow
    assert "publish.yml" not in workflow


def test_prune_script_defaults_to_report_only_and_cleans_up_tags() -> None:
    script = SCRIPT.read_text(encoding="utf-8")

    assert 'action="store_true"' in script
    assert '"--delete"' in script
    assert '"--cleanup-tag"' in script
    assert "v*-nightly.*" in script
    assert "v\\d+\\.\\d+\\.\\d+-pre\\.\\d+" in script
