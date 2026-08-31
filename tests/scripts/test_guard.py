from __future__ import annotations

import subprocess
from pathlib import Path
from types import ModuleType

import pytest

REVISION = "abc1234"


def _config_args(tmp_path: Path, *, bless: bool = False) -> list[str]:
    corpus = tmp_path / "mod"
    rules = tmp_path / "config" / "Config"
    binary = tmp_path / "cwtools"
    corpus.mkdir()
    rules.mkdir(parents=True)
    binary.write_text("stub", encoding="utf-8")
    args = [
        "md",
        "--corpus",
        str(corpus),
        "--rules",
        str(rules),
        "--baseline",
        str(tmp_path / "baseline.csv"),
        "--bin",
        str(binary),
        "--no-build",
    ]
    if bless:
        args.append("--bless")
    return args


def _stub_subprocess(
    monkeypatch: pytest.MonkeyPatch, guard: ModuleType, raw_report: str
) -> None:
    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        if command[0] == "git":
            stdout = f"{REVISION}\n" if "rev-parse" in command else ""
            return subprocess.CompletedProcess(command, 0, stdout=stdout, stderr="")

        assert command[1] == "validate"
        output = Path(command[command.index("--output-file") + 1])
        output.write_text(raw_report, encoding="utf-8")
        return subprocess.CompletedProcess(command, 0)

    monkeypatch.setattr(guard.subprocess, "run", run)


def _fixed_work_dir(
    monkeypatch: pytest.MonkeyPatch, guard: ModuleType, work: Path
) -> None:
    work.mkdir()
    monkeypatch.setattr(guard.tempfile, "mkdtemp", lambda **_kwargs: str(work))


def test_run_guard_clean_match_removes_work_dir(
    guard: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    corpus = tmp_path / "mod"
    raw = (
        "file,line,severity,code,message,hash\n"
        f"{corpus}/common/x.txt,4,Warning,CW100,same message,0123456789abcdef\n"
    )
    (tmp_path / "baseline.csv").write_text(
        "# deliberately stale metadata\n"
        "file,line,severity,code,message\n"
        "common/x.txt,4,Warning,CW100,same message\n",
        encoding="utf-8",
    )
    _stub_subprocess(monkeypatch, guard, raw)
    work = tmp_path / "work"
    _fixed_work_dir(monkeypatch, guard, work)
    config = guard.build_config(_config_args(tmp_path), {})

    assert guard.run_guard(config) == 0
    assert "guard: OK, 1 diagnostics match the baseline" in capsys.readouterr().out
    assert not work.exists()


def test_run_guard_drift_returns_one_and_preserves_useful_diff(
    guard: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    corpus = tmp_path / "mod"
    raw = (
        "file,line,severity,code,message,hash\n"
        f"{corpus}/common/x.txt,4,Warning,CW222,new message,0123456789abcdef\n"
    )
    (tmp_path / "baseline.csv").write_text(
        "# old run\n"
        "file,line,severity,code,message\n"
        "common/x.txt,4,Warning,CW100,old message\n",
        encoding="utf-8",
    )
    _stub_subprocess(monkeypatch, guard, raw)
    work = tmp_path / "work"
    _fixed_work_dir(monkeypatch, guard, work)
    config = guard.build_config(_config_args(tmp_path), {})

    assert guard.run_guard(config) == 1

    output = capsys.readouterr().out
    diff = (work / "drift.diff").read_text(encoding="utf-8")
    assert "guard: FAIL, diagnostics drifted from the baseline" in output
    assert f"guard: artifacts in {work}" in output
    assert "--- baseline" in diff
    assert "+++ current" in diff
    assert "-common/x.txt,4,Warning,CW100,old message" in diff
    assert "+common/x.txt,4,Warning,CW222,new message" in diff
    assert (work / "current.csv").is_file()


def test_run_guard_bless_writes_expected_baseline(
    guard: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    corpus = tmp_path / "mod"
    raw = (
        "file,line,severity,code,message,hash\n"
        f"{corpus}/common/z.txt,9,Warning,CW222,last,0123456789abcdef\n"
        f"{corpus}/common/a.txt,2,Information,CW100,first,fedcba9876543210\n"
    )
    _stub_subprocess(monkeypatch, guard, raw)
    work = tmp_path / "work"
    _fixed_work_dir(monkeypatch, guard, work)
    config = guard.build_config(_config_args(tmp_path, bless=True), {})

    assert guard.run_guard(config) == 0
    assert (tmp_path / "baseline.csv").read_text(encoding="utf-8") == (
        "# cwtools guard baseline. Regenerate with "
        "python3 scripts/guard.py md --bless\n"
        f"# corpus: mod @ {REVISION}\n"
        f"# rules:  config/Config @ {REVISION}\n"
        "file,line,severity,code,message\n"
        "common/a.txt,2,Information,CW100,first\n"
        "common/z.txt,9,Warning,CW222,last\n"
    )
    assert not work.exists()


def test_run_guard_missing_corpus_dies_with_setup_error(
    guard: ModuleType, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    config = guard.build_config(
        [
            "md",
            "--corpus",
            str(tmp_path / "missing-mod"),
            "--rules",
            str(tmp_path / "unused-rules"),
            "--baseline",
            str(tmp_path / "baseline.csv"),
            "--bin",
            str(tmp_path / "cwtools"),
            "--no-build",
        ],
        {},
    )

    with pytest.raises(SystemExit, match="2"):
        guard.run_guard(config)

    error = capsys.readouterr().err
    assert f"guard: corpus not found: {tmp_path / 'missing-mod'}" in error


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
