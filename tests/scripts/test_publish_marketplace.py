from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = REPO_ROOT / ".github" / "workflows" / "publish-marketplace.yml"


def test_marketplace_workflow_requires_local_vsce() -> None:
    workflow = WORKFLOW.read_text(encoding="utf-8")

    assert workflow.count("npx --no-install @vscode/vsce") == 2
    assert "npx @vscode/vsce" not in workflow
    assert "npx --no-install @vscode/vsce publish" in workflow
    assert "npx --no-install @vscode/vsce generate-manifest" in workflow
