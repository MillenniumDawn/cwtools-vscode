from __future__ import annotations

import contextlib
import json
import os
import shutil
import stat
import subprocess
import sys
from collections.abc import Callable, Mapping
from pathlib import Path
from typing import Any

from changelog import release_notes, top_changelog_version
from paths import (
    ARTIFACTS_ROOT,
    ENGINE_ROOT,
    EXTENSION_DIST_ROOT,
    EXTENSION_DOCS_ROOT,
    EXTENSION_PACKAGE_ROOT,
    EXTENSION_TEST_ROOT,
    EXTENSION_WEBVIEW_ROOT,
    REPO_ROOT,
    VSIX_ROOT,
)

VSIX_TARGETS = {
    "win-x64": "win32-x64",
    "linux-x64": "linux-x64",
    "linux-arm64": "linux-arm64",
    "osx-x64": "darwin-x64",
    "osx-arm64": "darwin-arm64",
}

SERVER_BIN_DIR = EXTENSION_DIST_ROOT / "bin" / "server" / "cwtools-server"


def _exe(cmd: str) -> str:
    found = shutil.which(cmd)
    return found if found else cmd


def run(
    cmd: str,
    args: list[str],
    *,
    cwd: Path | None = None,
    env: Mapping[str, str] | None = None,
) -> None:
    display = " ".join([cmd, *args])
    print(f"> {display}")
    result = subprocess.run(
        [_exe(cmd), *args],
        cwd=cwd or REPO_ROOT,
        env=None if env is None else dict(env),
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"command failed ({result.returncode}): {display}")


