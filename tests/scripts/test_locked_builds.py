"""Keep all automation building the engine on its committed lockfile."""

import re
import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]


def test_workflows_lock_engine_compilation() -> None:
    ci = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
    publish = (ROOT / ".github/workflows/publish.yml").read_text(encoding="utf-8")
    bench = (ROOT / ".github/workflows/build-bench.yml").read_text(encoding="utf-8")
    assert "cargo clippy --locked --workspace" in ci
    assert "cargo build --locked --workspace --all-targets --all-features" in ci
    assert "cargo test --locked --workspace --all-features" in ci
    assert 'cargo +"$MSRV" build --locked --workspace' in ci
    # Check every direct engine compilation/test command rather than freezing
    # the number of guard jobs. Additional CI tiers must also be locked.
    commands = re.findall(
        r'(?m)^\s*(?:run:\s*)?cargo (?:\+"\$MSRV"\s+)?(?:build|test|clippy)\b[^\n]*',
        ci,
    )
    assert commands
    assert all("--locked" in command for command in commands)
    assert ci.count("run: cargo build --locked --release -p cwtools_cli") >= 2
    assert "metadata --locked --format-version 1" in ci  # separate fuzz workspace
    assert "cargo build --locked --release -p cwtools_lsp" in publish
    assert "cargo build --locked --workspace --release" in bench


def test_script_wrappers_keep_locked_flags() -> None:
    build = (ROOT / "scripts/build/build.py").read_text(encoding="utf-8")
    coverage = (ROOT / "scripts/coverage.py").read_text(encoding="utf-8")
    guard = (ROOT / "scripts/guard.py").read_text(encoding="utf-8")
    assert '["build", "--locked", "--release", "-p", "cwtools_lsp"]' in build
    assert '"llvm-cov",\n            "--locked",\n            "--workspace"' in coverage
    assert (
        guard.count('["cargo", "build", "--locked", "--release", "-p", "cwtools_cli"]')
        == 2
    )


@pytest.mark.skipif(shutil.which("cargo") is None, reason="Cargo is unavailable")
def test_locked_metadata_rejects_stale_lockfile(tmp_path: Path) -> None:
    manifest = tmp_path / "Cargo.toml"
    manifest.write_text(
        '[package]\nname = "lock-check"\nversion = "0.1.0"\nedition = "2021"\n',
        encoding="utf-8",
    )
    (tmp_path / "src").mkdir()
    (tmp_path / "src/main.rs").write_text("fn main() {}\n", encoding="utf-8")
    subprocess.run(
        ["cargo", "generate-lockfile", "--offline"], cwd=tmp_path, check=True
    )
    lock = tmp_path / "Cargo.lock"
    original = lock.read_bytes()
    manifest.write_text(
        manifest.read_text(encoding="utf-8").replace("0.1.0", "0.1.1"),
        encoding="utf-8",
    )
    result = subprocess.run(
        ["cargo", "metadata", "--locked", "--offline", "--format-version", "1"],
        cwd=tmp_path,
        capture_output=True,
        check=False,
    )
    assert result.returncode != 0
    assert lock.read_bytes() == original
