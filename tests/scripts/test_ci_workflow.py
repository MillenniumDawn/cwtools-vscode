"""Regression guards for the client workflow's host-test gates."""

import re
from pathlib import Path

from workflow_helpers import job_scope, workflow_step

REPO_ROOT = Path(__file__).resolve().parents[2]
CI = REPO_ROOT / ".github" / "workflows" / "ci.yml"
LIVE_SETTINGS = REPO_ROOT / "extension" / "test" / "host" / "liveSettings.test.ts"


def test_stellaris_guard_uses_resolved_pins_and_retains_failures() -> None:
    workflow = CI.read_text(encoding="utf-8")
    job = workflow.split("  stellaris:\n", 1)[1].split("  # One PR comment", 1)[0]
    assert "needs: pins" in job
    assert "needs.pins.outputs.stellaris_corpus_rev" in job
    assert "needs.pins.outputs.stellaris_rules_rev" in job
    # Empty pins fail the tier before any checkout can fall back to a default
    # branch.
    assert job.index("Require resolved Stellaris pins") < job.index("actions/checkout")
    assert "scripts/guard.py stellaris --no-build" in job
    assert "ui-prescripted-countries/ui-prescripted-countries" in job
    assert "cwtools-stellaris-config/config" in job
    assert "if: failure()" in job
    assert "drift.diff" in job and "validate.log" in job


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

    assert suite_timeout >= 2 * hover_timeout + 15_000


def test_ci_keeps_distinct_host_suites_and_reuses_build_for_coverage() -> None:
    workflow = CI.read_text(encoding="utf-8")
    configurations = workflow_step(
        workflow, "Host tests (multi-root, watched files, activation gate)"
    )
    assert "--label multi-root --label watched" in configurations
    assert "--label parent-language" in configurations
    assert "--label gate-descriptor --label gate-disabled" in configurations
    assert "--label live" not in configurations
    rules_sync = workflow_step(workflow, "Host tests (rules sync)")
    assert "--label rules-sync" in rules_sync
    coverage = workflow_step(workflow, "Host coverage")
    assert (
        "if: ${{ !cancelled() && steps.build-engine.outcome == 'success' }}" in coverage
    )
    assert "always()" not in coverage
    assert "scripts/build/host_coverage.py" in coverage
    assert "--skip-compile" in coverage


def test_rust_coverage_cleans_stale_instrumented_binaries_before_reporting() -> None:
    workflow = CI.read_text(encoding="utf-8")
    job = job_scope(workflow, "coverage-rs")
    install = workflow_step(job, "Install cargo-llvm-cov")
    cleanup = workflow_step(job, "Clean instrumented coverage artifacts")
    report = workflow_step(job, "Run coverage (94.4% line floor)")
    assert "working-directory: engine" in job.split("    steps:", 1)[0]
    assert "run: cargo llvm-cov clean" in cleanup
    assert "COVERAGE_THRESHOLD: 94.4" in report
    assert job.index(install) < job.index(cleanup) < job.index(report)
