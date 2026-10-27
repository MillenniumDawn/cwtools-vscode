"""Regression guards for the dependency-advisory gates in CI (issue #595).

These prove placement against the workflow's own structure, not comment
strings: the schedule must sweep cargo-deny over both lockfiles and run
npm's audit, dependency review stays PR-only, and the fuzz smoke pins the
committed fuzz lockfile -- cargo-fuzz 0.13.2 has no --locked flag, and the
`-- <args>` tail of `fuzz run` reaches libFuzzer, not cargo.
"""

import re
import tomllib
from pathlib import Path

from workflow_helpers import job_scope, workflow_step

REPO_ROOT = Path(__file__).resolve().parents[2]
CI = REPO_ROOT / ".github" / "workflows" / "ci.yml"
DEPENDABOT = REPO_ROOT / ".github" / "dependabot.yml"
FUZZ_MANIFEST = REPO_ROOT / "engine" / "fuzz" / "Cargo.toml"
FUZZ_LOCK = REPO_ROOT / "engine" / "fuzz" / "Cargo.lock"
DENY_CONFIG = REPO_ROOT / "engine" / "deny.toml"

FUZZ_RUN_STEPS = (
    "parse_rules over pinned upstream configs",
    "parse_string over seed corpus",
    "parse_loc_text over seed corpus",
    "format_text over seed corpus",
    "fix_edits over seed corpus",
    "decode_capped over seed corpus",
    "with_archived_file over seed corpus",
    "read_errors_from_file over seed corpus",
    "vanilla_cache_load over seed corpus",
)


def workflow_triggers(workflow: str) -> str:
    """Return the top-level `on:` block, up to the next top-level key."""
    match = re.search(r"^on:\n((?:[ \t#].*\n|\n)*)", workflow, flags=re.MULTILINE)
    assert match is not None
    return match.group(1)


def entry_directories(entry: str) -> list[str]:
    """Return the `directory` or `directories` one Dependabot entry declares."""
    single = re.findall(r"^    directory: (\S+)$", entry, flags=re.MULTILINE)
    block = re.search(
        r"^    directories:\n((?:      .*\n)+)", entry, flags=re.MULTILINE
    )
    listed = (
        re.findall(r"^      - (\S+)$", block.group(1), flags=re.MULTILINE)
        if block
        else []
    )
    return single + listed


def test_ci_keeps_the_weekly_schedule_the_sweeps_depend_on() -> None:
    triggers = workflow_triggers(CI.read_text(encoding="utf-8"))
    assert "\n  schedule:\n" in triggers
    assert re.search(
        r'^    - cron: "\S+ \S+ \S+ \S+ \S+"$', triggers, flags=re.MULTILINE
    )
    assert "\n  workflow_dispatch:\n" in triggers


def test_scheduled_lint_sweeps_cargo_deny_over_engine_and_fuzz_lockfiles() -> None:
    workflow = CI.read_text(encoding="utf-8")
    lint = job_scope(workflow, "lint-rs")

    # lint-rs carries no event gate, so it runs on the weekly schedule.
    assert "\n    if:" not in lint

    engine = workflow_step(lint, "cargo-deny")
    fuzz = workflow_step(lint, "cargo-deny (fuzz lockfile)")
    assert "manifest-path: engine/Cargo.toml" in engine
    assert "manifest-path: engine/fuzz/Cargo.toml" in fuzz
    for step in (engine, fuzz):
        assert "EmbarkStudios/cargo-deny-action@" in step


def test_npm_audit_runs_on_the_schedule_dependabot_cannot_cover() -> None:
    workflow = CI.read_text(encoding="utf-8")
    audit = job_scope(workflow, "npm-audit")
    # workflow_dispatch is allowed too, so the job can be run on demand.
    assert (
        "    if: github.event_name == 'schedule' || "
        "github.event_name == 'workflow_dispatch'\n"
    ) in audit
    assert "npm audit --audit-level=high" in audit

    # The PR path keeps its own gate; the npm sweep is for the schedule and
    # manual dispatch only.
    review = job_scope(workflow, "dependency-review")
    assert "if: github.event_name == 'pull_request'" in review


def test_fuzz_smoke_pins_the_committed_lockfile_and_never_libfuzzer_args() -> None:
    workflow = CI.read_text(encoding="utf-8")
    smoke = job_scope(workflow, "fuzz-smoke")

    metadata_step = workflow_step(
        smoke, "fuzz lockfile matches the committed manifests"
    )
    # The only --locked the smoke knows is the cargo metadata query that
    # fails when fuzz/Cargo.lock has drifted.
    assert "cargo +nightly-2026-07-24 metadata --locked" in metadata_step
    assert "--format-version 1" in metadata_step
    assert "--manifest-path fuzz/Cargo.toml" in metadata_step

    # Whole step bodies, continuation lines included: a --locked appended on
    # a folded `run: >-` line would reach libFuzzer, which has no such flag.
    for name in FUZZ_RUN_STEPS:
        assert "--locked" not in workflow_step(smoke, name)

    first_run = workflow_step(smoke, "parse_rules over pinned upstream configs")
    assert "-- -runs=0" in first_run
    assert smoke.index("fuzz lockfile matches the committed manifests") < smoke.index(
        "parse_rules over pinned upstream configs"
    )


def test_dependabot_places_pip_beside_the_other_ecosystems() -> None:
    source = DEPENDABOT.read_text(encoding="utf-8")
    entries = source.split("\n  - package-ecosystem: ")[1:]

    ecosystems = [entry.split("\n", maxsplit=1)[0] for entry in entries]
    assert ecosystems == ["github-actions", "cargo", "npm", "pip"]

    pip = next(entry for entry in entries if entry.startswith("pip\n"))
    assert entry_directories(pip) == ["/"]
    # The pip entry follows the shared schedule/security-group contract the
    # other ecosystems use.
    for marker in (
        "interval: weekly",
        "day: monday",
        'time: "06:00"',
        "timezone: Etc/UTC",
        "cooldown:\n      default-days: 7",
        "applies-to: security-updates",
    ):
        assert marker in pip

    # The cargo entry covers the fuzz workspace's own lockfile as well.
    cargo = next(entry for entry in entries if entry.startswith("cargo\n"))
    assert entry_directories(cargo) == ["/engine", "/engine/fuzz"]


def test_fuzz_manifest_declares_a_license_for_the_deny_sweep() -> None:
    with FUZZ_MANIFEST.open("rb") as manifest_file:
        manifest = tomllib.load(manifest_file)
    assert manifest["package"]["license"] == "MIT"


def test_deny_allows_ncsa_only_for_libfuzzer_sys() -> None:
    with DENY_CONFIG.open("rb") as config_file:
        licenses = tomllib.load(config_file)["licenses"]
    # libfuzzer-sys, the fuzz harness in engine/fuzz/Cargo.lock, is
    # (MIT OR Apache-2.0) AND NCSA.
    assert 'name = "libfuzzer-sys"' in FUZZ_LOCK.read_text(encoding="utf-8")
    assert "NCSA" not in licenses["allow"]
    assert {"crate": "libfuzzer-sys", "allow": ["NCSA"]} in licenses["exceptions"]
    assert "MIT" in licenses["allow"]
    assert "Apache-2.0" in licenses["allow"]
