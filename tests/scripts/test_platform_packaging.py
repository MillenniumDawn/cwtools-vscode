from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from load import load_build

build = load_build("build")


def stage(server_bin_dir: Path, platforms: list[str]) -> None:
    for platform in platforms:
        directory = server_bin_dir / platform
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "cwtools-server").write_text(
            f"binary-{platform}", encoding="utf-8"
        )


def dirs_present(directory: Path) -> list[str]:
    if not directory.is_dir():
        return []
    return sorted(entry.name for entry in directory.iterdir() if entry.is_dir())


def has_binary(directory: Path, platform: str) -> bool:
    return (directory / platform / "cwtools-server").is_file()


class RunPlatformPackagingTests(unittest.TestCase):
    def test_each_per_platform_pass_sees_only_its_own_dir(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            server_bin_dir = root / "server" / "cwtools-server"
            holding = root / "temp" / "server-staging"
            platforms = ["linux-x64", "osx-arm64", "win32-x64"]
            stage(server_bin_dir, platforms)
            seen: list[tuple[str | None, list[str]]] = []

            def package_one(platform: str | None) -> list[str]:
                seen.append((platform, dirs_present(server_bin_dir)))
                return [platform or "universal"]

            build.run_platform_packaging(
                server_bin_dir, holding, platforms, package_one
            )
            per_platform = [item for item in seen if item[0] is not None]
            self.assertEqual(len(per_platform), len(platforms))
            for platform, present in per_platform:
                self.assertEqual(present, [platform])

    def test_restores_the_full_set_when_a_mid_platform_pass_throws(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            server_bin_dir = root / "server" / "cwtools-server"
            holding = root / "temp" / "server-staging"
            platforms = ["linux-x64", "osx-arm64", "win32-x64"]
            stage(server_bin_dir, platforms)

            def package_one(platform: str | None) -> list[str]:
                if platform == "osx-arm64":
                    raise RuntimeError("vsce boom")
                return [platform or "universal"]

            with self.assertRaisesRegex(RuntimeError, "vsce boom"):
                build.run_platform_packaging(
                    server_bin_dir, holding, platforms, package_one
                )
            self.assertEqual(dirs_present(server_bin_dir), platforms)
            for platform in platforms:
                self.assertTrue(has_binary(server_bin_dir, platform))
            self.assertFalse(holding.exists())

    def test_leaves_the_full_set_when_the_universal_pass_throws(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            server_bin_dir = root / "server" / "cwtools-server"
            holding = root / "temp" / "server-staging"
            platforms = ["linux-x64", "osx-arm64", "win32-x64"]
            stage(server_bin_dir, platforms)

            def package_one(platform: str | None) -> list[str]:
                if platform is None:
                    raise RuntimeError("universal boom")
                return [platform or "universal"]

            with self.assertRaises(RuntimeError):
                build.run_platform_packaging(
                    server_bin_dir, holding, platforms, package_one
                )
            self.assertEqual(dirs_present(server_bin_dir), platforms)
            for platform in platforms:
                self.assertTrue(has_binary(server_bin_dir, platform))
            self.assertFalse(holding.exists())

    def test_deletes_the_holding_dir_on_full_success(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            server_bin_dir = root / "server" / "cwtools-server"
            holding = root / "temp" / "server-staging"
            platforms = ["linux-x64", "osx-arm64", "win32-x64"]
            stage(server_bin_dir, platforms)
            vsixes = build.run_platform_packaging(
                server_bin_dir,
                holding,
                platforms,
                lambda platform: [platform or "universal"],
            )
            self.assertEqual(
                vsixes, ["linux-x64", "osx-arm64", "win32-x64", "universal"]
            )
            self.assertEqual(dirs_present(server_bin_dir), platforms)
            self.assertFalse(holding.exists())

    def test_restores_a_single_staged_set_on_mid_failure(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            server_bin_dir = root / "server" / "cwtools-server"
            holding = root / "temp" / "server-staging"
            platforms = ["linux-x64"]
            stage(server_bin_dir, platforms)

            def package_one(_platform: str | None) -> list[str]:
                raise RuntimeError("vsce boom")

            with self.assertRaises(RuntimeError):
                build.run_platform_packaging(
                    server_bin_dir, holding, platforms, package_one
                )
            self.assertEqual(dirs_present(server_bin_dir), ["linux-x64"])
            self.assertTrue(has_binary(server_bin_dir, "linux-x64"))
            self.assertFalse(holding.exists())


if __name__ == "__main__":
    unittest.main()
