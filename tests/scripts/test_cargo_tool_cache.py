"""Regression guards for versioned Cargo tool caches in CI."""

from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
CI = REPO_ROOT / ".github" / "workflows" / "ci.yml"
SETUP_RUST = REPO_ROOT / ".github" / "actions" / "setup-rust" / "action.yml"
CACHE_HIT = "steps.rust-setup.outputs.cargo-tool-cache-hit != 'true'"

TOOLS = {
    "cargo-machete": "0.9.2",
    "cargo-fuzz": "0.13.2",
    "cargo-llvm-cov": "0.8.7",
}


def step(workflow: str, name: str) -> str:
    return workflow.split(f"      - name: {name}\n", maxsplit=1)[1].split(
        "\n      - ", maxsplit=1
    )[0]


def test_setup_rust_separates_registry_and_versioned_tool_caches() -> None:
    action = SETUP_RUST.read_text(encoding="utf-8")
    registry = action.split("    - name: Cache cargo registry\n", maxsplit=1)[1].split(
        "    - name: Cache cargo tool\n", maxsplit=1
    )[0]
    tool = action.split("    - name: Cache cargo tool\n", maxsplit=1)[1].split(
        "    - name: Cache cargo target\n", maxsplit=1
    )[0]

    assert "~/.cargo/registry" in registry
    assert "~/.cargo/git" in registry
    assert "~/.cargo/bin" not in registry
    assert "path: ~/.cargo/bin/${{ inputs.cargo-tool }}" in tool
    assert "${{ runner.os }}-cargo-tool-${{ inputs.cargo-tool }}" in tool
    assert "inputs.cargo-tool-version" in tool
    assert "inputs.toolchain" in tool
    # Never restore an incompatible executable through a broad prefix match.
    assert "restore-keys:" not in tool


def test_ci_cargo_installs_are_pinned_and_skip_on_exact_tool_cache_hit() -> None:
    workflow = CI.read_text(encoding="utf-8")
    expected_installs = {
        "Install cargo-machete": "cargo install cargo-machete --locked --version 0.9.2",
        "Install cargo-fuzz": (
            "cargo +nightly-2026-07-24 install cargo-fuzz --version 0.13.2"
        ),
        "Install cargo-llvm-cov": (
            "cargo install cargo-llvm-cov --locked --version 0.8.7"
        ),
    }

    assert workflow.count("id: rust-setup") == len(TOOLS)
    assert "--force" not in workflow
    for name, install in expected_installs.items():
        install_step = step(workflow, name)
        assert CACHE_HIT in install_step
        assert install in install_step

    for tool, version in TOOLS.items():
        tool_cache_config = (
            f"cargo-tool: {tool}\n          cargo-tool-version: {version}"
        )
        assert tool_cache_config in workflow


def test_setup_rust_exposes_the_tool_cache_hit_to_install_steps() -> None:
    action = SETUP_RUST.read_text(encoding="utf-8")

    assert "cargo-tool-cache-hit:" in action
    assert "value: ${{ steps.cargo-tool-cache.outputs.cache-hit }}" in action
    assert "if: inputs.cargo-tool != '' && inputs.cargo-tool-version != ''" in action
