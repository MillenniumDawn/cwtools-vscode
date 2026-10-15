from __future__ import annotations

import json
import shutil
import subprocess
import sys
from collections.abc import Callable
from typing import NamedTuple

import pytest

import hosttest  # pyright: ignore[reportMissingImports]
from paths import REPO_ROOT  # pyright: ignore[reportMissingImports]

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
def test_unsupported_platform_fails_closed() -> None:
    with pytest.raises(
        RuntimeError, match="automated host tests currently require Linux"
    ):
        hosttest.resolve_display(platform="darwin")


@pytest.mark.usefixtures("no_override")
def test_native_flag_wins_over_the_linux_backend(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("CI", raising=False)
    monkeypatch.delenv("GITHUB_ACTIONS", raising=False)
    monkeypatch.setattr(shutil, "which", found("/usr/bin/xvfb-run"))

    display = hosttest.resolve_display(native=True, platform="linux")

    assert display.name == "native"
    assert display.prefix == []
    assert display.note is None


def test_ozone_override_adds_no_prefix(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("CWTOOLS_TEST_DISPLAY", "ozone")

    display = hosttest.resolve_display(platform="linux")

    assert display.name == "ozone"
    assert display.prefix == []


def test_xvfb_override_fails_off_linux(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("CWTOOLS_TEST_DISPLAY", "xvfb")
    monkeypatch.setattr(shutil, "which", found("/bin/xvfb-run"))

    with pytest.raises(RuntimeError, match="only supported on Linux"):
        hosttest.resolve_display(platform="darwin")


def test_native_environment_override_is_rejected(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("CWTOOLS_TEST_DISPLAY", "native")

    with pytest.raises(RuntimeError, match="explicit local debugging"):
        hosttest.resolve_display(platform="linux")


@pytest.mark.parametrize("ci_name,ci_value", [("CI", "1"), ("GITHUB_ACTIONS", "true")])
def test_native_flag_is_rejected_in_ci(
    monkeypatch: pytest.MonkeyPatch, ci_name: str, ci_value: str
) -> None:
    monkeypatch.setenv(ci_name, ci_value)

    with pytest.raises(RuntimeError, match="disabled in CI"):
        hosttest.resolve_display(native=True, platform="linux")


@pytest.mark.parametrize("ci_value", ["1", "true", "yes"])
def test_ci_detection_accepts_truthy_values(
    monkeypatch: pytest.MonkeyPatch, ci_value: str
) -> None:
    monkeypatch.setenv("CI", ci_value)

    assert hosttest.in_ci()


@pytest.mark.parametrize("ci_value", ["", "0", "false", "no", "off"])
def test_ci_detection_accepts_false_values(
    monkeypatch: pytest.MonkeyPatch, ci_value: str
) -> None:
    monkeypatch.setenv("CI", ci_value)
    monkeypatch.setenv("GITHUB_ACTIONS", "false")

    assert not hosttest.in_ci()


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
def test_virtual_display_environment_drops_inherited_desktops(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(shutil, "which", found("/usr/bin/xvfb-run"))
    for name in hosttest.DISPLAY_ENV_VARS:
        monkeypatch.setenv(name, f"inherited-{name.lower()}")
    monkeypatch.setenv("CWTOOLS_TEST_CONFIG_DISCOVERY", "1")

    env = hosttest.resolve_display(platform="linux").env()

    assert env["CWTOOLS_TEST_DISPLAY"] == "xvfb"
    assert env["CWTOOLS_TEST_LAUNCHER"] == "hosttest"
    assert env["CWTOOLS_TEST_NATIVE_DEBUG"] == "0"
    assert "CWTOOLS_TEST_CONFIG_DISCOVERY" not in env
    assert all(name not in env for name in hosttest.DISPLAY_ENV_VARS)


def test_test_cli_config_requires_launcher_and_selects_safe_platform() -> None:
    node = shutil.which("node")
    if node is None:
        pytest.skip("node is not installed")
    config_module = REPO_ROOT / "scripts/build/hosttest_config.mjs"
    script = (
        "import {hostTestLaunchArgs} from "
        "'" + config_module.as_uri() + "'; "
        "console.log(JSON.stringify(hostTestLaunchArgs(JSON.parse(process.argv[1]))))"
    )

    direct = subprocess.run(
        [node, "--input-type=module", "--eval", script, json.dumps({})],
        cwd=REPO_ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    assert direct.returncode != 0
    assert "must be launched through scripts/build/hosttest.py" in direct.stderr

    launched = subprocess.run(
        [
            node,
            "--input-type=module",
            "--eval",
            script,
            json.dumps(
                {
                    "CWTOOLS_TEST_LAUNCHER": "hosttest",
                    "CWTOOLS_TEST_DISPLAY": "xvfb",
                }
            ),
        ],
        cwd=REPO_ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    if sys.platform.startswith("linux"):
        assert launched.returncode == 0, launched.stderr
        assert json.loads(launched.stdout) == ["--ozone-platform=x11"]
    else:
        assert launched.returncode != 0
        assert "only supported on Linux" in launched.stderr

    discovery = subprocess.run(
        [
            node,
            "--input-type=module",
            "--eval",
            script,
            json.dumps({"CWTOOLS_TEST_CONFIG_DISCOVERY": "1"}),
        ],
        cwd=REPO_ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    assert discovery.returncode == 0, discovery.stderr
    assert json.loads(discovery.stdout) == []


def test_native_debug_keeps_the_interactive_desktop(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("CI", raising=False)
    monkeypatch.delenv("GITHUB_ACTIONS", raising=False)
    monkeypatch.setenv("DISPLAY", ":9")

    env = hosttest.resolve_display(native=True, platform="linux").env()

    assert env["DISPLAY"] == ":9"
    assert env["CWTOOLS_TEST_NATIVE_DEBUG"] == "1"


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
def test_missing_node_is_reported(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda _name: None)

    with pytest.raises(RuntimeError, match=r"^node is not on PATH$"):
        hosttest.test_cli_command(["unit"])


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
        monkeypatch.delenv("CI", raising=False)
        monkeypatch.delenv("GITHUB_ACTIONS", raising=False)
        monkeypatch.setattr(shutil, "which", lambda name: f"/usr/bin/{name}")
        monkeypatch.setattr(hosttest, "TEST_CLI", INSTALLED_CLI)
        monkeypatch.setattr(subprocess, "run", fake_run)
        monkeypatch.setattr(sys, "platform", platform)

        status = hosttest.main(argv)
        return Run(status, commands[-1], capsys.readouterr().err)

    return run


def test_defaults_to_the_smoke_label_under_the_display_prefix(
    run_main: Callable[..., Run],
) -> None:
    result = run_main([])

    assert result.status == 0
    assert result.command[:2] == ["/usr/bin/xvfb-run", "-a"]
    assert result.command[2] == "/usr/bin/node"
    assert result.command[-2:] == ["--label", "smoke"]


def test_passes_unrecognized_arguments_through(run_main: Callable[..., Run]) -> None:
    result = run_main(["--label", "host", "--grep", "completion"])

    assert result.command[-4:] == ["--label", "host", "--grep", "completion"]


def test_native_drops_the_display_prefix(run_main: Callable[..., Run]) -> None:
    assert run_main(["--native"]).command[0] == "/usr/bin/node"


# A runner that swallowed a failing exit code would turn every red CI run green,
# so this is the one that matters most.
def test_propagates_a_failing_exit_code(run_main: Callable[..., Run]) -> None:
    assert run_main([], returncode=3).status == 3


# The unit tests above use a fake subprocess.run, so they prove hosttest.py
# forwards a non-zero status once the child returns it. The remaining untested
# hop is xvfb-run itself: some old builds returned the cleanup command's exit
# code instead of the wrapped command's. This test exercises the real wrapper.
@pytest.mark.skipif(
    shutil.which("xvfb-run") is None,
    reason="xvfb-run is not installed",
)
def test_xvfb_run_preserves_a_failing_child_exit_code() -> None:
    xvfb_run = shutil.which("xvfb-run")
    assert xvfb_run is not None
    result = subprocess.run(
        [xvfb_run, "-a", sys.executable, "-c", "raise SystemExit(5)"],
        cwd=REPO_ROOT,
        check=False,
        capture_output=True,
    )
    assert result.returncode == 5


def test_reports_the_missing_backend_on_other_platforms(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("CWTOOLS_TEST_DISPLAY", "")

    with pytest.raises(RuntimeError, match="host tests currently require Linux"):
        hosttest.resolve_display(platform="darwin")


def test_says_nothing_when_a_backend_was_found(run_main: Callable[..., Run]) -> None:
    assert run_main([]).stderr == ""
