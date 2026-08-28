from __future__ import annotations

from pathlib import Path
from types import ModuleType


def test_normalize_strips_prefix_and_hash(guard: ModuleType) -> None:
    raw = (
        "file,line,severity,code,message,hash\n"
        "/ws/mod/common/x.txt,10,Warning,CW100,"
        "Localisation key FOO is not defined,0123456789abcdef\n"
    )
    rows = guard.normalize_rows(raw, Path("/ws/mod"))
    assert rows == ["common/x.txt,10,Warning,CW100,Localisation key FOO is not defined"]


def test_normalize_keeps_a_quoted_comma_path(guard: ModuleType) -> None:
    raw = (
        "file,line,severity,code,message,hash\n"
        '"/ws/mod/common/AUS effects (Austria, legacy).txt",105,Information,CW223,'
        '"NOT with, commas",0123456789abcdef\n'
    )
    rows = guard.normalize_rows(raw, Path("/ws/mod"))
    assert rows == [
        '"common/AUS effects (Austria, legacy).txt",105,Information,CW223,'
        '"NOT with, commas"'
    ]


def test_normalize_rewrites_backslashes(guard: ModuleType) -> None:
    raw = (
        "file,line,severity,code,message,hash\n"
        "C:\\ws\\mod\\common\\x.txt,10,Warning,CW100,msg,0123456789abcdef\n"
    )
    rows = guard.normalize_rows(raw, Path("C:/ws/mod"))
    assert rows == ["common/x.txt,10,Warning,CW100,msg"]


def test_report_body_skips_hash_headers(guard: ModuleType) -> None:
    text = "# comment\n# another\nfile,line,severity,code,message\nrow\n"
    assert guard.report_body(text) == ["file,line,severity,code,message", "row"]


def test_default_projects_falls_back_to_the_repo_root_parent(
    guard: ModuleType,
) -> None:
    assert guard.default_projects({}) == guard.REPO_ROOT.parent


def test_default_projects_env_wins(guard: ModuleType) -> None:
    assert guard.default_projects({"CWTOOLS_PROJECTS": "/tmp/projects"}) == Path(
        "/tmp/projects"
    )


def test_md_uses_the_default_corpus(guard: ModuleType) -> None:
    config = guard.build_config(["md"], {"CWTOOLS_PROJECTS": "/tmp/projects"})
    assert config.corpus == Path("/tmp/projects/Millennium-Dawn")
    assert str(config.baseline).endswith("md-baseline.csv")


def test_md_falls_back_to_the_repo_root_parent_without_the_env(
    guard: ModuleType,
) -> None:
    config = guard.build_config(["md"], {})
    assert config.corpus == guard.REPO_ROOT.parent / "Millennium-Dawn"


def test_md_corpus_flag_wins_over_the_env(guard: ModuleType) -> None:
    config = guard.build_config(
        ["md", "--corpus", "/tmp/other"],
        {"CWTOOLS_CORPUS": "/tmp/other-base", "CWTOOLS_PROJECTS": "/tmp/projects"},
    )
    assert config.corpus == Path("/tmp/other")


def test_md_reads_the_env(guard: ModuleType) -> None:
    config = guard.build_config(
        ["md"], {"CWTOOLS_CORPUS": "/tmp/MD", "CWTOOLS_PROJECTS": "/tmp/projects"}
    )
    assert config.corpus == Path("/tmp/MD")
    assert config.game == "hoi4"


def test_vanilla_overrides_the_game_env(guard: ModuleType) -> None:
    config = guard.build_config(["vanilla"], {"CWTOOLS_GAME": "hoi4"})
    assert config.game == "stellaris"
    assert config.corpus == guard.SCRIPT_DIR / "vanilla-fixture" / "mod"
