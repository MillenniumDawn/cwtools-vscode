from __future__ import annotations

import shutil


def require_executable(command: str) -> str:
    found = shutil.which(command)
    if found is None:
        raise RuntimeError(f"{command} is not on PATH")
    return found