def run_or_null(
    cmd: str,
    args: list[str],
    *,
    cwd: Path | None = None,
    env: Mapping[str, str] | None = None,
) -> int | None:
    result = subprocess.run(
        [_exe(cmd), *args],
        cwd=cwd or REPO_ROOT,
        env=None if env is None else dict(env),
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    return result.returncode


def run_capture(
    cmd: str,
    args: list[str],
    *,
    cwd: Path | None = None,
    env: Mapping[str, str] | None = None,
) -> str:
    result = subprocess.run(
        [_exe(cmd), *args],
        cwd=cwd or REPO_ROOT,
        env=None if env is None else dict(env),
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        display = " ".join([cmd, *args])
        raise RuntimeError(f"command failed ({result.returncode}): {display}")
    return result.stdout


def rust_workspace() -> Path:
    from_env = os.environ.get("CWTOOLS_RUST_WORKSPACE", "").strip()
    if from_env:
        return (
            Path(from_env).resolve()
            if Path(from_env).is_absolute()
            else (REPO_ROOT / from_env)
        )
    return ENGINE_ROOT


def remove_tree(path: Path) -> None:
    with contextlib.suppress(FileNotFoundError):
        shutil.rmtree(path)


def copy_dir(src: Path, dest: Path) -> None:
    dest.mkdir(parents=True, exist_ok=True)
    for entry in src.iterdir():
        target = dest / entry.name
        if entry.is_dir():
            copy_dir(entry, target)
        else:
            shutil.copy2(entry, target)


def build_and_deploy_rust_server() -> None:
    workspace = rust_workspace()
    run("cargo", ["build", "--release", "-p", "cwtools_lsp"], cwd=workspace)
    bin_name = "cwtools-server.exe" if os.name == "nt" else "cwtools-server"
    built = workspace / "target" / "release" / bin_name
    if not built.is_file():
        raise RuntimeError(
            f"Rust server binary not found at '{built}' after build. Check the crate "
            "name/target, or point CWTOOLS_RUST_WORKSPACE at the right engine checkout "
            f"(currently '{workspace}')."
        )
    out_dir = EXTENSION_DIST_ROOT / "bin" / "server" / "cwtools-server"
    remove_tree(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    dest = out_dir / bin_name
    shutil.copy2(built, dest)
    if os.name != "nt":
        dest.chmod(dest.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def build_client() -> None:
    run("npm", ["run", "compile:code"])


def copy_package_inputs() -> None:
    copy_dir(EXTENSION_PACKAGE_ROOT, EXTENSION_DIST_ROOT)


def copy_docs() -> None:
    shutil.copy2(EXTENSION_DOCS_ROOT / "README.md", EXTENSION_DIST_ROOT / "README.md")
    for name in ("LICENSE.md", "CHANGELOG.md"):
        shutil.copy2(REPO_ROOT / name, EXTENSION_DIST_ROOT / name)


def copy_webview_css() -> None:
    dest = EXTENSION_DIST_ROOT / "bin" / "client" / "webview"
    dest.mkdir(parents=True, exist_ok=True)
    for path in EXTENSION_WEBVIEW_ROOT.iterdir():
        if path.suffix == ".css":
            shutil.copy2(path, dest / path.name)


def copy_test_samples() -> None:
    copy_dir(
        EXTENSION_TEST_ROOT / "workspaces" / "stellaris",
        EXTENSION_DIST_ROOT / "bin" / "client" / "test" / "workspaces" / "stellaris",
    )


def clean_extension_dist() -> None:
    remove_tree(EXTENSION_DIST_ROOT)


def assemble_client() -> None:
    copy_package_inputs()
    build_client()
    copy_docs()
    copy_webview_css()
    copy_test_samples()


def package_vsix(target: str | None = None) -> list[str]:
    args = ["--no-install", "vsce", "package", "--no-dependencies"]
    if resolve_version()["preRelease"]:
        args.append("--pre-release")
    if target:
        args.extend(["--target", target])
    run("npx", args, cwd=EXTENSION_DIST_ROOT)
    VSIX_ROOT.mkdir(parents=True, exist_ok=True)
    packaged: list[str] = []
    for path in EXTENSION_DIST_ROOT.iterdir():
        if path.suffix == ".vsix":
            dest = VSIX_ROOT / path.name
            path.replace(dest)
            packaged.append(str(dest))
    return packaged


def staged_platforms() -> list[str]:
    if not SERVER_BIN_DIR.is_dir():
        return []
    return sorted(
        entry.name
        for entry in SERVER_BIN_DIR.iterdir()
        if entry.is_dir() and entry.name in VSIX_TARGETS
    )


def run_platform_packaging(
    server_bin_dir: Path,
    holding: Path,
    platforms: list[str],
    package_one: Callable[[str | None], list[str]],
) -> list[str]:
    remove_tree(holding)
    holding.parent.mkdir(parents=True, exist_ok=True)
    server_bin_dir.replace(holding)

    vsixes: list[str] = []
    restored = False
    try:
        for platform in platforms:
            remove_tree(server_bin_dir)
            copy_dir(holding / platform, server_bin_dir / platform)
            target = VSIX_TARGETS.get(platform, platform)
            print(f"packaging {target} ({platform})")
            vsixes.extend(package_one(platform))
        remove_tree(server_bin_dir)
        copy_dir(holding, server_bin_dir)
        restored = True
        print("packaging the universal fallback vsix")
        vsixes.extend(package_one(None))
    finally:
        if not restored:
            remove_tree(server_bin_dir)
            copy_dir(holding, server_bin_dir)
        remove_tree(holding)
    return vsixes


def package_all_vsixes() -> list[str]:
    platforms = staged_platforms()
    if not platforms:
        print("no per-platform server binaries staged; packaging a single vsix")
        return package_vsix()
    holding = ARTIFACTS_ROOT / "package" / "server-staging"
    return run_platform_packaging(
        SERVER_BIN_DIR,
        holding,
        platforms,
        lambda platform: (
            package_vsix() if platform is None else package_vsix(VSIX_TARGETS[platform])
        ),
    )


def resolve_version_from(env: Mapping[str, str], changelog: str) -> dict[str, Any]:
    version = env.get("CWTOOLS_BUILD_VERSION", "").strip()
    if version:
        tag = env.get("CWTOOLS_RELEASE_TAG", "").strip() or f"v{version}"
    else:
        flag = env.get("TAG_RELEASE", "")
        is_tag_release = flag.lower() in {"1", "true"}
        tag = env.get("GITHUB_REF_NAME", "").strip() if is_tag_release else ""
        if not tag:
            tag = f"v{top_changelog_version(changelog)}"
        version = tag.removeprefix("v")
    return {
        "version": version,
        "tag": tag,
        "preRelease": "-" in tag,
    }


def nightly_identity_from(env: Mapping[str, str], changelog: str) -> dict[str, str]:
    try:
        run_number = int(env.get("GITHUB_RUN_NUMBER", "").strip())
        run_attempt = int(env.get("GITHUB_RUN_ATTEMPT", "1").strip())
        base = top_changelog_version(changelog).split("-", maxsplit=1)[0]
        major, minor, patch = (int(part) for part in base.split("."))
    except ValueError as error:
        raise RuntimeError("nightly version inputs must be integers") from error
    if run_number < 1 or run_attempt < 1:
        raise RuntimeError("nightly run number and attempt must be positive")
    version = f"{major}.{minor}.{patch + run_number}"
    return {
        "version": version,
        "tag": f"v{version}-nightly.{run_attempt}",
    }


def read_changelog() -> str:
    return (REPO_ROOT / "CHANGELOG.md").read_text(encoding="utf-8")


def resolve_version() -> dict[str, Any]:
    return resolve_version_from(os.environ, read_changelog())


def set_release_version(version: str) -> None:
    manifest_path = EXTENSION_DIST_ROOT / "package.json"
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError(f"could not parse {manifest_path}: {error}") from error
    if not isinstance(manifest, dict):
        raise TypeError(f"could not parse {manifest_path}: not an object")
    manifest["version"] = version
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"set dist/extension/package.json version to {version}")


def find_vsixes() -> list[str]:
    files = (
        sorted(path.name for path in VSIX_ROOT.iterdir() if path.suffix == ".vsix")
        if VSIX_ROOT.is_dir()
        else []
    )
    if not files:
        raise RuntimeError(
            "no .vsix found in artifacts/vsix; run package-prebuilt first"
        )
    return [str(VSIX_ROOT / name) for name in files]


def publish_github_release(
    tag: str, version: str, pre_release: bool, vsixes: list[str]
) -> None:
    notes = release_notes(read_changelog(), version)
    notes_file = VSIX_ROOT / "release-notes.md"
    notes_file.write_text(notes, encoding="utf-8")
    if run_or_null("gh", ["release", "view", tag]) == 0:
        print(f"release {tag} already exists; deleting before recreate")
        run("gh", ["release", "delete", tag, "--yes"])
    args = [
        "release",
        "create",
        tag,
        *vsixes,
        "--title",
        tag,
        "--notes-file",
        str(notes_file),
    ]
    if pre_release:
        args.append("--prerelease")
    run("gh", args)


def publish_to_marketplace(vsixes: list[str]) -> None:
    token = os.environ.get("VSCE_TOKEN", "").strip()
    if not token:
        is_tag_release = os.environ.get("TAG_RELEASE", "").lower() in {"1", "true"}
        if os.environ.get("CI") and not is_tag_release:
            print(
                "No VSCE_TOKEN set; skipping VS Code Marketplace publish "
                "(not a tag release)."
            )
            return
        raise RuntimeError("VSCE_TOKEN is not set; cannot publish to the Marketplace.")
    run(
        "npx",
        ["--no-install", "vsce", "publish", "--pat", token, "--packagePath", *vsixes],
    )


def cmd_nightly_identity() -> None:
    identity = nightly_identity_from(os.environ, read_changelog())
    print(f"version={identity['version']}")
    print(f"tag={identity['tag']}")


def cmd_compile() -> None:
    assemble_client()


def cmd_quick() -> None:
    clean_extension_dist()
    assemble_client()
    build_and_deploy_rust_server()


def cmd_package() -> None:
    clean_extension_dist()
    assemble_client()
    build_and_deploy_rust_server()
    set_release_version(resolve_version()["version"])
    package_vsix()


def cmd_package_prebuilt() -> list[str]:
    set_release_version(resolve_version()["version"])
    return package_all_vsixes()


def cmd_publish_prebuilt() -> None:
    resolved = resolve_version()
    vsixes = find_vsixes()
    publish_github_release(
        resolved["tag"], resolved["version"], resolved["preRelease"], vsixes
    )
    publish_to_marketplace(vsixes)


def cmd_release_prebuilt() -> None:
    cmd_package_prebuilt()
    cmd_publish_prebuilt()


def release_preflight() -> None:
    tracked_status = run_or_null("git", ["diff", "--quiet", "HEAD"])
    if tracked_status == 1:
        raise RuntimeError(
            "working tree has uncommitted changes; commit them before tagging a release"
        )
    if tracked_status != 0:
        raise RuntimeError(f"git diff check failed ({tracked_status})")
    untracked = run_capture(
        "git", ["ls-files", "--others", "--exclude-standard"]
    ).splitlines()
    if untracked:
        paths = "\n".join(f"  {path}" for path in untracked)
        raise RuntimeError(
            "working tree has untracked files; commit or remove them before tagging "
            f"a release:\n{paths}"
        )
    run("git", ["fetch", "--quiet", "origin", "main"])
    remote_status = run_or_null(
        "git", ["merge-base", "--is-ancestor", "HEAD", "origin/main"]
    )
    if remote_status == 1:
        head = run_capture("git", ["rev-parse", "--short", "HEAD"]).strip()
        raise RuntimeError(
            f"HEAD {head} is not present on origin/main; push it before tagging "
            "a release"
        )
    if remote_status != 0:
        raise RuntimeError(f"git merge-base check failed ({remote_status})")


def cmd_release() -> None:
    resolved = resolve_version()
    version = resolved["version"]
    tag = resolved["tag"]
    release_notes(read_changelog(), version)
    release_preflight()
    if (
        run_or_null("git", ["rev-parse", "--verify", "--quiet", f"refs/tags/{tag}"])
        == 0
    ):
        raise RuntimeError(f"tag {tag} already exists locally")
    if (
        run_or_null("git", ["ls-remote", "--exit-code", "origin", f"refs/tags/{tag}"])
        == 0
    ):
        raise RuntimeError(f"tag {tag} already exists on origin")
    run("git", ["tag", tag])
    run("git", ["push", "origin", tag])
    print(
        f"pushed {tag}; the Release workflow now builds, smoke-tests, and publishes it."
    )


COMMANDS: dict[str, Callable[[], object]] = {
    "nightly-identity": cmd_nightly_identity,
    "compile": cmd_compile,
    "quick": cmd_quick,
    "package": cmd_package,
    "package-prebuilt": cmd_package_prebuilt,
    "publish-prebuilt": cmd_publish_prebuilt,
    "release-prebuilt": cmd_release_prebuilt,
    "release": cmd_release,
}


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    cmd = args[0] if args else "quick"
    handler = COMMANDS.get(cmd)
    if handler is None:
        known = ", ".join(COMMANDS)
        print(f"unknown command '{cmd}'. Known: {known}", file=sys.stderr)
        return 1
    handler()
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, TypeError, OSError) as error:
        sys.stderr.write(f"{error}\n")
        raise SystemExit(1) from error
