from __future__ import annotations

import sys
import unittest

from load import load_build

esbuild = load_build("esbuild")


class FakeProcess:
    def __init__(self, code: int | None) -> None:
        self.code = code

    def poll(self) -> int | None:
        return self.code


class EsbuildArgsTests(unittest.TestCase):
    def test_extension_bundle_is_node_cjs(self) -> None:
        args = esbuild.extension_args(watch=False)
        joined = " ".join(args)
        self.assertIn("--platform=node", joined)
        self.assertIn("--format=cjs", joined)
        self.assertIn("--external:vscode", joined)
        self.assertIn("extension.ts", joined)
        self.assertNotIn("--watch", joined)

    def test_webview_bundle_is_browser_iife(self) -> None:
        args = esbuild.webview_args(watch=False, dev=False)
        joined = " ".join(args)
        self.assertIn("--platform=browser", joined)
        self.assertIn("--format=iife", joined)
        self.assertIn("--global-name=cwtoolsgraph", joined)
        self.assertIn('process.env.NODE_ENV="production"', joined)
        self.assertIn("graph.ts", joined)

    def test_dev_and_watch_flags(self) -> None:
        args = esbuild.webview_args(watch=True, dev=True)
        joined = " ".join(args)
        self.assertIn('process.env.NODE_ENV="development"', joined)
        self.assertIn("--watch", joined)
        self.assertIn("--watch", " ".join(esbuild.extension_args(watch=True)))

    def test_bundle_commands_use_the_platform_esbuild_entrypoint(self) -> None:
        commands = esbuild.bundle_commands(watch=False, dev=False)
        self.assertEqual(len(commands), 2)
        esbuild_index = 1 if sys.platform == "win32" else 0
        for command in commands:
            self.assertEqual(command[esbuild_index], str(esbuild.esbuild_bin()))

    def test_watch_returns_when_the_second_bundle_exits(self) -> None:
        status = esbuild.wait_for_watcher_exit(
            [FakeProcess(None), FakeProcess(7)], poll_interval=0
        )
        self.assertEqual(status, 7)

    def test_watch_treats_an_early_clean_exit_as_failure(self) -> None:
        status = esbuild.wait_for_watcher_exit(
            [FakeProcess(None), FakeProcess(0)], poll_interval=0
        )
        self.assertEqual(status, 1)


if __name__ == "__main__":
    unittest.main()
