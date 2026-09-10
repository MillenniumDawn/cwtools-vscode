from __future__ import annotations

import contextlib
import json
import os
import shutil
import stat
import subprocess
import sys
import time
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


def build_client(*, release: bool = False) -> None:
    script = "compile:code:release" if release else "compile:code"
    run("npm", ["run", script])


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


def assemble_client(*, release: bool = False) -> None:
    copy_package_inputs()
    build_client(release=release)
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
    if not packaged:
        raise RuntimeError("vsce produced no .vsix file")
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
    flat_files = sorted(
        entry.name for entry in server_bin_dir.iterdir() if entry.is_file()
    )
    if platforms and flat_files:
        carried = ", ".join(flat_files)
        raise RuntimeError(
            f"cannot package platform VSIXes with flat server binaries: {carried}"
        )
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


# VS Code's channel convention: stable takes the even minors, pre-release the
# odd minor directly above the current stable one. The run number is the patch,
# so the pre-release line only ever climbs, and the Marketplace version stays
# numeric (VS Code rejects SemVer prerelease suffixes). Only the Git tag carries
# the rerun suffix.
def prerelease_identity_from(env: Mapping[str, str], changelog: str) -> dict[str, str]:
    try:
        run_number = int(env.get("GITHUB_RUN_NUMBER", "").strip())
        run_attempt = int(env.get("GITHUB_RUN_ATTEMPT", "1").strip())
        base = top_changelog_version(changelog).split("-", maxsplit=1)[0]
        major, minor, patch = (int(part) for part in base.split("."))
    except ValueError as error:
        raise RuntimeError("pre-release version inputs must be integers") from error
    if run_number < 1 or run_attempt < 1:
        raise RuntimeError("pre-release run number and attempt must be positive")
    if minor % 2 != 0:
        raise RuntimeError(
            f"pre-release requires an even stable minor, but the changelog is at "
            f"{major}.{minor}.{patch}; releases must not land on the odd "
            "pre-release line"
        )
    version = f"{major}.{minor + 1}.{run_number}"
    return {
        "version": version,
        "tag": f"v{version}-pre.{run_attempt}",
    }


# Releases stay on even minors, so a minor bump skips the odd pre-release line
# that sits directly above the current stable version.
def next_release_version(current: str, bump: str) -> str:
    try:
        major, minor, patch = (
            int(part) for part in current.split("-", maxsplit=1)[0].split(".")
        )
    except ValueError as error:
        raise RuntimeError(f"cannot bump '{current}': not a x.y.z version") from error
    if minor % 2 != 0:
        raise RuntimeError(
            f"cannot bump '{current}': {major}.{minor} is a pre-release line"
        )
    if bump == "patch":
        return f"{major}.{minor}.{patch + 1}"
    if bump == "minor":
        return f"{major}.{minor + 2}.0"
    if bump == "major":
        return f"{major + 1}.0.0"
    raise RuntimeError(f"unknown bump '{bump}'; expected patch, minor, or major")


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


# A pre-release version has no CHANGELOG section to draw notes from, so it gets
# a generated blurb instead. Keeping the branch here rather than in the workflow
# is what lets one publish job serve both channels.
def prerelease_notes(version: str) -> str:
    commit = os.environ.get("GITHUB_SHA", "").strip() or "this commit"
    return (
        f"Automated pre-release build {version} from commit {commit}.\n"
        "\n"
        "Pick the VSIX for your platform, or use the universal VSIX. The same "
        "build is published to the VS Code Marketplace and Open VSX on the "
        "pre-release channel, so installing from there gets you this build.\n"
    )


def publish_github_release(
    tag: str, version: str, pre_release: bool, vsixes: list[str]
) -> None:
    notes = (
        prerelease_notes(version)
        if pre_release
        else release_notes(read_changelog(), version)
    )
    notes_file = VSIX_ROOT / "release-notes.md"
    # Same contract as --skip-duplicate on the registries: an existing release
    # is the previous attempt's, so a re-run of the job leaves it alone rather
    # than deleting and recreating it under the same tag. Shipping different
    # code means a new version, never a reused tag.
    if run_or_null("gh", ["release", "view", tag]) == 0:
        print(f"release {tag} already exists; skipping")
        return
    notes_file.write_text(notes, encoding="utf-8")
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
    # The publish workflow runs on a branch ref, so without a target gh would
    # tag whatever the default branch head is by the time it runs.
    target = os.environ.get("GITHUB_SHA", "").strip()
    if target:
        args += ["--target", target]
    if pre_release:
        args.append("--prerelease")
    run("gh", args)


