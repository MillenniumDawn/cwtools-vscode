from __future__ import annotations

import shutil
import subprocess
import sys
from collections.abc import Callable
from typing import NamedTuple

import pytest

import hosttest
from paths import REPO_ROOT

# The runner only asks whether the CLI entry point exists, so a checked-in file
# stands in for it and the suite does not need node_modules installed.
INSTALLED_CLI = REPO_ROOT / "package.json"
MISSING_CLI = REPO_ROOT / "node_modules" / "@vscode" / "not-installed.mjs"


@pytest.fixture(name="no_override")
def no_override_fixture(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("CWTOOLS_TEST_DISPLAY", "")


def found(path: str) -> Callable[[str], str]:
    return lambda _name: path


@pytest.mark.usefixtures("no_override")
def test_linux_uses_xvfb_run(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", found("/usr/bin/xvfb-run"))

    display = hosttest.resolve_display(platform="linux")

    assert display.name == "xvfb"
    assert display.prefix == ["/usr/bin/xvfb-run", "-a"]
    assert display.note is None


@pytest.mark.usefixtures("no_override")
def test_linux_without_xvfb_run_fails_with_install_instructions(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(shutil, "which", lambda _name: None)

    with pytest.raises(RuntimeError, match="sudo apt install xvfb"):
        hosttest.resolve_display(platform="linux")


@pytest.mark.usefixtures("no_override")
def test_other_platforms_run_native_with_a_notice() -> None:
    display = hosttest.resolve_display(platform="darwin")

    assert display.name == "native"
    assert display.prefix == []
    assert display.note is not None
    assert "darwin" in display.note


@pytest.mark.usefixtures("no_override")
def test_native_flag_wins_over_the_linux_backend(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(shutil, "which", found("/usr/bin/xvfb-run"))

    display = hosttest.resolve_display(native=True, platform="linux")

    assert display.name == "native"
    assert display.prefix == []
    assert display.note is None


def test_ozone_override_adds_no_prefix(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("CWTOOLS_TEST_DISPLAY", "ozone")

    display = hosttest.resolve_display(platform="win32")

    assert display.name == "ozone"
    assert display.prefix == []


def test_xvfb_override_applies_off_linux(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("CWTOOLS_TEST_DISPLAY", "xvfb")
    monkeypatch.setattr(shutil, "which", found("/bin/xvfb-run"))

    assert hosttest.resolve_display(platform="darwin").name == "xvfb"


def test_an_unknown_backend_is_rejected(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("CWTOOLS_TEST_DISPLAY", "wayland")

    with pytest.raises(RuntimeError, match="xvfb, ozone, native"):
        hosttest.resolve_display(platform="linux")


@pytest.mark.skipif(
    not (REPO_ROOT / "node_modules" / "@vscode" / "test-cli").is_dir(),
    reason="node_modules/@vscode/test-cli is not installed",
)
def test_vendored_test_cli_path_resolves_to_file() -> None:
    assert hosttest.TEST_CLI.is_file()


@pytest.mark.usefixtures("no_override")
def test_env_carries_the_resolved_backend() -> None:
    display = hosttest.resolve_display(platform="darwin")

    assert display.env()["CWTOOLS_TEST_DISPLAY"] == "native"


@pytest.fixture(name="installed_cli")
def installed_cli_fixture(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda name: f"/usr/bin/{name}")
    monkeypatch.setattr(hosttest, "TEST_CLI", INSTALLED_CLI)


@pytest.mark.usefixtures("installed_cli")
def test_repeats_the_label_flag() -> None:
    command = hosttest.test_cli_command(["smoke", "live"])

    assert command[2:] == ["--label", "smoke", "--label", "live"]


@pytest.mark.usefixtures("installed_cli")
def test_appends_coverage_watch_and_passthrough_arguments() -> None:
    command = hosttest.test_cli_command(
        ["unit"], coverage=True, watch=True, extra=["--grep", "hover"]
    )

    assert command[2:] == ["--label", "unit", "--coverage", "-w", "--grep", "hover"]


@pytest.mark.usefixtures("installed_cli")
def test_missing_test_cli_names_npm_ci(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(hosttest, "TEST_CLI", MISSING_CLI)

    with pytest.raises(RuntimeError, match="npm ci"):
        hosttest.test_cli_command(["unit"])


class Run(NamedTuple):
    status: int
    command: list[str]
    stderr: str


@pytest.fixture(name="run_main")
def run_main_fixture(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> Callable[..., Run]:
    def run(argv: list[str], *, returncode: int = 0, platform: str = "linux") -> Run:
        commands: list[list[str]] = []

        def fake_run(
            command: list[str], **_kwargs: object
        ) -> subprocess.CompletedProcess[bytes]:
            commands.append(list(command))
            return subprocess.CompletedProcess(command, returncode)

        monkeypatch.setenv("CWTOOLS_TEST_DISPLAY", "")
        monkeypatch.setattr(shutil, "which", lambda name: f"/usr/bin/{name}")
        monkeypatch.setattr(hosttest, "TEST_CLI", INSTALLED_CLI)
        monkeypatch.setattr(subprocess, "run", fake_run)
        monkeypatch.setattr(sys, "platform", platform)

        status = hosttest.main(argv)
        return Run(status, commands[-1], capsys.readouterr().err)

    return run


def test_defaults_to_the_unit_label_under_the_display_prefix(
    run_main: Callable[..., Run],
) -> None:
    result = run_main([])

    assert result.status == 0
    assert result.command[:2] == ["/usr/bin/xvfb-run", "-a"]
    assert result.command[2] == "/usr/bin/node"
    assert result.command[-2:] == ["--label", "unit"]


def test_passes_unrecognized_arguments_through(run_main: Callable[..., Run]) -> None:
    result = run_main(["--label", "host", "--grep", "completion"])

    assert result.command[-4:] == ["--label", "host", "--grep", "completion"]


def test_native_drops_the_display_prefix(run_main: Callable[..., Run]) -> None:
    assert run_main(["--native"]).command[0] == "/usr/bin/node"


# A runner that swallowed a failing exit code would turn every red CI run green,
# so this is the one that matters most.
def test_propagates_a_failing_exit_code(run_main: Callable[..., Run]) -> None:
    assert run_main([], returncode=3).status == 3


def test_reports_the_missing_backend_on_other_platforms(
    run_main: Callable[..., Run],
) -> None:
    result = run_main([], platform="darwin")

    assert result.status == 0
    assert result.command[0] == "/usr/bin/node"
    assert "darwin" in result.stderr


def test_says_nothing_when_a_backend_was_found(run_main: Callable[..., Run]) -> None:
    assert run_main([]).stderr == ""
