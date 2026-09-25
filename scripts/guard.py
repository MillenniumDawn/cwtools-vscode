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
PIN_RE = re.compile(r"^# (corpus|rules|vanilla): .* @ (.+)$")
COLUMN_HEADER = "file,line,severity,code,message"
PIN_NAMES = ("corpus", "rules", "vanilla")
VALIDATE_TIMEOUT_SECONDS = 15 * 60
VALIDATE_LOG_TAIL_LINES = 40


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
    compare: str | None
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


def parse_pins(text: str) -> dict[str, str]:
    pins: dict[str, str] = {}
    for line in text.splitlines():
        if not line.startswith("#"):
            break
        match = PIN_RE.match(line)
        if match:
            pins[match.group(1)] = match.group(2)
    return pins


def compare_pins(
    baseline: Mapping[str, str], current: Mapping[str, str]
) -> list[tuple[str, str, str]]:
    mismatches: list[tuple[str, str, str]] = []
    for name in PIN_NAMES:
        recorded = baseline.get(name)
        actual = current.get(name)
        if recorded is None or actual is None:
            continue
        if recorded != actual or actual.endswith(" (dirty)"):
            mismatches.append((name, recorded, actual))
    return mismatches


def format_pin_mismatches(mismatches: list[tuple[str, str, str]]) -> str:
    return "; ".join(
        f"{name}: baseline {recorded}, current {actual}"
        for name, recorded, actual in mismatches
    )


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


def resolve_compare_revision(repo_root: Path, revision: str | None) -> str:
    if revision:
        command = [
            "git",
            "-C",
            str(repo_root),
            "rev-parse",
            "--verify",
            f"{revision}^{{commit}}",
        ]
    else:
        command = ["git", "-C", str(repo_root), "merge-base", "main", "HEAD"]
    resolved = subprocess.run(
        command,
        capture_output=True,
        text=True,
        check=False,
    )
    if resolved.returncode != 0 or not resolved.stdout.strip():
        requested = revision or "the merge-base of main and HEAD"
        die(f"could not resolve comparison revision {requested}")
    return resolved.stdout.strip()


def remove_compare_worktree(repo_root: Path, worktree: Path) -> None:
    subprocess.run(
        ["git", "-C", str(repo_root), "worktree", "remove", "--force", str(worktree)],
        capture_output=True,
        text=True,
        check=False,
    )
    # git worktree remove normally removes the directory itself. Keep this
    # fallback for interrupted builds and for files left by a failed checkout.
    shutil.rmtree(worktree, ignore_errors=True)


