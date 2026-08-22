#!/usr/bin/env python3

from __future__ import annotations

import argparse
import csv
import difflib
import io
import os
import re
import shutil
import subprocess
import sys
import tempfile
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import NoReturn

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent
HASH_RE = re.compile(r"^[0-9a-f]{16}$")
CODE_RE = re.compile(r",CW[0-9]{3},")
COLUMN_HEADER = "file,line,severity,code,message"


@dataclass
class Config:
    preset: str
    corpus: Path
    rules: Path
    vanilla: Path | None
    baseline: Path
    bin: Path
    game: str
    build: bool
    bless: bool
    repo_root: Path
    script_dir: Path


def csv_escape(s: str) -> str:
    if any(c in s for c in ',"\n'):
        return '"' + s.replace('"', '""') + '"'
    return s


def relativize_file(file: str, corpus: Path) -> str:
    file_slash = file.replace("\\", "/")
    roots = {
        str(corpus).replace("\\", "/").rstrip("/"),
        corpus.as_posix().rstrip("/"),
        str(corpus.resolve()).replace("\\", "/").rstrip("/"),
    }
    for root in sorted(roots, key=len, reverse=True):
        prefix = root + "/"
        if file_slash.startswith(prefix):
            return file_slash[len(prefix) :]
        if file_slash == root:
            return ""
    return file_slash


def normalize_rows(raw: str, corpus: Path) -> list[str]:
    lines = raw.splitlines()
    if not lines:
        return []
    reader = csv.reader(io.StringIO("\n".join(lines[1:])))
    rows: list[str] = []
    for row in reader:
        if not row:
            continue
        if HASH_RE.match(row[-1]):
            row = row[:-1]
        if not row:
            continue
        row[0] = relativize_file(row[0], corpus)
        rows.append(",".join(csv_escape(c) for c in row))
    rows.sort()
    return rows


def report_body(text: str) -> list[str]:
    seen = False
    out: list[str] = []
    for line in text.splitlines():
        if seen or not line.startswith("#"):
            seen = True
            out.append(line)
    return out


def describe(directory: Path) -> str:
    sha = subprocess.run(
        ["git", "-C", str(directory), "rev-parse", "--short", "HEAD"],
        capture_output=True,
        text=True,
        check=False,
    )
    if sha.returncode != 0:
        return "not a git checkout"
    sha_s = sha.stdout.strip()
    dirty = subprocess.run(
        ["git", "-C", str(directory), "status", "--porcelain"],
        capture_output=True,
        text=True,
        check=False,
    )
    if dirty.returncode == 0 and dirty.stdout.strip():
        return f"{sha_s} (dirty)"
    return sha_s


def resolve_bin(bin_path: Path) -> Path:
    if bin_path.exists():
        return bin_path
    if os.name == "nt":
        exe = bin_path.with_suffix(".exe")
        if exe.exists():
            return exe
    return bin_path


def default_projects(env: Mapping[str, str]) -> Path:
    raw = env.get("CWTOOLS_PROJECTS")
    if raw:
        return Path(raw)
    return Path.home() / "Documents" / "github-projects"


