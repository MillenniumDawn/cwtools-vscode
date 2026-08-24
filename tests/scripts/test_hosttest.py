from __future__ import annotations

import io
import os
import unittest
from contextlib import redirect_stderr
from unittest import mock

from load import load_build

hosttest = load_build("hosttest")


def without_override() -> mock._patch_dict:
    return mock.patch.dict(os.environ, {"CWTOOLS_TEST_DISPLAY": ""})


class ResolveDisplayTests(unittest.TestCase):
    def test_linux_uses_xvfb_run(self) -> None:
        with without_override(), mock.patch.object(
            hosttest.shutil, "which", return_value="/usr/bin/xvfb-run"
        ):
            display = hosttest.resolve_display(platform="linux")
        self.assertEqual(display.name, "xvfb")
        self.assertEqual(display.prefix, ["/usr/bin/xvfb-run", "-a"])
        self.assertIsNone(display.note)

    def test_linux_without_xvfb_run_fails_with_install_instructions(self) -> None:
        with without_override(), mock.patch.object(
            hosttest.shutil, "which", return_value=None
        ), self.assertRaisesRegex(RuntimeError, "sudo apt install xvfb"):
            hosttest.resolve_display(platform="linux")

    def test_other_platforms_run_native_with_a_notice(self) -> None:
        with without_override():
            display = hosttest.resolve_display(platform="darwin")
        self.assertEqual(display.name, "native")
        self.assertEqual(display.prefix, [])
        self.assertIsNotNone(display.note)
        self.assertIn("darwin", str(display.note))

    def test_native_flag_wins_over_the_linux_backend(self) -> None:
        with without_override(), mock.patch.object(
            hosttest.shutil, "which", return_value="/usr/bin/xvfb-run"
        ):
            display = hosttest.resolve_display(native=True, platform="linux")
        self.assertEqual(display.name, "native")
        self.assertEqual(display.prefix, [])
        self.assertIsNone(display.note)

    def test_ozone_override_adds_no_prefix(self) -> None:
        with mock.patch.dict(os.environ, {"CWTOOLS_TEST_DISPLAY": "ozone"}):
            display = hosttest.resolve_display(platform="win32")
        self.assertEqual(display.name, "ozone")
        self.assertEqual(display.prefix, [])

    def test_xvfb_override_applies_off_linux(self) -> None:
        with mock.patch.dict(
            os.environ, {"CWTOOLS_TEST_DISPLAY": "xvfb"}
        ), mock.patch.object(hosttest.shutil, "which", return_value="/bin/xvfb-run"):
            display = hosttest.resolve_display(platform="darwin")
        self.assertEqual(display.name, "xvfb")

    def test_an_unknown_backend_is_rejected(self) -> None:
        with mock.patch.dict(
            os.environ, {"CWTOOLS_TEST_DISPLAY": "wayland"}
        ), self.assertRaisesRegex(RuntimeError, "xvfb, ozone, native"):
            hosttest.resolve_display(platform="linux")

    def test_env_carries_the_resolved_backend(self) -> None:
        with without_override():
            display = hosttest.resolve_display(platform="darwin")
        self.assertEqual(display.env()["CWTOOLS_TEST_DISPLAY"], "native")


# The runner only asks whether the CLI entry point exists, so a checked-in file
# stands in for it and the suite does not need node_modules installed.
INSTALLED_CLI = hosttest.REPO_ROOT / "package.json"
MISSING_CLI = hosttest.REPO_ROOT / "node_modules" / "@vscode" / "not-installed.mjs"


class TestCliCommandTests(unittest.TestCase):
    def setUp(self) -> None:
        self.node = mock.patch.object(
            hosttest.shutil, "which", return_value="/usr/bin/node"
        )
        self.cli = mock.patch.object(hosttest, "TEST_CLI", INSTALLED_CLI)
        self.node.start()
        self.cli.start()
        self.addCleanup(self.node.stop)
        self.addCleanup(self.cli.stop)

    def test_repeats_the_label_flag(self) -> None:
        command = hosttest.test_cli_command(["smoke", "live"])
        self.assertEqual(command[2:], ["--label", "smoke", "--label", "live"])

    def test_appends_coverage_watch_and_passthrough_arguments(self) -> None:
        command = hosttest.test_cli_command(
            ["unit"], coverage=True, watch=True, extra=["--grep", "hover"]
        )
        self.assertEqual(
            command[2:],
            ["--label", "unit", "--coverage", "-w", "--grep", "hover"],
        )

    def test_missing_test_cli_names_npm_ci(self) -> None:
        with mock.patch.object(
            hosttest, "TEST_CLI", MISSING_CLI
        ), self.assertRaisesRegex(RuntimeError, "npm ci"):
            hosttest.test_cli_command(["unit"])


def fake_which(name: str) -> str:
    return f"/usr/bin/{name}"


class MainTests(unittest.TestCase):
    def call_main(
        self, argv: list[str], *, returncode: int = 0, platform: str = "linux"
    ) -> tuple[int, list[str], str]:
        completed = mock.Mock(returncode=returncode)
        stderr = io.StringIO()
        with without_override(), mock.patch.object(
            hosttest.shutil, "which", side_effect=fake_which
        ), mock.patch.object(hosttest, "TEST_CLI", INSTALLED_CLI), mock.patch.object(
            hosttest.subprocess, "run", return_value=completed
        ) as run, mock.patch.object(
            hosttest.sys, "platform", platform
        ), redirect_stderr(
            stderr
        ):
            status = hosttest.main(argv)
        return status, list(run.call_args.args[0]), stderr.getvalue()

    def run_main(self, argv: list[str]) -> list[str]:
        status, command, _ = self.call_main(argv)
        self.assertEqual(status, 0)
        return command

    def test_defaults_to_the_unit_label_under_the_display_prefix(self) -> None:
        command = self.run_main([])
        self.assertEqual(command[:2], ["/usr/bin/xvfb-run", "-a"])
        self.assertEqual(command[2], "/usr/bin/node")
        self.assertEqual(command[-2:], ["--label", "unit"])

    def test_passes_unrecognized_arguments_through(self) -> None:
        command = self.run_main(["--label", "host", "--grep", "completion"])
        self.assertEqual(command[-4:], ["--label", "host", "--grep", "completion"])

    def test_native_drops_the_display_prefix(self) -> None:
        command = self.run_main(["--native"])
        self.assertEqual(command[0], "/usr/bin/node")

    # A runner that swallowed a failing exit code would turn every red CI run
    # green, so this is the one that matters most.
    def test_propagates_a_failing_exit_code(self) -> None:
        status, _, _ = self.call_main([], returncode=3)
        self.assertEqual(status, 3)

    def test_reports_the_missing_backend_on_other_platforms(self) -> None:
        status, command, stderr = self.call_main([], platform="darwin")
        self.assertEqual(status, 0)
        self.assertEqual(command[0], "/usr/bin/node")
        self.assertIn("darwin", stderr)

    def test_says_nothing_when_a_backend_was_found(self) -> None:
        _, _, stderr = self.call_main([])
        self.assertEqual(stderr, "")


if __name__ == "__main__":
    unittest.main()
