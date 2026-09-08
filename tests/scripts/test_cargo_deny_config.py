from __future__ import annotations

import tomllib
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]


def test_cargo_deny_config_denies_yanked_crates() -> None:
    with (REPO_ROOT / "engine" / "deny.toml").open("rb") as config_file:
        config = tomllib.load(config_file)

    assert config["advisories"]["yanked"] == "deny"
