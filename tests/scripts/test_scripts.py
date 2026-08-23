from __future__ import annotations

import json
import os
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

from load import load_script

coverage_script = load_script("coverage")
guard = load_script("guard")
smoke_test_vsix = load_script("smoke_test_vsix")
stage_release_binaries = load_script("stage_release_binaries")
sync_paradox_syntax = load_script("sync_paradox_syntax")


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
    def test_md_uses_default_corpus(self) -> None:
        cfg = guard.build_config(
            ["md"],
            {
                "CWTOOLS_PROJECTS": "/tmp/projects",
            },
        )
        self.assertEqual(cfg.corpus, Path("/tmp/projects/Millennium-Dawn"))
        self.assertTrue(str(cfg.baseline).endswith("md-baseline.csv"))

    def test_md_corpus_flag_wins(self) -> None:
        cfg = guard.build_config(
            ["md", "--corpus", "/tmp/other"],
            {
                "CWTOOLS_CORPUS": "/tmp/other-base",
                "CWTOOLS_PROJECTS": "/tmp/projects",
            },
        )
        self.assertEqual(cfg.corpus, Path("/tmp/other"))

    def test_vanilla_overrides_game_env(self) -> None:
        cfg = guard.build_config(["vanilla"], {"CWTOOLS_GAME": "hoi4"})
        self.assertEqual(cfg.game, "stellaris")
        self.assertEqual(cfg.corpus, guard.SCRIPT_DIR / "vanilla-fixture" / "mod")

    def test_md_reads_env(self) -> None:
        cfg = guard.build_config(
            ["md"],
            {"CWTOOLS_CORPUS": "/tmp/MD", "CWTOOLS_PROJECTS": "/tmp/projects"},
        )
        self.assertEqual(cfg.corpus, Path("/tmp/MD"))
        self.assertEqual(cfg.game, "hoi4")


def _write_vsix(
    path: Path, platforms: list[str], *, include_entrypoint: bool = True
) -> None:
    package = {
        "main": "./bin/client/extension/extension.js",
        "icon": "media/icon.png",
        "l10n": "./l10n",
        "contributes": {
            "languages": [],
            "grammars": [],
            "themes": [],
            "snippets": [],
        },
    }
    with zipfile.ZipFile(path, "w") as zf:
        zf.writestr("extension/package.json", json.dumps(package))
        for relative in [
            "bin/client/extension/extension.js",
            "bin/client/webview/graph.js",
            "bin/client/webview/site.css",
            "media/icon.png",
            "l10n/bundle.l10n.test.json",
            "readme.md",
            "changelog.md",
            "LICENSE.md",
        ]:
            if relative.endswith("extension.js") and not include_entrypoint:
                continue
            zf.writestr(f"extension/{relative}", "x\n")
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
            _write_vsix(vsix, ["linux-x64"], include_entrypoint=False)
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
            extension = root / "extension"
            self.assertEqual(
                stage_release_binaries.main([str(artifacts), str(extension)]),
                0,
            )
            staged = (
                extension
                / "bin"
                / "server"
                / "cwtools-server"
                / "linux-x64"
                / "cwtools-server"
            )
            self.assertTrue(staged.is_file())


class CoverageSummaryTests(unittest.TestCase):
    def test_lcov_summary_records_and_repo_relative_paths(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            workspace = repo / "engine"
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
            summary = coverage_script.lcov_to_summary(lcov, repo, workspace)
            path = "engine/crates/parser/src/lib.rs"
            self.assertIn(path, summary)
            self.assertNotIn("engine/crates/empty.rs", summary)
            self.assertEqual(summary[path]["lines"]["total"], 2)
            self.assertEqual(summary[path]["lines"]["covered"], 1)
            self.assertEqual(summary[path]["statements"]["total"], 2)
            self.assertEqual(summary[path]["functions"]["total"], 1)
            self.assertEqual(summary[path]["branches"]["covered"], 1)
            self.assertEqual(summary["total"]["lines"]["total"], 2)

    def test_lcov_falls_back_to_hit_records(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            workspace = repo / "engine"
            sf = workspace / "src" / "main.rs"
            lcov = f"""\
SF:{sf}
FN:1,main
FNDA:3,main
DA:1,3
DA:2,0
BRDA:1,0,0,1
BRDA:1,0,1,-
end_of_record
"""
            summary = coverage_script.lcov_to_summary(lcov, repo, workspace)
            path = "engine/src/main.rs"
            self.assertEqual(summary[path]["lines"]["covered"], 1)
            self.assertEqual(summary[path]["lines"]["total"], 2)
            self.assertEqual(summary[path]["functions"]["covered"], 1)
            self.assertEqual(summary[path]["branches"]["covered"], 1)
            self.assertEqual(summary[path]["branches"]["total"], 2)

    def test_lcov_skips_bad_integers(self) -> None:
        repo = Path("/repo")
        workspace = repo / "engine"
        lcov = """\
SF:crates/x.rs
DA:1,nope
LF:abc
LH:1
DA:2,1
end_of_record
"""
        summary = coverage_script.lcov_to_summary(lcov, repo, workspace)
        path = "engine/crates/x.rs"
        self.assertEqual(summary[path]["lines"]["total"], 1)
        self.assertEqual(summary[path]["lines"]["covered"], 1)


class SyncSyntaxTests(unittest.TestCase):
    def test_skips_owned_cwt_grammar(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            src = root / "paradox-syntax" / "syntaxes"
            dst = root / "syntaxes"
            src.mkdir(parents=True)
            dst.mkdir()
            (src / "paradox.tmLanguage.json").write_text("{}\n", encoding="utf-8")
            (src / "cwt.tmLanguage.json").write_text(
                '{"owned":false}\n', encoding="utf-8"
            )
            (dst / "cwt.tmLanguage.json").write_text(
                '{"owned":true}\n', encoding="utf-8"
            )
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
