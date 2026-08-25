from __future__ import annotations

from pathlib import Path
from types import ModuleType

import pytest


def test_skips_the_cwt_grammar_this_repo_owns(
    sync_paradox_syntax: ModuleType, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    src = tmp_path / "paradox-syntax" / "syntaxes"
    dst = tmp_path / "syntaxes"
    src.mkdir(parents=True)
    dst.mkdir()
    (src / "paradox.tmLanguage.json").write_text("{}\n", encoding="utf-8")
    (src / "cwt.tmLanguage.json").write_text('{"owned":false}\n', encoding="utf-8")
    (dst / "cwt.tmLanguage.json").write_text('{"owned":true}\n', encoding="utf-8")
    monkeypatch.setenv("PARADOX_SYNTAX_SRC", str(tmp_path / "paradox-syntax"))
    monkeypatch.setenv("PARADOX_SYNTAX_DST", str(dst))

    assert sync_paradox_syntax.main() == 0

    kept = (dst / "cwt.tmLanguage.json").read_text(encoding="utf-8")
    assert kept == '{"owned":true}\n'
    assert (dst / "paradox.tmLanguage.json").is_file()
