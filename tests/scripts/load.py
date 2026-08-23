from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
from types import ModuleType

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_DIR = REPO_ROOT / "scripts"
BUILD_DIR = SCRIPTS_DIR / "build"


def _ensure_path(directory: Path) -> None:
    location = str(directory)
    if location not in sys.path:
        sys.path.insert(0, location)


def load_script(name: str) -> ModuleType:
    _ensure_path(SCRIPTS_DIR)
    path = SCRIPTS_DIR / f"{name}.py"
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise ImportError(name)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def load_build(name: str) -> ModuleType:
    _ensure_path(BUILD_DIR)
    if name in sys.modules and getattr(sys.modules[name], "__file__", "") == str(
        BUILD_DIR / f"{name}.py"
    ):
        return sys.modules[name]
    path = BUILD_DIR / f"{name}.py"
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise ImportError(name)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module
