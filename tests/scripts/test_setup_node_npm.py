from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
ACTION_PATH = REPO_ROOT / ".github" / "actions" / "setup-node-npm" / "action.yml"
NETWORK_ASSIGNMENT = 'version="$(curl -fsSL "$url" | jq -r \'.[0]\' || true)"'
INSTALL_RUN = "    - name: Install dependencies\n"


def _install_script() -> str:
    action = ACTION_PATH.read_text(encoding="utf-8")
    run_start = action.index("      run: |\n", action.index(INSTALL_RUN)) + len(
        "      run: |\n"
    )
    shell_lines = []
    for line in action[run_start:].splitlines():
        if not line.startswith("        "):
            break
        shell_lines.append(line[8:])
    return "\n".join(shell_lines)


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


@pytest.mark.parametrize(
    ("ignore_scripts", "expected"),
    [("false", "ci"), ("true", "ci --ignore-scripts --foreground-scripts")],
)
def test_dependency_install_mode(
    ignore_scripts: str, expected: str, tmp_path: Path
) -> None:
    bash = shutil.which("bash")
    if bash is None:
        pytest.skip("bash is not available")

    bin_path = tmp_path / "bin"
    bin_path.mkdir()
    npm = bin_path / "npm"
    npm.write_text('#!/bin/bash\nprintf "%s\\n" "$*" >> "$NPM_CALLS"\n')
    npm.chmod(0o755)
    calls_path = tmp_path / "calls"
    environment = os.environ.copy()
    environment.update(
        {
            "NPM_CALLS": str(calls_path),
            "IGNORE_SCRIPTS": ignore_scripts,
            "PATH": f"{bin_path}{os.pathsep}{environment['PATH']}",
        }
    )

    result = subprocess.run(
        [bash, "-c", _install_script()],
        check=False,
        capture_output=True,
        text=True,
        env=environment,
    )

    assert result.returncode == 0, result.stderr
    assert calls_path.read_text(encoding="utf-8") == expected + "\n"
