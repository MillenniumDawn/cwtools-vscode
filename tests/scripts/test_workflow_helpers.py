import pytest

from workflow_helpers import job_scope, workflow_step

WORKFLOW = """jobs:
  first:
    runs-on: ubuntu-latest
    steps:
      - name: Shared name
        run: first
      - uses: actions/checkout@pinned
  last:
    runs-on: windows-latest
    steps:
      - name: Shared name
        run: last
"""


@pytest.mark.parametrize("job", ["first", "last"])
def test_job_scope_stops_at_the_next_job(job: str) -> None:
    block = job_scope(WORKFLOW, job)
    other = "last" if job == "first" else "first"
    assert f"run: {job}" in block
    assert f"run: {other}" not in block


@pytest.mark.parametrize("job", ["first", "last"])
def test_step_scope_stops_at_the_next_named_or_unnamed_step(job: str) -> None:
    step = workflow_step(job_scope(WORKFLOW, job), "Shared name")
    assert step.strip() == f"run: {job}"


def test_missing_job_is_an_error() -> None:
    with pytest.raises(ValueError):
        job_scope(WORKFLOW, "absent")


def test_missing_step_is_an_error() -> None:
    with pytest.raises(IndexError):
        workflow_step(job_scope(WORKFLOW, "first"), "absent")
