from __future__ import annotations

import io
import json
import os
import subprocess
import textwrap
from pathlib import Path
from typing import Any

import pytest

import release_decisions
from workflow_helpers import job_scope

SHA = "a" * 40
OTHER = "b" * 40


def _run(**changes: Any) -> dict[str, Any]:
    return {
        "id": 1,
        "created_at": "2026-01-01T00:00:00Z",
        "head_sha": SHA,
        "event": "push",
        "head_branch": "main",
        "status": "completed",
        "conclusion": "success",
        **changes,
    }


def _pages(*runs: dict[str, Any]) -> list[dict[str, Any]]:
    return [{"total_count": len(runs), "workflow_runs": list(runs)}]


@pytest.mark.parametrize(
    "changes",
    [
        {"head_sha": OTHER},
        {"event": "pull_request"},
        {"head_branch": "feature"},
        {"status": "queued", "conclusion": None},
        {"status": "in_progress", "conclusion": None},
        {"conclusion": "failure"},
        {"conclusion": "cancelled"},
    ],
)
def test_ci_rejects_nonmatching_or_unsuccessful_runs(changes: dict[str, Any]) -> None:
    assert not release_decisions.ci_succeeded(_pages(_run(**changes)), SHA)


def test_ci_accepts_exact_main_push_and_rejects_absent_or_invalid_sha() -> None:
    assert release_decisions.ci_succeeded(_pages(_run()), SHA)
    assert not release_decisions.ci_succeeded(_pages(), SHA)
    assert not release_decisions.ci_succeeded(_pages(_run()), "bad")


@pytest.mark.parametrize("status", ["failure", "queued"])
def test_newer_unsuccessful_run_overrides_success_regardless_of_id(status: str) -> None:
    older = _run(id=900, created_at="2026-01-01T00:00:00Z")
    newer = _run(
        id=2,
        created_at="2026-01-02T00:00:00Z",
        status="queued" if status == "queued" else "completed",
        conclusion=None if status == "queued" else status,
    )
    assert not release_decisions.ci_succeeded(_pages(older, newer), SHA)
    assert release_decisions.ci_succeeded(
        _pages(
            {**older, "conclusion": "failure"},
            {**newer, "status": "completed", "conclusion": "success"},
        ),
        SHA,
    )


def test_complete_pagination_and_truncation() -> None:
    first = _run(id=1)
    second = _run(id=2, created_at="2026-01-02T00:00:00Z")
    pages = [
        {"total_count": 2, "workflow_runs": [first]},
        {"total_count": 2, "workflow_runs": [second]},
    ]
    assert release_decisions.ci_succeeded(pages, SHA)
    assert not release_decisions.ci_succeeded(pages[:1], SHA)
    assert not release_decisions.ci_succeeded(pages + pages[:1], SHA)
    pages[1]["total_count"] = 3
    assert not release_decisions.ci_succeeded(pages, SHA)


@pytest.mark.parametrize(
    "payload",
    [
        [],
        {},
        [{"message": "API error"}],
        [{"total_count": 1, "workflow_runs": [{}]}],
        [{"total_count": 1, "workflow_runs": [_run(created_at="bad")]}],
        [{"total_count": 1, "workflow_runs": [_run(head_sha=None)]}],
    ],
)
def test_malformed_api_payloads_fail_closed(payload: Any) -> None:
    assert not release_decisions.ci_succeeded(payload, SHA)


def test_cli_rejects_malformed_json_and_error_payload(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("GITHUB_SHA", SHA)
    for payload in ("{", json.dumps([{"message": "API error"}])):
        monkeypatch.setattr("sys.stdin", io.StringIO(payload))
        with pytest.raises(SystemExit):
            release_decisions.main()
    monkeypatch.setattr("sys.stdin", io.StringIO(json.dumps(_pages(_run()))))
    release_decisions.main()


@pytest.mark.parametrize("channel", ["release", "pre-release"])
def test_ci_gate_applies_to_both_channels(channel: str) -> None:
    workflow = (
        Path(__file__).resolve().parents[2] / ".github/workflows/publish.yml"
    ).read_text(encoding="utf-8")
    verify = job_scope(workflow, "verify")
    header = verify.split("    steps:", maxsplit=1)[0]
    assert "    if:" not in header, channel
    assert "Require successful CI for the source commit" in verify
    assert "needs: verify" in job_scope(workflow, "build-server")
    assert "needs: [check, verify, build-server]" in job_scope(workflow, "package")



def test_publish_polling_retries_and_remains_bounded(tmp_path: Path) -> None:
    """Run the actual workflow shell with fake API, checker, and clock."""
    workflow = (
        Path(__file__).resolve().parents[2] / ".github/workflows/publish.yml"
    ).read_text(encoding="utf-8")
    section = workflow.split("- name: Require successful CI for the source commit", 1)[1]
    script = section.split("        run: |\n", 1)[1].split("      - name:", 1)[0]
    script = textwrap.dedent(script)
    bindir = tmp_path / "bin"
    bindir.mkdir()
    state = tmp_path / "attempts"
    for name, body in {
        "gh": '#!/bin/sh\ncat "$STATE" 2>/dev/null || echo 0\n',
        "python3": (
            '#!/bin/sh\nread n\nn=$((n+1))\necho "$n" > "$STATE"\n'
            '[ "$n" -ge "$SUCCEED_AT" ]\n'
        ),
        "sleep": "#!/bin/sh\nexit 0\n",
    }.items():
        executable = bindir / name
        executable.write_text(body, encoding="utf-8")
        executable.chmod(0o755)
    env = {
        **os.environ,
        "PATH": f"{bindir}:{os.environ['PATH']}",
        "STATE": str(state),
        "GITHUB_REPOSITORY": "test/repo",
        "GITHUB_SHA": SHA,
    }
    for succeed_at, expected, attempts in [(3, 0, 3), (999, 1, 90)]:
        state.unlink(missing_ok=True)
        result = subprocess.run(
            ["bash", "-o", "pipefail", "-c", script],
            env={**env, "SUCCEED_AT": str(succeed_at)},
            capture_output=True,
            check=False,
        )
        assert result.returncode == expected, result.stderr
        assert int(state.read_text(encoding="utf-8")) == attempts