def build_config(
    argv: list[str],
    env: Mapping[str, str],
    script_dir: Path | None = None,
    repo_root: Path | None = None,
) -> Config:
    script_dir = script_dir or SCRIPT_DIR
    repo_root = repo_root or REPO_ROOT
    projects = default_projects(env)

    parser = argparse.ArgumentParser(
        prog="guard.py",
        description=(
            "Validate a pinned mod and diff the report against a committed "
            "baseline. Exit 0 if it matches, 1 if it drifted, 2 on setup failure."
        ),
    )
    parser.add_argument(
        "preset",
        nargs="?",
        choices=("md", "vanilla"),
        default="md",
        help="md (Millennium Dawn, default) or vanilla (fixture)",
    )
    parser.add_argument("--corpus", help="mod corpus to validate (env CWTOOLS_CORPUS)")
    parser.add_argument("--rules", help=".cwt ruleset directory (env CWTOOLS_RULES)")
    parser.add_argument("--vanilla", help="base game to index (env CWTOOLS_VANILLA)")
    parser.add_argument(
        "--baseline", help="committed baseline report (env CWTOOLS_BASELINE)"
    )
    parser.add_argument("--bin", help="cwtools binary (env CWTOOLS_BIN)")
    parser.add_argument("--game", help="game id, default hoi4 (env CWTOOLS_GAME)")
    parser.add_argument(
        "--no-build",
        action="store_true",
        help="use --bin as-is instead of rebuilding it first",
    )
    parser.add_argument(
        "--bless",
        action="store_true",
        help="overwrite the baseline with this run's report",
    )
    args = parser.parse_args(argv)

    corpus = Path(
        args.corpus
        or env.get("CWTOOLS_CORPUS")
        or (projects / "Millennium-Dawn")
    )
    rules = Path(
        args.rules
        or env.get("CWTOOLS_RULES")
        or (projects / "cwtools-hoi4-config" / "Config")
    )
    vanilla_raw = (
        args.vanilla if args.vanilla is not None else env.get("CWTOOLS_VANILLA")
    )
    vanilla = Path(vanilla_raw) if vanilla_raw else None
    baseline = Path(
        args.baseline
        or env.get("CWTOOLS_BASELINE")
        or (script_dir / "md-baseline.csv")
    )
    bin_path = Path(
        args.bin
        or env.get("CWTOOLS_BIN")
        or (repo_root / "engine" / "target" / "release" / "cwtools")
    )
    game = args.game or env.get("CWTOOLS_GAME") or "hoi4"

    if args.preset == "vanilla":
        fixture = script_dir / "vanilla-fixture"
        if args.game is None:
            game = "stellaris"
        if args.corpus is None:
            corpus = fixture / "mod"
        if args.rules is None:
            rules = fixture / "rules"
        if args.vanilla is None:
            vanilla = fixture / "vanilla"
        if args.baseline is None:
            baseline = (
                Path(env["CWTOOLS_BASELINE"])
                if env.get("CWTOOLS_BASELINE")
                else script_dir / "vanilla-baseline.csv"
            )

    return Config(
        preset=args.preset,
        corpus=corpus,
        rules=rules,
        vanilla=vanilla,
        baseline=baseline,
        bin=resolve_bin(bin_path),
        game=game,
        build=not args.no_build,
        bless=args.bless,
        repo_root=repo_root,
        script_dir=script_dir,
    )


def die(message: str) -> NoReturn:
    print(f"guard: {message}", file=sys.stderr)
    raise SystemExit(2)


def compose_current(
    rows: list[str],
    config: Config,
    corpus_rev: str,
    rules_rev: str,
    vanilla_rev: str | None,
) -> str:
    rules_label = f"{config.rules.parent.name}/{config.rules.name}"
    lines = [
        f"# cwtools guard baseline. Regenerate with python3 scripts/guard.py {config.preset} --bless",
        f"# corpus: {config.corpus.name} @ {corpus_rev}",
        f"# rules:  {rules_label} @ {rules_rev}",
    ]
    if config.vanilla is not None and vanilla_rev is not None:
        lines.append(f"# vanilla: {config.vanilla.name} @ {vanilla_rev}")
    lines.append(COLUMN_HEADER)
    lines.extend(rows)
    return "\n".join(lines) + "\n"


def code_from_diff_line(line: str) -> str:
    match = CODE_RE.search(line)
    if match:
        return match.group(0)[1:6]
    return "(no-code)"


