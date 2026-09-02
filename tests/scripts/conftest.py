from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
from types import ModuleType

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_DIR = REPO_ROOT / "scripts"


# scripts/build/*.py are on pytest's pythonpath and import normally. The
# standalone entry points in scripts/ load by path under a prefixed name, so
# scripts/coverage.py cannot shadow the coverage package. The sys.modules entry
# is what @dataclass needs to resolve its own module's annotations.
def _load(name: str) -> ModuleType:
    qualified = f"cwtools_scripts.{name}"
    path = SCRIPTS_DIR / f"{name}.py"
    spec = importlib.util.spec_from_file_location(qualified, path)
    if spec is None or spec.loader is None:
        raise ImportError(name)
    module = importlib.util.module_from_spec(spec)
    sys.modules[qualified] = module
    spec.loader.exec_module(module)
    return module


@pytest.fixture(scope="session")
def guard() -> ModuleType:
    return _load("guard")


@pytest.fixture(scope="session")
def resolve_pins() -> ModuleType:
    return _load("resolve_pins")


@pytest.fixture(scope="session")
def rust_coverage() -> ModuleType:
    return _load("coverage")


@pytest.fixture(scope="session")
def smoke_test_vsix() -> ModuleType:
    return _load("smoke_test_vsix")


@pytest.fixture(scope="session")
def stage_release_binaries() -> ModuleType:
    return _load("stage_release_binaries")


@pytest.fixture(scope="session")
def sync_paradox_syntax() -> ModuleType:
    return _load("sync_paradox_syntax")


@pytest.fixture(scope="session")
def workspace_manifest() -> ModuleType:
    return _load("workspace_manifest")
