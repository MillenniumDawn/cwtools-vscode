from __future__ import annotations

import json
import re
import subprocess
import sys
from collections.abc import Callable
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any

from paths import EXTENSION_HOST_ROOT, REPO_ROOT

GAMES_FILE = EXTENSION_HOST_ROOT / "games.ts"
MANIFEST_FILE = REPO_ROOT / "rules-pins.json"

REVISION_RE = re.compile(r"export const RULES_MANIFEST_REVISION = (\d+);")
REVISION_LINE_RE = re.compile(r"export const RULES_MANIFEST_REVISION = \d+;")
ID_RE = re.compile(r'id:\s*"([^"]+)"')
SHA_RE = re.compile(r"^[0-9a-f]{40}$")


@dataclass(frozen=True)
class GamePin:
    game_id: str
    repo: str
    repo_ref: str


def parse_games_source(source: str) -> tuple[int, list[GamePin]]:
    revision_match = REVISION_RE.search(source)
    if revision_match is None:
        raise RuntimeError(f"no rules manifest revision in {GAMES_FILE}")
    try:
        revision = int(revision_match.group(1), 10)
    except ValueError as error:
        raise RuntimeError(f"no rules manifest revision in {GAMES_FILE}") from error

    start = source.find("export const GAMES")
    if start == -1:
        raise RuntimeError(f"no GAMES array in {GAMES_FILE}")
    rest = source[start:]
    end = rest.find("export interface RulesRepo")
    if end == -1:
        end = rest.find("export const LANGUAGE_REPOS")
    block = rest if end == -1 else rest[:end]

    ids = list(ID_RE.finditer(block))
    games: list[GamePin] = []
    for index, match in enumerate(ids):
        chunk_end = ids[index + 1].start() if index + 1 < len(ids) else len(block)
        chunk = block[match.start() : chunk_end]
        game_id = match.group(1)
        repo = _parse_repo(chunk)
        repo_ref = _parse_ref(chunk)
        if repo is None or repo_ref is None:
            raise RuntimeError(f"{game_id}: could not parse repo/repoRef")
        games.append(GamePin(game_id, repo, repo_ref))
    if not games:
        raise RuntimeError(f"no games in {GAMES_FILE}")
    return revision, games


def _parse_repo(chunk: str) -> str | None:
    match = re.search(r'repo:\s*"([^"]+)"', chunk)
    if match:
        return match.group(1)
    match = re.search(r'repo:[\s\S]*?"(https://[^"]+)"', chunk)
    return match.group(1) if match else None


def _parse_ref(chunk: str) -> str | None:
    match = re.search(r"repoRef:\s*'([0-9a-f]{40})'", chunk)
    if match:
        return match.group(1)
    match = re.search(r'repoRef:\s*"([0-9a-f]{40})"', chunk)
    if match:
        return match.group(1)
    match = re.search(r'repoRef:[\s\S]*?"([0-9a-f]{40})"', chunk)
    return match.group(1) if match else None


def parse_manifest(value: object) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise TypeError(f"could not read {MANIFEST_FILE}")
    keys = sorted(value)
    if keys != ["pins", "revision", "schema"]:
        raise RuntimeError("Rules manifest has unexpected fields.")
    if value.get("schema") != 1:
        raise RuntimeError("Rules manifest has an unsupported schema.")
    revision = value.get("revision")
    if not isinstance(revision, int) or isinstance(revision, bool):
        raise TypeError("Rules manifest has an invalid revision.")
    pins = value.get("pins")
    if not isinstance(pins, dict):
        raise TypeError("Rules manifest pins must be an object.")
    return {"schema": 1, "revision": revision, "pins": dict(pins)}


