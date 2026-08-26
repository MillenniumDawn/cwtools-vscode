from __future__ import annotations

from pathlib import Path
from types import ModuleType

import pytest

CLEAN_BASELINE = (
    "# cwtools guard baseline. Regenerate with python3 scripts/guard.py md --bless\n"
    "# corpus: Millennium-Dawn @ 44940966fe\n"
    "# rules:  cwtools-hoi4-config/Config @ 46d5886\n"
    "file,line,severity,code,message\n"
)

DIRTY_BASELINE = (
    "# cwtools guard baseline. Regenerate with python3 scripts/guard.py md --bless\n"
    "# corpus: Millennium-Dawn @ 44940966fe (dirty)\n"
    "# rules:  cwtools-hoi4-config/Config @ 46d5886\n"
    "file,line,severity,code,message\n"
)

MISSING_RULES_BASELINE = (
    "# cwtools guard baseline. Regenerate with python3 scripts/guard.py md --bless\n"
    "# corpus: Millennium-Dawn @ 44940966fe\n"
    "file,line,severity,code,message\n"
)

FULL_SHA_A = "a" * 40
FULL_SHA_B = "b" * 40


def test_parses_a_clean_pin(resolve_pins: ModuleType) -> None:
    assert resolve_pins.parse_pin(CLEAN_BASELINE, "corpus") == "44940966fe"
    assert resolve_pins.parse_pin(CLEAN_BASELINE, "rules") == "46d5886"


def test_rejects_a_dirty_pin(resolve_pins: ModuleType) -> None:
    assert resolve_pins.parse_pin(DIRTY_BASELINE, "corpus") is None


def test_missing_pin_is_none(resolve_pins: ModuleType) -> None:
    assert resolve_pins.parse_pin(MISSING_RULES_BASELINE, "rules") is None


@pytest.mark.parametrize(
    ("sha", "valid"),
    [
        ("a" * 40, True),
        ("a" * 39, False),
        ("a" * 41, False),
        ("g" * 40, False),
        ("", False),
    ],
)
def test_check_sha_accepts_only_a_full_hex_sha(
    resolve_pins: ModuleType, sha: str, valid: bool
) -> None:
    if valid:
        resolve_pins.check_sha("label", sha)
    else:
        with pytest.raises(resolve_pins.PinError):
            resolve_pins.check_sha("label", sha)


def test_resolve_pins_returns_shas_and_log_lines(resolve_pins: ModuleType) -> None:
    calls: list[tuple[str, str]] = []

    def gh_api(repo: str, pin: str) -> str:
        calls.append((repo, pin))
        return FULL_SHA_A if repo == "md/repo" else FULL_SHA_B

    md_sha, rules_sha, lines = resolve_pins.resolve_pins(
        CLEAN_BASELINE, Path("baseline.csv"), "md/repo", "rules/repo", gh_api
    )

    assert md_sha == FULL_SHA_A
    assert rules_sha == FULL_SHA_B
    assert calls == [("md/repo", "44940966fe"), ("rules/repo", "46d5886")]
    assert lines == [
        "md baseline pins: corpus 44940966fe, rules 46d5886",
        f"resolved: md {FULL_SHA_A}, rules {FULL_SHA_B}",
    ]


def test_resolve_pins_rejects_a_dirty_baseline(resolve_pins: ModuleType) -> None:
    def gh_api(_repo: str, _pin: str) -> str:
        raise AssertionError("gh_api should not be called for a dirty baseline")

    with pytest.raises(resolve_pins.PinError, match="no clean revision pin"):
        resolve_pins.resolve_pins(
            DIRTY_BASELINE, Path("baseline.csv"), "md/repo", "rules/repo", gh_api
        )


def test_resolve_pins_reports_an_unresolvable_sha(resolve_pins: ModuleType) -> None:
    def gh_api(_repo: str, _pin: str) -> str:
        return "not-a-sha"

    with pytest.raises(resolve_pins.PinError, match="could not resolve"):
        resolve_pins.resolve_pins(
            CLEAN_BASELINE, Path("baseline.csv"), "md/repo", "rules/repo", gh_api
        )


def test_main_writes_outputs_and_succeeds(
    resolve_pins: ModuleType, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(resolve_pins, "MD_BASELINE", tmp_path / "md-baseline.csv")
    resolve_pins.MD_BASELINE.write_text(CLEAN_BASELINE, encoding="utf-8")

    def gh_api(repo: str, _pin: str) -> str:
        return FULL_SHA_A if repo == "MillenniumDawn/Millennium-Dawn" else FULL_SHA_B

    monkeypatch.setattr(resolve_pins, "gh_api_commit_sha", gh_api)
    output_path = tmp_path / "github_output"

    exit_code = resolve_pins.main(
        {
            "MD_REPO": "MillenniumDawn/Millennium-Dawn",
            "RULES_REPO": "Kaiserreich/cwtools-hoi4-config",
            "GITHUB_OUTPUT": str(output_path),
        }
    )

    assert exit_code == 0
    assert output_path.read_text(encoding="utf-8") == (
        f"md_rev={FULL_SHA_A}\nrules_rev={FULL_SHA_B}\n"
    )


def test_main_fails_without_a_clean_pin(
    resolve_pins: ModuleType, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(resolve_pins, "MD_BASELINE", tmp_path / "md-baseline.csv")
    resolve_pins.MD_BASELINE.write_text(DIRTY_BASELINE, encoding="utf-8")
    output_path = tmp_path / "github_output"

    exit_code = resolve_pins.main(
        {
            "MD_REPO": "MillenniumDawn/Millennium-Dawn",
            "RULES_REPO": "Kaiserreich/cwtools-hoi4-config",
            "GITHUB_OUTPUT": str(output_path),
        }
    )

    assert exit_code == 1
    assert not output_path.exists()


def test_main_requires_the_repo_env_vars(
    resolve_pins: ModuleType, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(resolve_pins, "MD_BASELINE", tmp_path / "md-baseline.csv")
    resolve_pins.MD_BASELINE.write_text(CLEAN_BASELINE, encoding="utf-8")

    exit_code = resolve_pins.main({"GITHUB_OUTPUT": str(tmp_path / "github_output")})

    assert exit_code == 1
