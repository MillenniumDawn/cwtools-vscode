from __future__ import annotations

import json
import subprocess
from pathlib import Path
from types import ModuleType

import pytest

VALID_ROOT = """[workspace]
members = ["crates/sample"]

[workspace.dependencies]
tempfile = "3"
assert_cmd = "2"
predicates = "3"

[workspace.lints.rust]
warnings = "deny"

[workspace.lints.clippy]
all = "deny"
"""

VALID_CRATE = """[package]
name = "sample"
version = "0.1.0"
edition = "2024"
repository.workspace = true

[lints]
workspace = true

[dev-dependencies]
tempfile = { workspace = true }
"""

VALID_METADATA = {
    "packages": [
        {
            "name": "sample",
            "source": None,
            "repository": "https://example.test/sample",
        }
    ]
}


def _write_workspace(
    tmp_path: Path, *, root_text: str = VALID_ROOT, crate_text: str = VALID_CRATE
) -> Path:
    workspace = tmp_path / "engine"
    crate = workspace / "crates" / "sample"
    crate.mkdir(parents=True)
    (workspace / "Cargo.toml").write_text(root_text, encoding="utf-8")
    (crate / "Cargo.toml").write_text(crate_text, encoding="utf-8")
    return workspace


def _mock_cargo(
    monkeypatch: pytest.MonkeyPatch,
    workspace_manifest: ModuleType,
    *,
    stdout: str = json.dumps(VALID_METADATA),
    returncode: int = 0,
) -> None:
    monkeypatch.setattr(workspace_manifest.shutil, "which", lambda _name: "/fake/cargo")

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        return subprocess.CompletedProcess(
            command, returncode, stdout=stdout, stderr=""
        )

    monkeypatch.setattr(workspace_manifest.subprocess, "run", run)


def test_accepts_a_valid_crate_and_workspace(
    workspace_manifest: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    workspace = _write_workspace(tmp_path)
    monkeypatch.setenv("CWTOOLS_RS", str(workspace))
    _mock_cargo(monkeypatch, workspace_manifest)

    assert workspace_manifest.main() == 0
    assert capsys.readouterr().out == "workspace-manifest-check: ok\n"


def test_rejects_a_crate_without_repository_inheritance(
    workspace_manifest: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    workspace = _write_workspace(
        tmp_path, crate_text=VALID_CRATE.replace("repository.workspace = true\n", "")
    )
    monkeypatch.setenv("CWTOOLS_RS", str(workspace))
    _mock_cargo(monkeypatch, workspace_manifest)

    with pytest.raises(SystemExit) as caught:
        workspace_manifest.main()

    assert caught.value.code == 1
    assert "sample: missing repository.workspace = true" in capsys.readouterr().err


def test_rejects_a_crate_without_lints_inheritance(
    workspace_manifest: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    workspace = _write_workspace(
        tmp_path, crate_text=VALID_CRATE.replace("[lints]\nworkspace = true\n", "")
    )
    monkeypatch.setenv("CWTOOLS_RS", str(workspace))
    _mock_cargo(monkeypatch, workspace_manifest)

    with pytest.raises(SystemExit) as caught:
        workspace_manifest.main()

    assert caught.value.code == 1
    assert "sample: missing [lints] workspace = true" in capsys.readouterr().err


def test_rejects_a_direct_shared_dev_dependency(
    workspace_manifest: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    crate_text = VALID_CRATE.replace(
        "tempfile = { workspace = true }", 'tempfile = "3"'
    )
    workspace = _write_workspace(tmp_path, crate_text=crate_text)
    monkeypatch.setenv("CWTOOLS_RS", str(workspace))
    _mock_cargo(monkeypatch, workspace_manifest)

    with pytest.raises(SystemExit) as caught:
        workspace_manifest.main()

    assert caught.value.code == 1
    assert (
        "sample: tempfile/assert_cmd/predicates must use { workspace = true }"
        in capsys.readouterr().err
    )


def test_rejects_metadata_package_without_a_repository(
    workspace_manifest: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    workspace = _write_workspace(tmp_path)
    monkeypatch.setenv("CWTOOLS_RS", str(workspace))
    _mock_cargo(
        monkeypatch,
        workspace_manifest,
        stdout=json.dumps(
            {"packages": [{"name": "sample", "source": None, "repository": None}]}
        ),
    )

    with pytest.raises(SystemExit) as caught:
        workspace_manifest.main()

    assert caught.value.code == 1
    assert "cargo metadata: sample has repository=null" in capsys.readouterr().err


@pytest.mark.parametrize(
    "case",
    [
        (
            'tempfile = "3"\n',
            "workspace Cargo.toml missing [workspace.dependencies] tempfile",
        ),
        (
            'assert_cmd = "2"\n',
            "workspace Cargo.toml missing [workspace.dependencies] assert_cmd",
        ),
        (
            'predicates = "3"\n',
            "workspace Cargo.toml missing [workspace.dependencies] predicates",
        ),
    ],
)
def test_rejects_a_missing_shared_workspace_dependency(
    workspace_manifest: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
    case: tuple[str, str],
) -> None:
    missing, message = case
    workspace = _write_workspace(tmp_path, root_text=VALID_ROOT.replace(missing, ""))
    monkeypatch.setenv("CWTOOLS_RS", str(workspace))
    _mock_cargo(monkeypatch, workspace_manifest)

    with pytest.raises(SystemExit) as caught:
        workspace_manifest.main()

    assert caught.value.code == 1
    assert message in capsys.readouterr().err


@pytest.mark.parametrize(
    "case",
    [
        (
            '[workspace.lints.rust]\nwarnings = "deny"\n',
            "workspace Cargo.toml missing [workspace.lints.rust]",
        ),
        (
            '[workspace.lints.clippy]\nall = "deny"\n',
            "workspace Cargo.toml missing [workspace.lints.clippy]",
        ),
    ],
)
def test_rejects_a_missing_workspace_lint_table(
    workspace_manifest: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
    case: tuple[str, str],
) -> None:
    missing, message = case
    workspace = _write_workspace(tmp_path, root_text=VALID_ROOT.replace(missing, ""))
    monkeypatch.setenv("CWTOOLS_RS", str(workspace))
    _mock_cargo(monkeypatch, workspace_manifest)

    with pytest.raises(SystemExit) as caught:
        workspace_manifest.main()

    assert caught.value.code == 1
    assert message in capsys.readouterr().err


def test_dies_when_the_crates_directory_is_missing(
    workspace_manifest: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    workspace = tmp_path / "engine"
    workspace.mkdir()
    (workspace / "Cargo.toml").write_text(VALID_ROOT, encoding="utf-8")
    monkeypatch.setenv("CWTOOLS_RS", str(workspace))
    _mock_cargo(monkeypatch, workspace_manifest)

    with pytest.raises(SystemExit) as caught:
        workspace_manifest.main()

    assert caught.value.code == 2
    assert f"crates dir not found: {workspace / 'crates'}" in capsys.readouterr().err


def test_dies_when_cargo_metadata_fails(
    workspace_manifest: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    workspace = _write_workspace(tmp_path)
    monkeypatch.setenv("CWTOOLS_RS", str(workspace))
    _mock_cargo(monkeypatch, workspace_manifest, returncode=1)

    with pytest.raises(SystemExit) as caught:
        workspace_manifest.main()

    assert caught.value.code == 2
    assert "cargo metadata failed" in capsys.readouterr().err
