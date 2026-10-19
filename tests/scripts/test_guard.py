from __future__ import annotations

import os
import subprocess
from pathlib import Path
from types import ModuleType
from typing import TextIO, cast

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


def _compare_args(tmp_path: Path, revision: str | None = None) -> list[str]:
    args = _config_args(tmp_path)
    args.append("--compare")
    if revision is not None:
        args.append(revision)
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


def test_resolve_compare_revision_uses_merge_base_by_default(
    guard: ModuleType, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    commands: list[list[str]] = []

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        commands.append(command)
        return subprocess.CompletedProcess(command, 0, stdout="merge-base-sha\n")

    monkeypatch.setattr(guard.subprocess, "run", run)

    assert guard.resolve_compare_revision(tmp_path, None) == "merge-base-sha"
    assert commands == [["git", "-C", str(tmp_path), "merge-base", "main", "HEAD"]]


def test_run_guard_compare_build_failure_removes_worktree(
    guard: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    config = guard.build_config(_compare_args(tmp_path, "old-revision"), {})
    work = tmp_path / "work"
    _fixed_work_dir(monkeypatch, guard, work)
    commands: list[list[str]] = []

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        commands.append(command)
        if command[0] == "git":
            if "worktree" in command and "add" in command:
                worktree = Path(command[command.index("--detach") + 1])
                worktree.mkdir()
            stdout = "resolved-revision\n" if "rev-parse" in command else ""
            return subprocess.CompletedProcess(command, 0, stdout=stdout, stderr="")
        if command[0] == "cargo":
            return subprocess.CompletedProcess(command, 1)
        raise AssertionError(command)

    monkeypatch.setattr(guard.subprocess, "run", run)

    with pytest.raises(SystemExit, match="2"):
        guard.run_guard(config)

    output = capsys.readouterr().err
    assert "comparison release build failed" in output
    assert any(
        command[:5]
        == [
            "git",
            "-C",
            str(guard.REPO_ROOT),
            "worktree",
            "remove",
        ]
        for command in commands
    )
    assert not work.exists()


def test_run_guard_compare_prints_drift_table_and_cleans_worktree(
    guard: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    corpus = tmp_path / "mod"
    current_binary = tmp_path / "cwtools"
    current_binary.write_text("current", encoding="utf-8")
    old_raw = (
        "file,line,severity,code,message,hash\n"
        f"{corpus}/common/x.txt,4,Warning,CW100,old,0123456789abcdef\n"
    )
    current_raw = (
        "file,line,severity,code,message,hash\n"
        f"{corpus}/common/x.txt,4,Warning,CW222,new,0123456789abcdef\n"
    )
    config = guard.build_config(_compare_args(tmp_path, "old-revision"), {})
    work = tmp_path / "work"
    _fixed_work_dir(monkeypatch, guard, work)
    monkeypatch.setattr(guard, "describe", lambda _directory: REVISION)

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        if command[0] == "git":
            if "worktree" in command and "add" in command:
                worktree = Path(command[command.index("--detach") + 1])
                binary = worktree / "engine" / "target" / "release" / "cwtools"
                binary.parent.mkdir(parents=True)
                binary.write_text("old", encoding="utf-8")
            stdout = "resolved-revision\n" if "rev-parse" in command else ""
            return subprocess.CompletedProcess(command, 0, stdout=stdout, stderr="")
        if command[0] == "cargo":
            return subprocess.CompletedProcess(command, 0)
        output = Path(command[command.index("--output-file") + 1])
        raw = old_raw if "comparison-worktree" in command[0] else current_raw
        output.write_text(raw, encoding="utf-8")
        return subprocess.CompletedProcess(command, 0)

    monkeypatch.setattr(guard.subprocess, "run", run)

    assert guard.run_guard(config) == 1

    output = capsys.readouterr().out
    assert "diagnostics drifted from comparison revision" in output
    assert "by code (gone/new):" in output
    assert "CW100      -1 +0" in output
    assert "CW222      -0 +1" in output
    assert f"guard: artifacts in {work}" in output
    assert not (work / "comparison-worktree").exists()
    assert (work / "drift.diff").is_file()


@pytest.mark.parametrize("compare", [False, True])
@pytest.mark.parametrize("inherit_cache", [False, True])
def test_validate_runs_use_separate_empty_caches(
    guard: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    compare: bool,
    inherit_cache: bool,
) -> None:
    args = _compare_args(tmp_path, REVISION) if compare else _config_args(tmp_path)
    config = guard.build_config(args, {})
    (tmp_path / "baseline.csv").write_text(
        "file,line,severity,code,message\n", encoding="utf-8"
    )
    work = tmp_path / "work"
    _fixed_work_dir(monkeypatch, guard, work)
    monkeypatch.setattr(guard, "describe", lambda _directory: REVISION)
    inherited = tmp_path / "user-cache"
    inherited.mkdir()
    sentinel = inherited / "keep"
    sentinel.write_bytes(b"user cache")
    for key in ("XDG_CACHE_HOME", "LOCALAPPDATA"):
        if inherit_cache:
            monkeypatch.setenv(key, str(inherited))
        else:
            monkeypatch.delenv(key, raising=False)
    monkeypatch.setenv("CWTOOLS_CACHE_TEST", "preserved")
    original_env = dict(os.environ)
    caches: list[Path] = []

    def run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        if command[0] == "git":
            if "add" in command:
                binary = work / "comparison-worktree/engine/target/release/cwtools"
                binary.parent.mkdir(parents=True)
                binary.write_text("comparison", encoding="utf-8")
            return subprocess.CompletedProcess(command, 0, stdout=f"{REVISION}\n")
        if command[0] == "cargo":
            return subprocess.CompletedProcess(command, 0)

        assert command[1] == "validate"
        env = cast(dict[str, str], kwargs["env"])
        assert env["CWTOOLS_CACHE_TEST"] == "preserved"
        cache = Path(env["XDG_CACHE_HOME"])
        assert env["LOCALAPPDATA"] == str(cache)
        assert cache.is_relative_to(work)
        assert cache.is_dir()
        assert not list(cache.iterdir())
        assert cache not in caches
        caches.append(cache)
        (cache / "parsed-by-this-binary").write_text(command[0], encoding="utf-8")
        output = Path(command[command.index("--output-file") + 1])
        output.write_text("file,line,severity,code,message,hash\n", encoding="utf-8")
        return subprocess.CompletedProcess(command, 0)

    monkeypatch.setattr(guard.subprocess, "run", run)

    assert guard.run_guard(config) == 0
    assert len(caches) == (2 if compare else 1)
    assert dict(os.environ) == original_env
    assert list(inherited.iterdir()) == [sentinel]
    assert sentinel.read_bytes() == b"user cache"
    assert not work.exists()


def test_run_guard_missing_pins_are_compatible(
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
    output = capsys.readouterr().out
    assert "guard: OK, 1 diagnostics match the baseline" in output
    assert "input revisions differ" not in output
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
        "# cwtools guard baseline\n"
        "# corpus: mod @ old-corpus\n"
        "# rules:  config/Config @ old-rules\n"
        "file,line,severity,code,message\n"
        "common/x.txt,4,Warning,CW100,old message\n",
        encoding="utf-8",
    )
    _stub_subprocess(monkeypatch, guard, raw)
    monkeypatch.setattr(
        guard,
        "describe",
        lambda directory: (
            "current-corpus" if directory.name == "mod" else "current-rules"
        ),
    )
    work = tmp_path / "work"
    _fixed_work_dir(monkeypatch, guard, work)
    config = guard.build_config(_config_args(tmp_path), {})

    assert guard.run_guard(config) == 1

    output = capsys.readouterr().out
    diff = (work / "drift.diff").read_text(encoding="utf-8")
    assert "guard: FAIL, diagnostics drifted from the baseline" in output
    warning = (
        "warning: baseline input revisions differ from current inputs "
        "(corpus: baseline old-corpus, current current-corpus; rules: baseline "
        "old-rules, current current-rules); this diff may be input drift, not your "
        "change"
    )
    assert warning in output
    assert output.index(warning) < output.index("by code (gone/new):")
    assert "for a current-input before-baseline, use --baseline <path>" in output
    assert "--compare" not in output
    assert f"guard: artifacts in {work}" in output
    assert "--- baseline" in diff
    assert "+++ current" in diff
    assert "-common/x.txt,4,Warning,CW100,old message" in diff
    assert "+common/x.txt,4,Warning,CW222,new message" in diff
    assert (work / "current.csv").is_file()


def test_run_guard_matching_clean_pins_are_silent(
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
        "# cwtools guard baseline\n"
        f"# corpus: mod @ {REVISION}\n"
        f"# rules:  config/Config @ {REVISION}\n"
        "file,line,severity,code,message\n"
        "common/x.txt,4,Warning,CW100,same message\n",
        encoding="utf-8",
    )
    _stub_subprocess(monkeypatch, guard, raw)
    work = tmp_path / "work"
    _fixed_work_dir(monkeypatch, guard, work)
    config = guard.build_config(_config_args(tmp_path), {})

    assert guard.run_guard(config) == 0

    output = capsys.readouterr().out
    assert "guard: OK, 1 diagnostics match the baseline" in output
    assert "input revisions differ" not in output


def test_run_guard_pass_with_mismatched_pins_prints_one_line_notice(
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
        "# cwtools guard baseline\n"
        "# corpus: mod @ old-corpus\n"
        "# rules:  config/Config @ old-rules\n"
        "file,line,severity,code,message\n"
        "common/x.txt,4,Warning,CW100,same message\n",
        encoding="utf-8",
    )
    _stub_subprocess(monkeypatch, guard, raw)
    monkeypatch.setattr(
        guard,
        "describe",
        lambda directory: (
            "current-corpus" if directory.name == "mod" else "current-rules"
        ),
    )
    work = tmp_path / "work"
    _fixed_work_dir(monkeypatch, guard, work)
    config = guard.build_config(_config_args(tmp_path), {})

    assert guard.run_guard(config) == 0

    output = capsys.readouterr().out
    notice = (
        "guard: note: baseline input revisions differ from current inputs "
        "(corpus: baseline old-corpus, current current-corpus; rules: baseline "
        "old-rules, current current-rules)"
    )
    assert notice in output.splitlines()


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


def test_run_guard_bless_previews_diagnostic_changes_before_replacing(
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
    baseline = tmp_path / "baseline.csv"
    baseline.write_text(
        "# cwtools guard baseline\n"
        f"# corpus: mod @ {REVISION}\n"
        f"# rules:  config/Config @ {REVISION}\n"
        "file,line,severity,code,message\n"
        "common/x.txt,4,Warning,CW100,old message\n",
        encoding="utf-8",
    )
    _stub_subprocess(monkeypatch, guard, raw)
    work = tmp_path / "work"
    _fixed_work_dir(monkeypatch, guard, work)
    config = guard.build_config(_config_args(tmp_path, bless=True), {})
    original_write_text = Path.write_text
    output_before_write: list[str] = []

    def check_preview_precedes_write(
        path: Path,
        data: str,
        encoding: str | None = None,
        errors: str | None = None,
        newline: str | None = None,
    ) -> int:
        if path == baseline:
            shown = capsys.readouterr().out
            assert "-common/x.txt,4,Warning,CW100,old message" in shown
            assert "+common/x.txt,4,Warning,CW222,new message" in shown
            assert "full diff:" in shown
            assert "old message" in baseline.read_text(encoding="utf-8")
            output_before_write.append(shown)
        return original_write_text(
            path, data, encoding=encoding, errors=errors, newline=newline
        )

    monkeypatch.setattr(Path, "write_text", check_preview_precedes_write)

    assert guard.run_guard(config) == 0

    assert output_before_write
    output = output_before_write[0] + capsys.readouterr().out
    diff = (work / "bless.diff").read_text(encoding="utf-8")
    assert "guard: re-blessing " in output
    assert "(1 -> 1 diagnostics; -1 +1 rows)" in output
    assert "-common/x.txt,4,Warning,CW100,old message" in output
    assert "+common/x.txt,4,Warning,CW222,new message" in output
    assert f"full diff: {work / 'bless.diff'}" in output
    assert "-common/x.txt,4,Warning,CW100,old message" in diff
    assert "common/x.txt,4,Warning,CW222,new message" in baseline.read_text(
        encoding="utf-8"
    )
    assert work.exists()


def test_run_guard_bless_reports_pin_only_changes(
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
    baseline = tmp_path / "baseline.csv"
    baseline.write_text(
        "# cwtools guard baseline\n"
        "# corpus: mod @ old-corpus\n"
        "# rules:  config/Config @ old-rules\n"
        "file,line,severity,code,message\n"
        "common/x.txt,4,Warning,CW100,same message\n",
        encoding="utf-8",
    )
    _stub_subprocess(monkeypatch, guard, raw)
    monkeypatch.setattr(
        guard,
        "describe",
        lambda directory: (
            "current-corpus" if directory.name == "mod" else "current-rules"
        ),
    )
    work = tmp_path / "work"
    _fixed_work_dir(monkeypatch, guard, work)
    config = guard.build_config(_config_args(tmp_path, bless=True), {})

    assert guard.run_guard(config) == 0

    lines = capsys.readouterr().out.splitlines()
    assert f"guard: blessed {baseline} (1 -> 1 diagnostics); no row changes" in lines
    assert (
        "  input revisions: corpus: baseline old-corpus, current current-corpus; "
        "rules: baseline old-rules, current current-rules"
    ) in lines
    assert "# corpus: mod @ current-corpus" in baseline.read_text(encoding="utf-8")
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
    assert "CWTOOLS_CORPUS" in error
    assert "--corpus" in error
    assert "Millennium-Dawn" in error
    assert "CWTOOLS_PROJECTS" in error


def test_run_guard_missing_rules_names_overrides_and_checkout(
    guard: ModuleType, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    config = guard.build_config(
        [
            "md",
            "--corpus",
            str(tmp_path / "mod"),
            "--rules",
            str(tmp_path / "missing-rules"),
            "--baseline",
            str(tmp_path / "baseline.csv"),
            "--bin",
            str(tmp_path / "cwtools"),
            "--no-build",
        ],
        {},
    )
    config.corpus.mkdir()

    with pytest.raises(SystemExit, match="2"):
        guard.run_guard(config)

    error = capsys.readouterr().err
    assert f"guard: rules not found: {tmp_path / 'missing-rules'}" in error
    assert "CWTOOLS_RULES" in error
    assert "--rules" in error
    assert "cwtools-hoi4-config" in error
    assert "CWTOOLS_PROJECTS" in error


def test_run_guard_missing_vanilla_fixture_does_not_recommend_ignored_env(
    guard: ModuleType, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    script_dir = tmp_path / "scripts"
    fixture = script_dir / "vanilla-fixture"
    (fixture / "mod").mkdir(parents=True)
    (fixture / "rules").mkdir()
    config = guard.build_config(
        ["vanilla"],
        {"CWTOOLS_VANILLA": str(tmp_path / "ignored")},
        script_dir=script_dir,
    )

    with pytest.raises(SystemExit, match="2"):
        guard.run_guard(config)

    error = capsys.readouterr().err
    assert f"guard: vanilla not found: {fixture / 'vanilla'}" in error
    assert "--vanilla" in error
    assert "vanilla-fixture/vanilla" in error
    assert "CWTOOLS_VANILLA" not in error


def test_run_guard_timeout_keeps_log_and_prints_tail(
    guard: ModuleType,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    config = guard.build_config(_config_args(tmp_path), {})
    work = tmp_path / "work"
    _fixed_work_dir(monkeypatch, guard, work)
    monkeypatch.setattr(guard, "describe", lambda _directory: REVISION)

    def timeout(
        command: list[str], **kwargs: object
    ) -> subprocess.CompletedProcess[str]:
        assert kwargs["timeout"] == guard.VALIDATE_TIMEOUT_SECONDS
        log = cast(TextIO, kwargs["stdout"])
        for index in range(45):
            log.write(f"line {index}\n")
        raise subprocess.TimeoutExpired(command, guard.VALIDATE_TIMEOUT_SECONDS)

    monkeypatch.setattr(guard.subprocess, "run", timeout)

    with pytest.raises(SystemExit, match="2"):
        guard.run_guard(config)

    captured = capsys.readouterr()
    assert "validate timed out after" in captured.err
    assert "last 40 lines of validate.log" in captured.err
    assert "line 44" in captured.err
    assert "\nline 4\n" not in captured.err
    assert f"guard: artifacts in {work}" in captured.out
    assert (work / "validate.log").read_text(encoding="utf-8") == "".join(
        f"line {index}\n" for index in range(45)
    )


def test_normalize_strips_prefix_and_hash(guard: ModuleType) -> None:
    raw = (
        "file,line,severity,code,message,hash\n"
        "/ws/mod/common/x.txt,10,Warning,CW100,"
        "Localisation key FOO is not defined,0123456789abcdef\n"
    )
    rows = guard.normalize_rows(raw, Path("/ws/mod"))
    assert rows == ["common/x.txt,10,Warning,CW100,Localisation key FOO is not defined"]


def test_normalize_strips_rules_prefix_and_hash(guard: ModuleType) -> None:
    raw = (
        "file,line,severity,code,message,hash\n"
        "/ws/rules/config/effects.cwt,18,Error,CW601,"
        "rule references undefined type `modifier`,0123456789abcdef\n"
    )
    rows = guard.normalize_rows(raw, Path("/ws/mod"), Path("/ws/rules/config"))
    assert rows == [
        "effects.cwt,18,Error,CW601,rule references undefined type `modifier`"
    ]


def test_normalize_keeps_rules_path_without_rules_root(guard: ModuleType) -> None:
    raw = (
        "file,line,severity,code,message,hash\n"
        "/ws/rules/config/effects.cwt,18,Error,CW601,"
        "rule references undefined type `modifier`,0123456789abcdef\n"
    )
    rows = guard.normalize_rows(raw, Path("/ws/mod"))
    assert rows == [
        "/ws/rules/config/effects.cwt,18,Error,CW601,"
        "rule references undefined type `modifier`"
    ]


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


def test_parse_pins_reads_known_leading_headers(guard: ModuleType) -> None:
    text = (
        "# cwtools guard baseline\n"
        "# corpus: mod @ abc1234\n"
        "# rules:  config/Config @ def5678\n"
        "# vanilla: vanilla @ 9876543\n"
        "file,line,severity,code,message\n"
        "# rules: ignored @ later\n"
    )
    assert guard.parse_pins(text) == {
        "corpus": "abc1234",
        "rules": "def5678",
        "vanilla": "9876543",
    }


def test_compare_pins_treats_matching_dirty_revision_as_mismatch(
    guard: ModuleType,
) -> None:
    assert guard.compare_pins(
        {"rules": "abc1234 (dirty)"}, {"rules": "abc1234 (dirty)"}
    ) == [("rules", "abc1234 (dirty)", "abc1234 (dirty)")]


def test_full_revision_marks_a_dirty_checkout(
    guard: ModuleType, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    revision = "a" * 40
    commands: list[list[str]] = []

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        commands.append(command)
        stdout = f"{revision}\n" if "rev-parse" in command else " M mod.txt\n"
        return subprocess.CompletedProcess(command, 0, stdout=stdout)

    monkeypatch.setattr(guard.subprocess, "run", run)

    assert guard.full_revision(tmp_path) == f"{revision} (dirty)"
    assert commands == [
        ["git", "-C", str(tmp_path), "rev-parse", "HEAD"],
        ["git", "-C", str(tmp_path), "status", "--porcelain"],
    ]


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


def test_stellaris_uses_the_pinned_corpus_defaults(guard: ModuleType) -> None:
    config = guard.build_config(["stellaris"], {"CWTOOLS_PROJECTS": "/tmp/projects"})
    assert config.corpus == Path(
        "/tmp/projects/stellaris-ui-prescripted-countries/ui-prescripted-countries"
    )
    assert config.rules == Path("/tmp/projects/cwtools-stellaris-config/config")
    assert config.game == "stellaris"
    assert config.baseline == guard.SCRIPT_DIR / "stellaris-baseline.csv"


def test_stellaris_honors_explicit_input_overrides(guard: ModuleType) -> None:
    config = guard.build_config(
        ["stellaris", "--corpus", "/tmp/mod", "--rules", "/tmp/rules"],
        {
            "CWTOOLS_CORPUS": "/tmp/env-mod",
            "CWTOOLS_RULES": "/tmp/env-rules",
            "CWTOOLS_GAME": "other-game",
            "CWTOOLS_BASELINE": "/tmp/baseline.csv",
            "CWTOOLS_PROJECTS": "/tmp/projects",
        },
    )
    assert config.corpus == Path("/tmp/mod")
    assert config.rules == Path("/tmp/rules")
    assert config.game == "other-game"
    assert config.baseline == Path("/tmp/baseline.csv")


def test_stellaris_baseline_records_sources_and_full_revisions(
    guard: ModuleType, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(guard, "full_revision", lambda _directory: "a" * 40)
    config = guard.build_config(["stellaris"], {})
    baseline = guard.compose_current(
        ["mod.txt,1,Error,CW100,message"], config, "c0ffee0", "rules123", None
    )

    assert "# corpus: ui-prescripted-countries @ c0ffee0" in baseline
    assert (
        "# corpus-source: https://github.com/hmlendea/stellaris-ui-prescripted-countries"
        in baseline
    )
    assert "# corpus-license: GPL-3.0; scoped to ui-prescripted-countries/" in baseline
    assert f"# corpus-commit: {'a' * 40}" in baseline
    assert (
        "# rules-source: https://github.com/cwtools/cwtools-stellaris-config"
        in baseline
    )
    assert f"# rules-commit: {'a' * 40}" in baseline
    assert baseline.endswith(
        "file,line,severity,code,message\nmod.txt,1,Error,CW100,message\n"
    )


def test_vanilla_overrides_the_game_env(guard: ModuleType) -> None:
    config = guard.build_config(["vanilla"], {"CWTOOLS_GAME": "hoi4"})
    assert config.game == "stellaris"
    assert config.corpus == guard.SCRIPT_DIR / "vanilla-fixture" / "mod"
