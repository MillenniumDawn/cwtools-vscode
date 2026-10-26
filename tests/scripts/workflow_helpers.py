import re


def job_scope(workflow: str, job: str) -> str:
    marker = f"\n  {job}:\n"
    body = workflow[workflow.index(marker) + len(marker) :]
    next_job = re.search(r"\n  \S", body)
    return body if next_job is None else body[: next_job.start()]


def workflow_step(scope: str, name: str) -> str:
    return scope.split(f"      - name: {name}\n", maxsplit=1)[1].split(
        "\n      - ", maxsplit=1
    )[0]
