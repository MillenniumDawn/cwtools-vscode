from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
ACTION_PATH = REPO_ROOT / ".github" / "actions" / "setup-node-npm" / "action.yml"
NETWORK_ASSIGNMENT = 'version="$(curl -fsSL "$url" | jq -r \'.[0]\' || true)"'


def _validation_script() -> str:
    action = ACTION_PATH.read_text(encoding="utf-8")
    resolve_start = action.index("    - name: Resolve VS Code stable version")
    run_start = action.index("      run: |\n", resolve_start) + len("      run: |\n")
    shell_lines = []
    for line in action[run_start:].splitlines():
        if not line.startswith("        "):
            break
        shell_lines.append(line[8:])

    script = "\n".join(shell_lines)
    assert script.count(NETWORK_ASSIGNMENT) == 1
    return script.replace(NETWORK_ASSIGNMENT, 'version="$VERSION_INPUT"', 1)


@pytest.mark.parametrize(
    ("version", "expected"),
    [
        ("1.2.3", "1.2.3"),
        ("not-a-version", "unknown"),
        ("", "unknown"),
        ("1.2.3\n4.5.6", "unknown"),
        (" 1.2.3", "unknown"),
        ("1.2.3 ", "unknown"),
        ("1.2.3\n", "unknown"),
        ("1.2.3-beta.1", "unknown"),
        ("1.2.3+build.5", "unknown"),
        ("1.2.3.4", "unknown"),
        ("1.2", "unknown"),
        ("1" * 5000, "unknown"),
        ("1.2.3; rm -rf /", "unknown"),
        ("$(printf injected)", "unknown"),
        ("1.2.3 & echo hacked", "unknown"),
    ],
    ids=[
        "valid",
        "malformed",
        "empty",
        "multiline",
        "leading-whitespace",
        "trailing-whitespace",
        "trailing-newline",
        "prerelease",
        "build-metadata",
        "four-segments",
        "two-segments",
        "very-long",
        "semicolon-metachar",
        "command-substitution",
        "ampersand-metachar",
    ],
)
def test_vscode_version_validation_writes_expected_output(
    version: str, expected: str, tmp_path: Path
) -> None:
    bash = shutil.which("bash")
    if bash is None:
        pytest.skip("bash is not available")

    output_path = tmp_path / "github_output"
    environment = os.environ.copy()
    environment.update({"GITHUB_OUTPUT": str(output_path), "VERSION_INPUT": version})
    result = subprocess.run(
        [bash, "-c", _validation_script()],
        check=False,
        capture_output=True,
        text=True,
        env=environment,
    )

    assert result.returncode == 0, result.stderr
    assert output_path.read_text(encoding="utf-8") == f"version={expected}\n"