def run_guard(config: Config) -> int:
    if not config.corpus.is_dir():
        die(f"corpus not found: {config.corpus}")
    if not config.rules.is_dir():
        die(f"rules not found: {config.rules}")
    if config.vanilla is not None and not config.vanilla.is_dir():
        die(f"vanilla not found: {config.vanilla}")

    if config.build:
        print("guard: cargo build --release -p cwtools_cli")
        built = subprocess.run(
            ["cargo", "build", "--release", "-p", "cwtools_cli"],
            cwd=config.repo_root / "engine",
            check=False,
        )
        if built.returncode != 0:
            die("release build failed; nothing to validate with")

    if not config.bin.exists():
        die(f"cwtools binary not found: {config.bin} (build it, or pass --bin)")

    corpus = config.corpus.resolve()
    rules = config.rules.resolve()
    vanilla = config.vanilla.resolve() if config.vanilla is not None else None
    config = Config(
        preset=config.preset,
        corpus=corpus,
        rules=rules,
        vanilla=vanilla,
        baseline=config.baseline,
        bin=config.bin,
        game=config.game,
        build=config.build,
        bless=config.bless,
        repo_root=config.repo_root,
        script_dir=config.script_dir,
    )

    corpus_rev = describe(corpus)
    rules_rev = describe(rules)
    vanilla_rev = describe(vanilla) if vanilla is not None else None

    tmp_parent = os.environ.get("TMPDIR")
    work = Path(
        tempfile.mkdtemp(
            prefix="corpus-guard.",
            dir=tmp_parent if tmp_parent else None,
        )
    )
    keep = False
    try:
        raw_path = work / "report.csv"
        current_path = work / "current.csv"
        log_path = work / "validate.log"

        print(f"guard: {corpus} [{corpus_rev}]")
        print(f"guard: {rules} [{rules_rev}]")

        cmd = [
            str(config.bin),
            "validate",
            "--game",
            config.game,
            "--directory",
            str(corpus),
            "--rules",
            str(rules),
            "--report-type",
            "csv",
            "--output-file",
            str(raw_path),
        ]
        if vanilla is not None:
            print(f"guard: {vanilla} [{vanilla_rev}]")
            cmd.extend(["--vanilla", str(vanilla), "--no-vanilla-cache"])

        with log_path.open("w", encoding="utf-8", errors="surrogateescape") as log:
            status = subprocess.run(
                cmd, stdout=log, stderr=subprocess.STDOUT, check=False
            ).returncode
        if status > 1:
            keep = True
            text = log_path.read_text(encoding="utf-8", errors="replace")
            print("\n".join(text.splitlines()[:40]), file=sys.stderr)
            die(f"validate exited {status}, no report to compare")
        if not raw_path.is_file() or raw_path.stat().st_size == 0:
            keep = True
            die(f"validate wrote no report to {raw_path}")

        raw = raw_path.read_text(encoding="utf-8", errors="surrogateescape")
        rows = normalize_rows(raw, corpus)
        current = compose_current(rows, config, corpus_rev, rules_rev, vanilla_rev)
        current_path.write_text(current, encoding="utf-8", newline="\n")

        current_body = report_body(current)

        if config.bless:
            if config.baseline.is_file():
                before = max(
                    len(report_body(config.baseline.read_text(encoding="utf-8"))) - 1, 0
                )
            else:
                before = 0
            after = max(len(current_body) - 1, 0)
            config.baseline.parent.mkdir(parents=True, exist_ok=True)
            config.baseline.write_text(current, encoding="utf-8", newline="\n")
            print(f"guard: blessed {config.baseline} ({before} -> {after} diagnostics)")
            return 0

        if not config.baseline.is_file():
            die(f"no baseline at {config.baseline} (create one with --bless)")

        baseline_text = config.baseline.read_text(
            encoding="utf-8", errors="surrogateescape"
        )
        baseline_body = report_body(baseline_text)
        (work / "baseline.body").write_text(
            "\n".join(baseline_body) + "\n", encoding="utf-8", newline="\n"
        )
        (work / "current.body").write_text(
            "\n".join(current_body) + "\n", encoding="utf-8", newline="\n"
        )

        if baseline_body == current_body:
            n = max(len(current_body) - 1, 0)
            print(f"guard: OK, {n} diagnostics match the baseline")
            return 0

        keep = True
        diff_lines = list(
            difflib.unified_diff(
                baseline_body,
                current_body,
                fromfile="baseline",
                tofile="current",
                n=0,
                lineterm="",
            )
        )
        drift_path = work / "drift.diff"
        drift_path.write_text(
            "\n".join(diff_lines) + "\n", encoding="utf-8", newline="\n"
        )

        removed = sum(
            1
            for line in diff_lines
            if line.startswith("-") and not line.startswith("---")
        )
        added = sum(
            1
            for line in diff_lines
            if line.startswith("+") and not line.startswith("+++")
        )
        base_n = max(len(baseline_body) - 1, 0)
        curr_n = max(len(current_body) - 1, 0)

        print()
        print("guard: FAIL, diagnostics drifted from the baseline")
        print(f"  baseline {base_n} diagnostics")
        print(f"  current  {curr_n} diagnostics")
        print(f"  -{removed} +{added} rows")
        print()
        print("  by code (gone/new):")

        gone: dict[str, int] = {}
        new: dict[str, int] = {}
        for line in diff_lines:
            if line.startswith("-") and not line.startswith("---"):
                gone[code_from_diff_line(line)] = (
                    gone.get(code_from_diff_line(line), 0) + 1
                )
            elif line.startswith("+") and not line.startswith("+++"):
                new[code_from_diff_line(line)] = (
                    new.get(code_from_diff_line(line), 0) + 1
                )
        for code in sorted(set(gone) | set(new)):
            print(f"    {code:<10} -{gone.get(code, 0)} +{new.get(code, 0)}")

        print()
        print("  first 40 diff lines:")
        for line in diff_lines[2:42]:
            print(f"    {line}")
        print()
        print(f"  full diff:   {drift_path}")
        print(f"  full report: {current_path}")
        print(
            f"  if the change is intended, re-bless: python3 scripts/guard.py {config.preset} --bless"
        )
        return 1
    finally:
        if keep:
            print(f"guard: artifacts in {work}")
        else:
            shutil.rmtree(work, ignore_errors=True)


def main(argv: list[str] | None = None, env: Mapping[str, str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    environ: Mapping[str, str] = os.environ if env is None else env
    config = build_config(argv, environ)
    return run_guard(config)


if __name__ == "__main__":
    sys.exit(main())
