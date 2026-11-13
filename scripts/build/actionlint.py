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
        return subprocess.run([str(executable)], check=False).returncode


def main() -> int:
    try:
        return run()
    except (OSError, RuntimeError, tarfile.TarError, KeyError) as error:
        print(f"actionlint: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
