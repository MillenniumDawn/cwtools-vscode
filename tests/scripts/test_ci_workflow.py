"""Regression guards for the client workflow's host-test gates."""

import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
CI = REPO_ROOT / ".github" / "workflows" / "ci.yml"
LIVE_SETTINGS = REPO_ROOT / "extension" / "test" / "host" / "liveSettings.test.ts"


def workflow_step(workflow: str, name: str) -> str:
    return workflow.split(f"      - name: {name}\n", maxsplit=1)[1].split(
        "\n      - ", maxsplit=1
    )[0]


def test_package_steps_do_not_run_after_cancellation() -> None:
    workflow = CI.read_text(encoding="utf-8")
    test_build = workflow_step(workflow, "Test build")
    smoke_test = workflow_step(workflow, "Smoke test VSIX")

    assert "!cancelled()" in test_build
    assert "steps.build-engine.outcome == 'success'" in test_build
    assert "steps.host-rules-sync.outcome == 'success'" in test_build
    assert "steps.host-configurations.outcome == 'success'" in test_build
    assert "always()" not in test_build
    assert "!cancelled()" in smoke_test
    assert "steps.test-build.outcome == 'success'" in smoke_test
    assert "always()" not in smoke_test


def test_live_settings_timeout_covers_two_hover_waits_with_overhead() -> None:
    source = LIVE_SETTINGS.read_text(encoding="utf-8")
    hover_match = re.search(r"hoverTimeoutMs = ([\d_]+)", source)
    suite_match = re.search(r"this\.timeout\(([\d_]+)\)", source)
    assert hover_match is not None
    assert suite_match is not None
    hover_timeout = int(hover_match.group(1).replace("_", ""))
    suite_timeout = int(suite_match.group(1).replace("_", ""))

    assert hover_timeout == 30_000
    assert suite_timeout >= 2 * hover_timeout + 15_000


def test_ci_keeps_distinct_host_suites_and_reuses_build_for_coverage() -> None:
    workflow = CI.read_text(encoding="utf-8")
    assert "--label live --label multi-root --label watched" in workflow
    assert "--label rules-sync" in workflow
    coverage = workflow_step(workflow, "Host coverage")
    assert "scripts/build/host_coverage.py" in coverage
    assert "--skip-compile" in coverage
