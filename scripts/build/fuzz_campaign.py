"""Run one bounded fuzz target, retaining its corpus and replay metadata."""

from __future__ import annotations

import argparse
import json
import os
import shlex
import signal
import subprocess
from pathlib import Path

TOOLCHAIN = "nightly-2026-07-24"
CARGO_FUZZ = "0.13.2"
WALL_SECONDS = 20 * 60
TARGETS = (
    "parse_rules",
    "parse_string",
    "parse_loc_text",
    "format_text",
    "fix_edits",
    "decode_capped",
    "with_archived_file",
    "read_errors_from_file",
    "vanilla_cache_load",
)
LIMITS = ["-timeout=10", "-rss_limit_mb=2048", "-max_len=1048576"]


def stop_group(process: subprocess.Popen[str]) -> None:
    """Stop the entire Linux process group, including a stuck fuzz child."""
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        pass
    # The leader may have exited while a child remains in its process group.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait()


def run_campaign(target: str, corpus: Path, output: Path) -> int:
    output = output.resolve()
    artifacts = output / "artifacts"
    artifacts.mkdir(parents=True, exist_ok=True)
    corpus.mkdir(parents=True, exist_ok=True)
    command = [
        "cargo",
        f"+{TOOLCHAIN}",
        "fuzz",
        "run",
        target,
        str(corpus),
        "--",
        "-max_total_time=60",
        *LIMITS,
        f"-artifact_prefix={artifacts}/",
    ]
    metadata = {
        "target": target,
        "source_revision": os.environ["GITHUB_SHA"],
        "rust_toolchain": TOOLCHAIN,
        "cargo_fuzz": CARGO_FUZZ,
        "command": command,
        "status": "starting",
        "replay_instructions": (
            "Download the artifact into engine/fuzz-campaign; "
            "run replay commands from engine/."
        ),
    }
    metadata_path = output / "metadata.json"
    metadata_path.write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
    status = "failed to start"
    try:
        with (output / "campaign.log").open("w", encoding="utf-8") as log:
            process = subprocess.Popen(
                command,
                stdout=log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
                text=True,
            )
            try:
                code = process.wait(timeout=WALL_SECONDS)
                status = f"exited {code}"
                return code
            except subprocess.TimeoutExpired:
                status = "wall timeout"
                stop_group(process)
                return 124
    finally:
        replay = []
        for crash in sorted(artifacts.iterdir()):
            if crash.is_file():
                replay_command = [
                    "cargo",
                    f"+{TOOLCHAIN}",
                    "fuzz",
                    "run",
                    target,
                    f"fuzz-campaign/artifacts/{crash.name}",
                    "--",
                    "-runs=1",
                    *LIMITS,
                ]
                replay.append(shlex.join(replay_command))
        (output / "replay.txt").write_text(
            "\n".join(replay) + ("\n" if replay else ""), encoding="utf-8"
        )
        metadata["status"] = status
        metadata_path.write_text(
            json.dumps(metadata, indent=2) + "\n", encoding="utf-8"
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("target", choices=TARGETS)
    parser.add_argument("corpus", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    return run_campaign(args.target, args.corpus, args.output)


if __name__ == "__main__":
    raise SystemExit(main())
