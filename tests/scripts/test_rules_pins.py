from __future__ import annotations

import json
import unittest

from load import REPO_ROOT, load_build

rules_pins = load_build("rules_pins")

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


class RulesPinsTests(unittest.TestCase):
    def test_does_not_write_when_every_head_matches(self) -> None:
        calls: list[str] = []

        def ls_remote(repo: str) -> str:
            calls.append(repo)
            game_id = next(key for key, value in REPOS.items() if value == repo)
            return f"{PINS[game_id]}\tHEAD\n"

        new_source, new_manifest, lines = rules_pins.refresh_pins(
            games_source(), manifest(), ls_remote, "2026-08-07"
        )
        self.assertIsNone(new_source)
        self.assertIsNone(new_manifest)
        self.assertEqual(lines, ["rules pins are already current"])
        self.assertEqual(len(calls), len(PINS))

    def test_updates_both_pin_sets_and_increments_once(self) -> None:
        new_ref = "f" * 40

        def ls_remote(repo: str) -> str:
            if repo == REPOS["hoi4"]:
                return f"{new_ref}\tHEAD\n"
            game_id = next(key for key, value in REPOS.items() if value == repo)
            return f"{PINS[game_id]}\tHEAD\n"

        new_source, new_manifest, lines = rules_pins.refresh_pins(
            games_source(), manifest(), ls_remote, "2026-08-07"
        )
        assert new_source is not None
        assert new_manifest is not None
        self.assertIn("RULES_MANIFEST_REVISION = 2", new_source)
        self.assertIn(f"repoRef: '{new_ref}', // 2026-08-07", new_source)
        self.assertEqual(new_manifest["revision"], 2)
        self.assertEqual(new_manifest["pins"]["hoi4"], new_ref)
        self.assertEqual(new_manifest["pins"]["stellaris"], PINS["stellaris"])
        self.assertEqual(
            lines,
            [f"- `hoi4` {REPOS['hoi4']}/compare/{PINS['hoi4']}...{new_ref}"],
        )

    def test_increments_the_revision_once_when_multiple_games_move(self) -> None:
        hoi4_ref = "f" * 40
        stellaris_ref = "e" * 40

        def ls_remote(repo: str) -> str:
            if repo == REPOS["hoi4"]:
                return hoi4_ref
            if repo == REPOS["stellaris"]:
                return stellaris_ref
            game_id = next(key for key, value in REPOS.items() if value == repo)
            return PINS[game_id]

        _source, new_manifest, _lines = rules_pins.refresh_pins(
            games_source(), manifest(), ls_remote, "2026-08-07"
        )
        assert new_manifest is not None
        self.assertEqual(new_manifest["revision"], 2)
        self.assertEqual(new_manifest["pins"]["hoi4"], hoi4_ref)
        self.assertEqual(new_manifest["pins"]["stellaris"], stellaris_ref)

    def test_fails_before_fetching_when_revision_disagrees(self) -> None:
        calls: list[str] = []

        def ls_remote(repo: str) -> str:
            calls.append(repo)
            return "0" * 40

        with self.assertRaisesRegex(RuntimeError, "does not match games.ts revision"):
            rules_pins.refresh_pins(
                games_source(1), manifest(2), ls_remote, "2026-08-07"
            )
        self.assertEqual(calls, [])

    def test_fails_before_fetching_when_a_manifest_pin_disagrees(self) -> None:
        calls: list[str] = []
        bad = dict(PINS)
        bad["hoi4"] = "f" * 40

        def ls_remote(repo: str) -> str:
            calls.append(repo)
            return "0" * 40

        with self.assertRaisesRegex(
            RuntimeError, "manifest pin does not match games.ts"
        ):
            rules_pins.refresh_pins(
                games_source(), manifest(pins=bad), ls_remote, "2026-08-07"
            )
        self.assertEqual(calls, [])

    def test_parses_the_checked_in_games_file(self) -> None:
        source = (REPO_ROOT / "extension" / "src" / "host" / "games.ts").read_text(
            encoding="utf-8"
        )
        revision, games = rules_pins.parse_games_source(source)
        self.assertGreaterEqual(revision, 1)
        ids = [game.game_id for game in games]
        self.assertIn("hoi4", ids)
        self.assertIn("stellaris", ids)
        hoi4 = next(game for game in games if game.game_id == "hoi4")
        self.assertTrue(hoi4.repo.startswith("https://"))
        self.assertRegex(hoi4.repo_ref, r"^[0-9a-f]{40}$")

    def test_replaces_the_hoi4_multiline_pin(self) -> None:
        old = "b" * 40
        new = "c" * 40
        source = (
            "repoRef:\n"
            "\t\t\tprocess.env.CWTOOLS_TEST_HOI4_REF ||\n"
            f'\t\t\t"{old}", // 2026-08-05\n'
        )
        updated = rules_pins.replace_pin(source, old, new, "2026-08-07")
        self.assertIn(f'"{new}", // 2026-08-07', updated)
        self.assertNotIn(old, updated)

    def test_round_trips_manifest_json(self) -> None:
        text = (REPO_ROOT / "rules-pins.json").read_text(encoding="utf-8")
        parsed = rules_pins.parse_manifest(json.loads(text))
        self.assertEqual(parsed["schema"], 1)
        self.assertIn("hoi4", parsed["pins"])


if __name__ == "__main__":
    unittest.main()
