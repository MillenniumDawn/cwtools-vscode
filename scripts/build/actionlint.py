"""Download and run the checksum-pinned Linux x86-64 actionlint release."""

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


def run() -> int:
    """Verify the archive before reading its executable, then check workflows."""
    with urlopen(URL, timeout=60) as response:
        archive = response.read()
    digest = hashlib.sha256(archive).hexdigest()
    if digest != SHA256:
        raise RuntimeError(f"actionlint checksum mismatch: {digest}")
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as tar:
        member = tar.getmember("actionlint")
        if not member.isfile():
            raise RuntimeError("actionlint archive member is not a regular file")
        source = tar.extractfile(member)
        if source is None:
            raise RuntimeError("actionlint archive member has no content")
        binary = source.read()
    with tempfile.TemporaryDirectory(prefix="cwtools-actionlint-") as directory:
        executable = Path(directory) / "actionlint"
        executable.write_bytes(binary)
        executable.chmod(0o755)
        result = subprocess.run([str(executable)], check=False).returncode
        if result:
            return result
        return check_fixtures(executable, Path(directory))


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