# One gallery upload per attempt, three attempts, backing off between them. The
# Marketplace times out on /_apis/gallery often enough that a single batched
# upload of every platform vsix is a coin flip; --skip-duplicate is what makes
# the retry (and a re-run of the job) safe.
MARKETPLACE_ATTEMPTS = 3
MARKETPLACE_BACKOFF_SECONDS = (15, 45)


def _sleep(seconds: float) -> None:
    time.sleep(seconds)


def publish_one_to_marketplace(vsix: str, args: list[str]) -> None:
    for attempt in range(1, MARKETPLACE_ATTEMPTS + 1):
        try:
            run("npx", [*args, "--packagePath", vsix])
            return
        except RuntimeError as error:
            if attempt == MARKETPLACE_ATTEMPTS:
                raise
            delay = MARKETPLACE_BACKOFF_SECONDS[attempt - 1]
            print(f"{vsix}: {error}; retrying in {delay}s")
            _sleep(delay)


def publish_to_marketplace(vsixes: list[str], pre_release: bool = False) -> None:
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
    args = ["--no-install", "vsce", "publish", "--pat", token, "--skip-duplicate"]
    if pre_release:
        args.append("--pre-release")
    # Every vsix is attempted even after one fails, so a single flaky platform
    # cannot strand the other five; a re-run then only retries what is missing.
    failed: list[str] = []
    for vsix in vsixes:
        try:
            publish_one_to_marketplace(vsix, args)
        except RuntimeError as error:
            print(f"::error::{vsix} failed to publish: {error}")
            failed.append(vsix)
    if failed:
        raise RuntimeError("Marketplace publish failed for: " + ", ".join(failed))


def cmd_prerelease_identity() -> None:
    identity = prerelease_identity_from(os.environ, read_changelog())
    print(f"version={identity['version']}")
    print(f"tag={identity['tag']}")


def cmd_compile() -> None:
    assemble_client()


def cmd_compile_release() -> None:
    assemble_client(release=True)


def cmd_quick() -> None:
    clean_extension_dist()
    assemble_client()
    build_and_deploy_rust_server()


def cmd_package() -> None:
    clean_extension_dist()
    assemble_client(release=True)
    build_and_deploy_rust_server()
    set_release_version(resolve_version()["version"])
    package_vsix()


def cmd_package_prebuilt() -> list[str]:
    build_client(release=True)
    set_release_version(resolve_version()["version"])
    return package_all_vsixes()


def cmd_publish_prebuilt() -> None:
    resolved = resolve_version()
    vsixes = find_vsixes()
    publish_github_release(
        resolved["tag"], resolved["version"], resolved["preRelease"], vsixes
    )
    publish_to_marketplace(vsixes, resolved["preRelease"])


# The two halves of publish-prebuilt on their own. The publish workflow runs
# each target as its own job, so one registry timing out no longer cancels the
# others; publish-prebuilt stays for the local release-prebuilt path.
def cmd_publish_marketplace() -> None:
    resolved = resolve_version()
    publish_to_marketplace(find_vsixes(), resolved["preRelease"])


def cmd_publish_github() -> None:
    resolved = resolve_version()
    publish_github_release(
        resolved["tag"], resolved["version"], resolved["preRelease"], find_vsixes()
    )


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
        f"pushed {tag}; the Publish workflow now builds, smoke-tests, and publishes it."
    )


COMMANDS: dict[str, Callable[[], object]] = {
    "prerelease-identity": cmd_prerelease_identity,
    "compile": cmd_compile,
    "compile-release": cmd_compile_release,
    "quick": cmd_quick,
    "package": cmd_package,
    "package-prebuilt": cmd_package_prebuilt,
    "publish-prebuilt": cmd_publish_prebuilt,
    "publish-marketplace": cmd_publish_marketplace,
    "publish-github": cmd_publish_github,
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