def default_projects(env: Mapping[str, str]) -> Path:
    raw = env.get("CWTOOLS_PROJECTS")
    if raw:
        return Path(raw)
    return REPO_ROOT.parent


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
    parser.add_argument(
        "--compare",
        nargs="?",
        const="",
        metavar="REV",
        help="compare with REV, or the merge-base with main when omitted",
    )
    args = parser.parse_args(argv)
    if args.compare is not None and args.bless:
        parser.error("--compare cannot be combined with --bless")

    corpus = Path(
        args.corpus or env.get("CWTOOLS_CORPUS") or (projects / "Millennium-Dawn")
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
        args.baseline or env.get("CWTOOLS_BASELINE") or (script_dir / "md-baseline.csv")
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
        compare=args.compare,
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
        "# cwtools guard baseline. Regenerate with "
        f"python3 scripts/guard.py {config.preset} --bless",
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
        if config.preset == "md":
            die(
                f"corpus not found: {config.corpus}\n"
                "  Set CWTOOLS_CORPUS, pass --corpus, or clone Millennium-Dawn "
                "under CWTOOLS_PROJECTS."
            )
        die(
            f"corpus not found: {config.corpus}\n"
            "  Pass --corpus or restore the committed "
            "scripts/vanilla-fixture/mod fixture."
        )
    if not config.rules.is_dir():
        if config.preset == "md":
            die(
                f"rules not found: {config.rules}\n"
                "  Set CWTOOLS_RULES, pass --rules, or clone cwtools-hoi4-config "
                "under CWTOOLS_PROJECTS."
            )
        die(
            f"rules not found: {config.rules}\n"
            "  Pass --rules or restore the committed "
            "scripts/vanilla-fixture/rules fixture."
        )
    if config.vanilla is not None and not config.vanilla.is_dir():
        if config.preset == "md":
            hint = (
                "Set CWTOOLS_VANILLA, pass --vanilla, or provide the committed "
                "scripts/vanilla-fixture/vanilla."
            )
        else:
            hint = (
                "Pass --vanilla or provide the committed "
                "scripts/vanilla-fixture/vanilla."
            )
        die(f"vanilla not found: {config.vanilla}\n  {hint}")

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
        compare=config.compare,
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
    compare_worktree: Path | None = None
    comparison_revision: str | None = None
    comparison_bin: Path | None = None
    try:
        raw_path = work / "report.csv"
        current_path = work / "current.csv"
        log_path = work / "validate.log"

        if config.compare is not None:
            comparison_revision = resolve_compare_revision(
                config.repo_root, config.compare or None
            )
            compare_worktree = work / "comparison-worktree"
            worktree_added = subprocess.run(
                [
                    "git",
                    "-C",
                    str(config.repo_root),
                    "worktree",
                    "add",
                    "--detach",
                    str(compare_worktree),
                    comparison_revision,
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            if worktree_added.returncode != 0:
                die(f"could not create comparison worktree at {comparison_revision}")

            print(
                f"guard: cargo build --release -p cwtools_cli ({comparison_revision})"
            )
            comparison_build = subprocess.run(
                ["cargo", "build", "--release", "-p", "cwtools_cli"],
                cwd=compare_worktree / "engine",
                check=False,
            )
            if comparison_build.returncode != 0:
                die("comparison release build failed; nothing to compare with")
            comparison_bin = resolve_bin(
                compare_worktree / "engine" / "target" / "release" / "cwtools"
            )
            if not comparison_bin.exists():
                die(f"comparison cwtools binary not found: {comparison_bin}")

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

        try:
            with log_path.open("w", encoding="utf-8", errors="surrogateescape") as log:
                status = subprocess.run(
                    cmd,
                    stdout=log,
                    stderr=subprocess.STDOUT,
                    check=False,
                    timeout=VALIDATE_TIMEOUT_SECONDS,
                ).returncode
        except subprocess.TimeoutExpired:
            keep = True
            text = log_path.read_text(encoding="utf-8", errors="replace")
            tail = text.splitlines()[-VALIDATE_LOG_TAIL_LINES:]
            print(
                f"guard: validate timed out after {VALIDATE_TIMEOUT_SECONDS} seconds",
                file=sys.stderr,
            )
            print(
                f"guard: last {VALIDATE_LOG_TAIL_LINES} lines of validate.log:",
                file=sys.stderr,
            )
            if tail:
                print("\n".join(tail), file=sys.stderr)
            die("validate timed out, no report to compare")
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

        comparison_body: list[str] | None = None
        if config.compare is not None:
            if comparison_bin is None or comparison_revision is None:
                die("comparison binary was not prepared")
            comparison_raw_path = work / "comparison-report.csv"
            comparison_log_path = work / "comparison-validate.log"
            comparison_cmd = [
                str(comparison_bin),
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
                str(comparison_raw_path),
            ]
            if vanilla is not None:
                comparison_cmd.extend(["--vanilla", str(vanilla), "--no-vanilla-cache"])
            try:
                with comparison_log_path.open(
                    "w", encoding="utf-8", errors="surrogateescape"
                ) as log:
                    comparison_status = subprocess.run(
                        comparison_cmd,
                        stdout=log,
                        stderr=subprocess.STDOUT,
                        check=False,
                        timeout=VALIDATE_TIMEOUT_SECONDS,
                    ).returncode
            except subprocess.TimeoutExpired:
                keep = True
                die("comparison validate timed out, no report to compare")
            if comparison_status > 1:
                keep = True
                text = comparison_log_path.read_text(encoding="utf-8", errors="replace")
                print("\n".join(text.splitlines()[:40]), file=sys.stderr)
                die(
                    f"comparison validate exited {comparison_status}, "
                    "no report to compare"
                )
            if (
                not comparison_raw_path.is_file()
                or comparison_raw_path.stat().st_size == 0
            ):
                keep = True
                die(f"comparison validate wrote no report to {comparison_raw_path}")
            comparison_raw = comparison_raw_path.read_text(
                encoding="utf-8", errors="surrogateescape"
            )
            comparison_rows = normalize_rows(comparison_raw, corpus)
            comparison = compose_current(
                comparison_rows, config, corpus_rev, rules_rev, vanilla_rev
            )
            comparison_body = report_body(comparison)

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

        if comparison_body is not None:
            baseline_body = comparison_body
            pin_mismatches: list[tuple[str, str, str]] = []
        else:
            if not config.baseline.is_file():
                die(f"no baseline at {config.baseline} (create one with --bless)")

            baseline_text = config.baseline.read_text(
                encoding="utf-8", errors="surrogateescape"
            )
            baseline_body = report_body(baseline_text)
            pin_mismatches = compare_pins(
                parse_pins(baseline_text), parse_pins(current)
            )
        (work / "baseline.body").write_text(
            "\n".join(baseline_body) + "\n", encoding="utf-8", newline="\n"
        )
        (work / "current.body").write_text(
            "\n".join(current_body) + "\n", encoding="utf-8", newline="\n"
        )

        if baseline_body == current_body:
            n = max(len(current_body) - 1, 0)
            if comparison_revision is not None:
                print(
                    "guard: OK, "
                    f"{n} diagnostics match comparison revision "
                    f"{comparison_revision}"
                )
            else:
                print(f"guard: OK, {n} diagnostics match the baseline")
            if pin_mismatches:
                print(
                    "guard: note: baseline input revisions differ from current "
                    f"inputs ({format_pin_mismatches(pin_mismatches)})"
                )
            return 0

        keep = True
        diff_lines = list(
            difflib.unified_diff(
                baseline_body,
                current_body,
                fromfile=(
                    "comparison" if comparison_revision is not None else "baseline"
                ),
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
        if comparison_revision is not None:
            print(
                "guard: FAIL, diagnostics drifted from comparison revision "
                f"{comparison_revision}"
            )
            print(f"  comparison {base_n} diagnostics")
        else:
            print("guard: FAIL, diagnostics drifted from the baseline")
            print(f"  baseline {base_n} diagnostics")
        print(f"  current    {curr_n} diagnostics")
        print(f"  -{removed} +{added} rows")
        if pin_mismatches:
            print()
            print(
                "  warning: baseline input revisions differ from current inputs "
                f"({format_pin_mismatches(pin_mismatches)}); this diff may be input "
                "drift, not your change"
            )
            print(
                "  for a current-input before-baseline, use "
                f"--baseline <path> --bless (currently {config.baseline})"
            )
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
        if comparison_revision is None:
            print(
                "  if the change is intended, re-bless: "
                f"python3 scripts/guard.py {config.preset} --bless"
            )
        return 1
    finally:
        if compare_worktree is not None:
            remove_compare_worktree(config.repo_root, compare_worktree)
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
