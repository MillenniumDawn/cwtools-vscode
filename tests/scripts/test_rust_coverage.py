from __future__ import annotations

from pathlib import Path
from types import ModuleType

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]


def test_lcov_summary_records_repo_relative_paths(
    rust_coverage: ModuleType, tmp_path: Path
) -> None:
    workspace = tmp_path / "engine"
    workspace.mkdir()
    lcov = """\
SF:crates/parser/src/lib.rs
DA:10,1
DA:11,0
LF:2
LH:1
FNF:1
FNH:1
BRF:2
BRH:1
end_of_record
SF:crates/empty.rs
LF:0
LH:0
end_of_record
"""
    summary = rust_coverage.lcov_to_summary(lcov, tmp_path, workspace)

    path = "engine/crates/parser/src/lib.rs"
    assert path in summary
    assert "engine/crates/empty.rs" not in summary
    assert summary[path]["lines"]["total"] == 2
    assert summary[path]["lines"]["covered"] == 1
    assert summary[path]["statements"]["total"] == 2
    assert summary[path]["functions"]["total"] == 1
    assert summary[path]["branches"]["covered"] == 1
    assert summary["total"]["lines"]["total"] == 2


def test_lcov_falls_back_to_hit_records(
    rust_coverage: ModuleType, tmp_path: Path
) -> None:
    workspace = tmp_path / "engine"
    lcov = f"""\
SF:{workspace / "src" / "main.rs"}
FN:1,main
FNDA:3,main
DA:1,3
DA:2,0
BRDA:1,0,0,1
BRDA:1,0,1,-
end_of_record
"""
    summary = rust_coverage.lcov_to_summary(lcov, tmp_path, workspace)

    path = "engine/src/main.rs"
    assert summary[path]["lines"]["covered"] == 1
    assert summary[path]["lines"]["total"] == 2
    assert summary[path]["functions"]["covered"] == 1
    assert summary[path]["branches"]["covered"] == 1
    assert summary[path]["branches"]["total"] == 2


def test_lcov_skips_bad_integers(rust_coverage: ModuleType) -> None:
    repo = Path("/repo")
    lcov = """\
SF:crates/x.rs
DA:1,nope
LF:abc
LH:1
DA:2,1
end_of_record
"""
    summary = rust_coverage.lcov_to_summary(lcov, repo, repo / "engine")

    path = "engine/crates/x.rs"
    assert summary[path]["lines"]["total"] == 1
    assert summary[path]["lines"]["covered"] == 1


def test_reset_coverage_outputs_unlinks_existing_files(
    rust_coverage: ModuleType, tmp_path: Path
) -> None:
    coverage_dir = tmp_path / "target" / "coverage"
    coverage_dir.mkdir(parents=True)
    lcov = coverage_dir / "lcov.info"
    summary = coverage_dir / "coverage-summary.json"
    lcov.write_text("SF:stale\nend_of_record\n", encoding="utf-8")
    summary.write_text('{"total": {}}\n', encoding="utf-8")

    returned_lcov, returned_summary = rust_coverage.reset_coverage_outputs(coverage_dir)

    assert returned_lcov == lcov
    assert returned_summary == summary
    assert not lcov.exists()
    assert not summary.exists()


def test_reset_coverage_outputs_creates_a_missing_directory(
    rust_coverage: ModuleType, tmp_path: Path
) -> None:
    coverage_dir = tmp_path / "fresh" / "coverage"

    returned_lcov, returned_summary = rust_coverage.reset_coverage_outputs(coverage_dir)

    assert returned_lcov == coverage_dir / "lcov.info"
    assert returned_summary == coverage_dir / "coverage-summary.json"
    assert coverage_dir.is_dir()
    assert not returned_lcov.exists()
    assert not returned_summary.exists()


def test_reset_coverage_outputs_tolerates_a_clean_directory(
    rust_coverage: ModuleType, tmp_path: Path
) -> None:
    coverage_dir = tmp_path / "empty" / "coverage"
    coverage_dir.mkdir(parents=True)

    rust_coverage.reset_coverage_outputs(coverage_dir)

    assert coverage_dir.is_dir()
    assert not (coverage_dir / "lcov.info").exists()
    assert not (coverage_dir / "coverage-summary.json").exists()


