"""Download and run checksum-pinned Linux x86-64 actionlint and ShellCheck releases."""

from __future__ import annotations

import hashlib
import io
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path
from urllib.request import urlopen

VERSION = "1.7.12"
URL = (
    "https://github.com/rhysd/actionlint/releases/download/"
    f"v{VERSION}/actionlint_{VERSION}_linux_amd64.tar.gz"
)
SHA256 = "8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8"

SHELLCHECK_VERSION = "0.11.0"
SHELLCHECK_URL = (
    "https://github.com/koalaman/shellcheck/releases/download/"
    f"v{SHELLCHECK_VERSION}/shellcheck-v{SHELLCHECK_VERSION}.linux.x86_64.tar.gz"
)
SHELLCHECK_SHA256 = "b7af85e41cc99489dcc21d66c6d5f3685138f06d34651e6d34b42ec6d54fe6f6"
SHELLCHECK_MEMBER = f"shellcheck-v{SHELLCHECK_VERSION}/shellcheck"


def verified_executable(url: str, sha256: str, member_name: str) -> bytes:
    """Return one archive member after checking the whole archive's checksum."""
    with urlopen(url, timeout=60) as response:
        archive = response.read()
    digest = hashlib.sha256(archive).hexdigest()
    if digest != sha256:
        raise RuntimeError(f"{member_name} checksum mismatch: {digest}")
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as tar:
        member = tar.getmember(member_name)
        if not member.isfile():
            raise RuntimeError(f"{member_name} archive member is not a regular file")
        source = tar.extractfile(member)
        if source is None:
            raise RuntimeError(f"{member_name} archive member has no content")
        return source.read()


def install(directory: Path, name: str, binary: bytes) -> Path:
    executable = directory / name
    executable.write_bytes(binary)
    executable.chmod(0o755)
    return executable


def run() -> int:
    """Verify both archives before reading their executables, then check workflows."""
    actionlint = verified_executable(URL, SHA256, "actionlint")
    shellcheck = verified_executable(
        SHELLCHECK_URL, SHELLCHECK_SHA256, SHELLCHECK_MEMBER
    )
    with tempfile.TemporaryDirectory(prefix="cwtools-actionlint-") as name:
        directory = Path(name)
        executable = install(directory, "actionlint", actionlint)
        checker = install(directory, "shellcheck", shellcheck)
        # Pyflakes is off so the result never depends on what the runner installed.
        command = [str(executable), f"-shellcheck={checker}", "-pyflakes="]
        result = subprocess.run(command, check=False).returncode
        if result:
            return result
        return check_fixtures(executable, directory)


def check_fixtures(executable: Path, directory: Path) -> int:
    """Check that representative semantic failures are actually detected."""
    fixtures = {
        "expression": (
            "    if: ${{ github.no_such_property }}\n"
            "    steps:\n      - run: echo ok\n",
            "no_such_property",
        ),
        "dependency": (
            "    needs: missing_job\n    steps:\n      - run: echo ok\n",
            "missing_job",
        ),
        "input": (
            "    steps:\n      - uses: actions/checkout@v4\n"
            "        with:\n          invalid_fixture_input: true\n",
            "invalid_fixture_input",
        ),
    }
    for name, (body, expected) in fixtures.items():
        fixture = directory / f"{name}.yml"
        fixture.write_text(
            "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n" + body
        )
        result = subprocess.run(
            [str(executable), "-no-color", str(fixture)],
            check=False,
            capture_output=True,
            text=True,
        )
        output = result.stdout + result.stderr
        if result.returncode == 0 or expected not in output:
            print(
                f"actionlint fixture {name} did not produce its expected diagnostic:"
                f"\n{output}",
                file=sys.stderr,
            )
            return 1
        print(f"actionlint fixture {name}: detected")
    return 0


def main() -> int:
    try:
        return run()
    except (OSError, RuntimeError, tarfile.TarError, KeyError) as error:
        print(f"actionlint: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