def load_and_check(
    games_source: str, manifest_raw: object
) -> tuple[int, list[GamePin], dict[str, Any]]:
    revision, games = parse_games_source(games_source)
    manifest = parse_manifest(manifest_raw)
    if manifest["revision"] != revision:
        raise RuntimeError(
            f"{MANIFEST_FILE} revision {manifest['revision']} does not match "
            f"games.ts revision {revision}"
        )
    pins = manifest["pins"]
    pin_ids = sorted(str(key) for key in pins)
    game_ids = sorted(game.game_id for game in games)
    if pin_ids != game_ids:
        raise RuntimeError("Rules manifest pins do not match the supported games.")
    for game in games:
        ref = pins.get(game.game_id)
        if not isinstance(ref, str) or not SHA_RE.fullmatch(ref):
            raise RuntimeError(f"Rules manifest has an invalid {game.game_id} ref.")
        if ref != game.repo_ref:
            raise RuntimeError(f"{game.game_id}: manifest pin does not match games.ts")
    return revision, games, manifest


def replace_pin(source: str, old: str, new: str, today: str) -> str:
    simple = f"repoRef: '{old}', // "
    idx = source.find(simple)
    if idx != -1:
        end = source.find("\n", idx)
        replacement = f"repoRef: '{new}', // {today}"
        return source[:idx] + replacement + source[end if end != -1 else len(source) :]

    simple_dq = f'repoRef: "{old}", // '
    idx = source.find(simple_dq)
    if idx != -1:
        end = source.find("\n", idx)
        replacement = f'repoRef: "{new}", // {today}'
        return source[:idx] + replacement + source[end if end != -1 else len(source) :]

    quoted = f'"{old}", // '
    idx = source.find(quoted)
    if idx != -1:
        end = source.find("\n", idx)
        replacement = f'"{new}", // {today}'
        return source[:idx] + replacement + source[end if end != -1 else len(source) :]

    raise RuntimeError(f"no pin line for {old} in {GAMES_FILE}")


def refresh_pins(
    games_source: str,
    manifest_raw: object,
    ls_remote: Callable[[str], str],
    today: str,
) -> tuple[str | None, dict[str, Any] | None, list[str]]:
    revision, games, manifest = load_and_check(games_source, manifest_raw)
    next_source = games_source
    pins: dict[str, str] = {
        str(key): str(value) for key, value in dict(manifest["pins"]).items()
    }
    next_manifest: dict[str, Any] = {
        "schema": 1,
        "revision": revision,
        "pins": pins,
    }
    bumped: list[str] = []
    for game in games:
        raw_head = ls_remote(game.repo)
        head = raw_head.split()[0] if raw_head else ""
        if not SHA_RE.fullmatch(head):
            raise RuntimeError(
                f"{game.game_id}: git ls-remote {game.repo} returned no commit"
            )
        if head == game.repo_ref:
            continue
        next_source = replace_pin(next_source, game.repo_ref, head, today)
        pins[game.game_id] = head
        bumped.append(
            f"- `{game.game_id}` {game.repo}/compare/{game.repo_ref}...{head}"
        )

    if not bumped:
        return None, None, ["rules pins are already current"]

    next_revision = revision + 1
    if REVISION_LINE_RE.search(next_source) is None:
        raise RuntimeError(f"no rules manifest revision in {GAMES_FILE}")
    next_source = REVISION_LINE_RE.sub(
        f"export const RULES_MANIFEST_REVISION = {next_revision};",
        next_source,
        count=1,
    )
    next_manifest["revision"] = next_revision
    return next_source, next_manifest, bumped


def git_ls_remote(repo: str) -> str:
    result = subprocess.run(
        ["git", "ls-remote", repo, "HEAD"],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(f"git ls-remote {repo} failed")
    return result.stdout


def main() -> int:
    try:
        games_source = GAMES_FILE.read_text(encoding="utf-8")
        manifest_raw = json.loads(MANIFEST_FILE.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError(f"could not read pin inputs: {error}") from error

    today = datetime.now(UTC).date().isoformat()
    new_source, new_manifest, lines = refresh_pins(
        games_source, manifest_raw, git_ls_remote, today
    )
    if new_source is None or new_manifest is None:
        print("rules pins are already current")
        return 0
    GAMES_FILE.write_text(new_source, encoding="utf-8")
    MANIFEST_FILE.write_text(
        json.dumps(new_manifest, indent="\t") + "\n", encoding="utf-8"
    )
    for line in lines:
        print(line)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, TypeError, OSError) as error:
        sys.stderr.write(f"{error}\n")
        raise SystemExit(1) from error
