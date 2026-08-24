from __future__ import annotations

import json
import re

import pytest

from paths import REPO_ROOT
from rules_pins import (
    parse_games_source,
    parse_manifest,
    refresh_pins,
    replace_pin,
)

PINS = {
    "stellaris": "a" * 40,
    "hoi4": "b" * 40,
    "eu4": "c" * 40,
    "ck3": "d" * 40,
    "ck2": "e" * 40,
    "vic3": "f" * 40,
    "vic2": "1" * 40,
    "imperator": "2" * 40,
    "eu5": "3" * 40,
}

REPOS = {
    "stellaris": "https://github.com/cwtools/cwtools-stellaris-config",
    "hoi4": "https://github.com/cwtools/cwtools-hoi4-config",
    "eu4": "https://github.com/cwtools/cwtools-eu4-config",
    "ck3": "https://github.com/cwtools/cwtools-ck3-config",
    "ck2": "https://github.com/cwtools/cwtools-ck2-config",
    "vic3": "https://github.com/cwtools/cwtools-vic3-config",
    "vic2": "https://github.com/cwtools/cwtools-vic2-config",
    "imperator": "https://github.com/cwtools/cwtools-ir-config",
    "eu5": "https://github.com/kaiser-chris/cwtools-eu5-config",
}

TODAY = "2026-08-07"


def games_source(revision: int = 1) -> str:
    lines = [
        f"export const RULES_MANIFEST_REVISION = {revision};",
        "export const GAMES: GameDef[] = [",
    ]
    for game_id, ref in PINS.items():
        lines.append(f'  {{ id: "{game_id}",')
        lines.append(f'    repo: "{REPOS[game_id]}",')
        lines.append(f"    repoRef: '{ref}', // 2026-01-01")
        lines.append("  },")
    lines.append("];")
    lines.append("export interface RulesRepo {")
    return "\n".join(lines) + "\n"


def manifest(
    revision: int = 1, pins: dict[str, str] | None = None
) -> dict[str, object]:
    return {"schema": 1, "revision": revision, "pins": dict(pins or PINS)}


def game_id_for(repo: str) -> str:
    return next(key for key, value in REPOS.items() if value == repo)


def test_does_not_write_when_every_head_matches() -> None:
    calls: list[str] = []

    def ls_remote(repo: str) -> str:
        calls.append(repo)
        return f"{PINS[game_id_for(repo)]}\tHEAD\n"

    new_source, new_manifest, lines = refresh_pins(
        games_source(), manifest(), ls_remote, TODAY
    )

    assert new_source is None
    assert new_manifest is None
    assert lines == ["rules pins are already current"]
    assert len(calls) == len(PINS)


def test_updates_both_pin_sets_and_increments_once() -> None:
    new_ref = "f" * 40

    def ls_remote(repo: str) -> str:
        if repo == REPOS["hoi4"]:
            return f"{new_ref}\tHEAD\n"
        return f"{PINS[game_id_for(repo)]}\tHEAD\n"

    new_source, new_manifest, lines = refresh_pins(
        games_source(), manifest(), ls_remote, TODAY
    )

    assert new_source is not None
    assert new_manifest is not None
    assert "RULES_MANIFEST_REVISION = 2" in new_source
    assert f"repoRef: '{new_ref}', // {TODAY}" in new_source
    assert new_manifest["revision"] == 2
    assert new_manifest["pins"]["hoi4"] == new_ref
    assert new_manifest["pins"]["stellaris"] == PINS["stellaris"]
    assert lines == [f"- `hoi4` {REPOS['hoi4']}/compare/{PINS['hoi4']}...{new_ref}"]


def test_increments_the_revision_once_when_multiple_games_move() -> None:
    moved = {"hoi4": "f" * 40, "stellaris": "e" * 40}

    def ls_remote(repo: str) -> str:
        game_id = game_id_for(repo)
        return moved.get(game_id, PINS[game_id])

    _source, new_manifest, _lines = refresh_pins(
        games_source(), manifest(), ls_remote, TODAY
    )

    assert new_manifest is not None
    assert new_manifest["revision"] == 2
    assert new_manifest["pins"]["hoi4"] == moved["hoi4"]
    assert new_manifest["pins"]["stellaris"] == moved["stellaris"]


def test_fails_before_fetching_when_the_revision_disagrees() -> None:
    calls: list[str] = []

    def ls_remote(repo: str) -> str:
        calls.append(repo)
        return "0" * 40

    with pytest.raises(
        RuntimeError, match=re.escape("does not match games.ts revision")
    ):
        refresh_pins(games_source(1), manifest(2), ls_remote, TODAY)
    assert not calls


def test_fails_before_fetching_when_a_manifest_pin_disagrees() -> None:
    calls: list[str] = []
    bad = dict(PINS, hoi4="f" * 40)

    def ls_remote(repo: str) -> str:
        calls.append(repo)
        return "0" * 40

    with pytest.raises(
        RuntimeError, match=re.escape("manifest pin does not match games.ts")
    ):
        refresh_pins(games_source(), manifest(pins=bad), ls_remote, TODAY)
    assert not calls


def test_parses_the_checked_in_games_file() -> None:
    source = (REPO_ROOT / "extension" / "src" / "host" / "games.ts").read_text(
        encoding="utf-8"
    )
    revision, games = parse_games_source(source)

    assert revision >= 1
    ids = [game.game_id for game in games]
    assert "hoi4" in ids
    assert "stellaris" in ids
    hoi4 = next(game for game in games if game.game_id == "hoi4")
    assert hoi4.repo.startswith("https://")
    assert re.fullmatch(r"[0-9a-f]{40}", hoi4.repo_ref)


def test_replaces_the_hoi4_multiline_pin() -> None:
    old = "b" * 40
    new = "c" * 40
    source = (
        "repoRef:\n"
        "\t\t\tprocess.env.CWTOOLS_TEST_HOI4_REF ||\n"
        f'\t\t\t"{old}", // 2026-08-05\n'
    )

    updated = replace_pin(source, old, new, TODAY)

    assert f'"{new}", // {TODAY}' in updated
    assert old not in updated


def test_round_trips_manifest_json() -> None:
    text = (REPO_ROOT / "rules-pins.json").read_text(encoding="utf-8")
    parsed = parse_manifest(json.loads(text))

    assert parsed["schema"] == 1
    assert "hoi4" in parsed["pins"]
