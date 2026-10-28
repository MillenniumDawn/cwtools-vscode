"""Regression guard for Python CodeQL language coverage."""

import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
CODEQL = REPO_ROOT / ".github" / "workflows" / "codeql.yml"


def test_codeql_matrix_includes_python() -> None:
    workflow = CODEQL.read_text(encoding="utf-8")
    matrix = re.search(
        r"(?ms)^\s*language:\s*\n(?P<languages>(?:^[ \t]+-[ \t]+\S+[ \t]*\n?)+)",
        workflow,
    )
    assert matrix is not None
    languages = re.findall(r"(?m)^[ \t]+-[ \t]+(\S+)[ \t]*$", matrix["languages"])

    assert "python" in languages
    assert "queries: security-extended" in workflow
