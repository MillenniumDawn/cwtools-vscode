#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import os
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from types import ModuleType
from unittest.mock import patch


def _load(name: str) -> ModuleType:
    path = Path(__file__).with_name(f"{name}.py")
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise ImportError(name)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


guard = _load("guard")
smoke_test_vsix = _load("smoke_test_vsix")
stage_release_binaries = _load("stage_release_binaries")
sync_paradox_syntax = _load("sync_paradox_syntax")


class GuardNormalizeTests(unittest.TestCase):
    def test_strips_prefix_and_hash(self) -> None:
        corpus = Path("/ws/mod")
        raw = (
            "file,line,severity,code,message,hash\n"
            "/ws/mod/common/x.txt,10,Warning,CW100,Localisation key FOO is not defined,0123456789abcdef\n"
        )
        rows = guard.normalize_rows(raw, corpus)
        self.assertEqual(
            rows,
            ["common/x.txt,10,Warning,CW100,Localisation key FOO is not defined"],
        )

    def test_quoted_comma_path(self) -> None:
        corpus = Path("/ws/mod")
        raw = (
            "file,line,severity,code,message,hash\n"
            '"/ws/mod/common/AUS effects (Austria, legacy).txt",105,Information,CW223,'
            '"NOT with, commas",0123456789abcdef\n'
        )
        rows = guard.normalize_rows(raw, corpus)
        self.assertEqual(
            rows,
            [
                '"common/AUS effects (Austria, legacy).txt",105,Information,CW223,"NOT with, commas"'
            ],
        )

    def test_backslash_normalized(self) -> None:
        corpus = Path("C:/ws/mod")
        raw = (
            "file,line,severity,code,message,hash\n"
            "C:\\ws\\mod\\common\\x.txt,10,Warning,CW100,msg,0123456789abcdef\n"
        )
        rows = guard.normalize_rows(raw, corpus)
        self.assertEqual(rows, ["common/x.txt,10,Warning,CW100,msg"])

    def test_body_skips_hash_headers(self) -> None:
        text = "# comment\n# another\nfile,line,severity,code,message\nrow\n"
        self.assertEqual(
            guard.report_body(text),
            ["file,line,severity,code,message", "row"],
        )


class GuardConfigTests(unittest.TestCase):
    def test_md_ignores_corpus_env(self) -> None:
        cfg = guard.build_config(
            ["md"],
            {
                "CWTOOLS_CORPUS": "/tmp/Kaiserreich-4-Development",
                "CWTOOLS_PROJECTS": "/tmp/projects",
            },
        )
        self.assertEqual(cfg.corpus, Path("/tmp/projects/Millennium-Dawn"))
        self.assertTrue(str(cfg.baseline).endswith("md-baseline.csv"))

    def test_md_corpus_flag_wins(self) -> None:
        cfg = guard.build_config(
            ["md", "--corpus", "/tmp/other"],
            {
                "CWTOOLS_CORPUS": "/tmp/Kaiserreich-4-Development",
                "CWTOOLS_PROJECTS": "/tmp/projects",
            },
        )
        self.assertEqual(cfg.corpus, Path("/tmp/other"))

    def test_vanilla_overrides_game_env(self) -> None:
        cfg = guard.build_config(["vanilla"], {"CWTOOLS_GAME": "hoi4"})
        self.assertEqual(cfg.game, "stellaris")
        self.assertEqual(cfg.corpus, guard.SCRIPT_DIR / "vanilla-fixture" / "mod")

    def test_corpus_reads_env(self) -> None:
        cfg = guard.build_config(
            ["corpus"],
            {"CWTOOLS_CORPUS": "/tmp/KR", "CWTOOLS_PROJECTS": "/tmp/projects"},
        )
        self.assertEqual(cfg.corpus, Path("/tmp/KR"))
        self.assertEqual(cfg.game, "hoi4")


def _write_vsix(path: Path, platforms: list[str], package: str = "{}") -> None:
    with zipfile.ZipFile(path, "w") as zf:
        zf.writestr("extension/package.json", package)
        zf.writestr("extension/bin/client/extension/extension.js", "console.log(1)\n")
        for platform in platforms:
            zf.writestr(
                f"extension/bin/server/cwtools-server/{platform}/cwtools-server",
                "binary\n",
            )


class SmokeTestTests(unittest.TestCase):
    def test_targeted_and_universal(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            _write_vsix(directory / "ext-linux-x64-1.0.0.vsix", ["linux-x64"])
            _write_vsix(directory / "ext-1.0.0.vsix", ["linux-x64", "win-x64"])
            _write_vsix(directory / "ext-win32-x64-1.0.0.vsix", ["win-x64"])
            self.assertEqual(
                smoke_test_vsix.main([str(directory), "linux-x64", "win-x64"]),
                0,
            )

    def test_missing_entrypoint(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            vsix = directory / "ext-1.0.0.vsix"
            with zipfile.ZipFile(vsix, "w") as zf:
                zf.writestr("extension/package.json", "{}")
                zf.writestr(
                    "extension/bin/server/cwtools-server/linux-x64/cwtools-server",
                    "x",
                )
            with self.assertRaises(SystemExit) as ctx:
                smoke_test_vsix.main([str(directory)])
            self.assertEqual(ctx.exception.code, 1)

    def test_wrong_platform(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            _write_vsix(directory / "ext-linux-x64-1.0.0.vsix", ["win-x64"])
            with self.assertRaises(SystemExit) as ctx:
                smoke_test_vsix.main([str(directory)])
            self.assertEqual(ctx.exception.code, 1)


class StageReleaseTests(unittest.TestCase):
    def test_stages_platform_dir(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            artifacts = root / "artifacts"
            binary = artifacts / "server-linux-x64" / "cwtools-server"
            binary.parent.mkdir(parents=True)
            binary.write_text("x\n", encoding="utf-8")
            release = root / "release"
            self.assertEqual(
                stage_release_binaries.main([str(artifacts), str(release)]),
                0,
            )
            staged = release / "bin" / "server" / "cwtools-server" / "linux-x64" / "cwtools-server"
            self.assertTrue(staged.is_file())


class SyncSyntaxTests(unittest.TestCase):
    def test_skips_owned_cwt_grammar(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            src = root / "paradox-syntax" / "syntaxes"
            dst = root / "syntaxes"
            src.mkdir(parents=True)
            dst.mkdir()
            (src / "paradox.tmLanguage.json").write_text("{}\n", encoding="utf-8")
            (src / "cwt.tmLanguage.json").write_text('{"owned":false}\n', encoding="utf-8")
            (dst / "cwt.tmLanguage.json").write_text('{"owned":true}\n', encoding="utf-8")
            with patch.dict(
                os.environ,
                {
                    "PARADOX_SYNTAX_SRC": str(root / "paradox-syntax"),
                    "PARADOX_SYNTAX_DST": str(dst),
                },
            ):
                self.assertEqual(sync_paradox_syntax.main(), 0)
            self.assertEqual(
                (dst / "cwt.tmLanguage.json").read_text(encoding="utf-8"),
                '{"owned":true}\n',
            )
            self.assertTrue((dst / "paradox.tmLanguage.json").is_file())


if __name__ == "__main__":
    unittest.main()