def test_main_clears_outputs_when_llvm_cov_is_missing(
    rust_coverage: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    coverage_dir = tmp_path / "engine" / "target" / "coverage"
    coverage_dir.mkdir(parents=True)
    (coverage_dir / "lcov.info").write_text("stale", encoding="utf-8")
    (coverage_dir / "coverage-summary.json").write_text("{}", encoding="utf-8")

    monkeypatch.setattr(rust_coverage.shutil, "which", lambda _name: None)
    monkeypatch.setattr(
        rust_coverage.os, "environ", {"CWTOOLS_RS": str(tmp_path / "engine")}
    )

    rc = rust_coverage.main()

    assert rc == 1
    assert not (coverage_dir / "lcov.info").exists()
    assert not (coverage_dir / "coverage-summary.json").exists()


def test_main_returns_first_cargo_exit_when_llvm_cov_fails(
    rust_coverage: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """A failing cargo run must clear the stale summary but keep the fresh lcov."""

    coverage_dir = tmp_path / "engine" / "target" / "coverage"
    coverage_dir.mkdir(parents=True)
    stale_lcov = coverage_dir / "lcov.info"
    stale_summary = coverage_dir / "coverage-summary.json"
    stale_lcov.write_text("stale-lcov-from-prior-run", encoding="utf-8")
    stale_summary.write_text("{}", encoding="utf-8")

    fresh_lcov_body = "fresh-lcov-written-by-cargo-before-failing\n"

    class _FakeCompleted:
        returncode = 2

    def fake_run(
        command: object,
        *_args: object,
        **_kwargs: object,
    ) -> object:
        # cargo-llvm-cov writes the lcov before failing the gate; keep it.
        if isinstance(command, list) and "--output-path" in command:
            out_index = command.index("--output-path") + 1
            Path(command[out_index]).write_text(fresh_lcov_body, encoding="utf-8")
        return _FakeCompleted()

    def fake_which(_name: str) -> str:
        return "/usr/bin/cargo-llvm-cov"

    monkeypatch.setattr(rust_coverage.shutil, "which", fake_which)
    monkeypatch.setattr(rust_coverage.subprocess, "run", fake_run)
    monkeypatch.setattr(
        rust_coverage.os, "environ", {"CWTOOLS_RS": str(tmp_path / "engine")}
    )

    rc = rust_coverage.main()

    assert rc == 2
    assert not stale_summary.exists()
    assert stale_lcov.read_text(encoding="utf-8") == fresh_lcov_body


# The default floor must match the value the CI workflow pins.
DEFAULT_LINE_FLOOR = "91.5"


def test_main_uses_the_default_threshold_when_env_is_unset(
    rust_coverage: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    coverage_dir = tmp_path / "engine" / "target" / "coverage"
    coverage_dir.mkdir(parents=True)
    captured: list[list[str]] = []

    class _FakeCompleted:
        returncode = 0

        def __init__(self, command: list[str]) -> None:
            captured.append(list(command))

    def fake_run(
        command: object,
        *_args: object,
        **_kwargs: object,
    ) -> object:
        if not isinstance(command, list):
            return _FakeCompleted([])
        if "--output-path" in command:
            out_index = command.index("--output-path") + 1
            Path(command[out_index]).write_text(
                "TN:\nSF:placeholder\nend_of_record\n", encoding="utf-8"
            )
        return _FakeCompleted(command)

    def fake_which(_name: str) -> str:
        return "/usr/bin/cargo-llvm-cov"

    monkeypatch.setattr(rust_coverage.shutil, "which", fake_which)
    monkeypatch.setattr(rust_coverage.subprocess, "run", fake_run)
    monkeypatch.setattr(
        rust_coverage.os,
        "environ",
        {"CWTOOLS_RS": str(tmp_path / "engine")},
    )

    rc = rust_coverage.main()

    assert rc == 0
    lcov_command = next(cmd for cmd in captured if "--output-path" in cmd)
    fail_under_index = lcov_command.index("--fail-under-lines") + 1
    assert lcov_command[fail_under_index] == DEFAULT_LINE_FLOOR


def test_ci_workflow_pins_the_same_threshold_and_step_name() -> None:
    workflow = (REPO_ROOT / ".github" / "workflows" / "ci.yml").read_text(
        encoding="utf-8"
    )

    assert f"COVERAGE_THRESHOLD: {DEFAULT_LINE_FLOOR}" in workflow
    assert f"Run coverage ({DEFAULT_LINE_FLOOR}% line floor)" in workflow
