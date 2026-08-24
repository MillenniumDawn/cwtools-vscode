from __future__ import annotations

from pathlib import Path
from types import ModuleType


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
