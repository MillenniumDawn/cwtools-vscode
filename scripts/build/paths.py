from __future__ import annotations

from pathlib import Path

BUILD_DIR = Path(__file__).resolve().parent
REPO_ROOT = BUILD_DIR.parent.parent

ENGINE_ROOT = REPO_ROOT / "engine"
EXTENSION_ROOT = REPO_ROOT / "extension"
EXTENSION_SOURCE_ROOT = EXTENSION_ROOT / "src"
EXTENSION_HOST_ROOT = EXTENSION_SOURCE_ROOT / "host"
EXTENSION_WEBVIEW_ROOT = EXTENSION_SOURCE_ROOT / "webview"
EXTENSION_TEST_ROOT = EXTENSION_ROOT / "test"
EXTENSION_PACKAGE_ROOT = EXTENSION_ROOT / "package"
EXTENSION_DOCS_ROOT = REPO_ROOT / "docs" / "extension"
EXTENSION_DIST_ROOT = REPO_ROOT / "dist" / "extension"
ARTIFACTS_ROOT = REPO_ROOT / "artifacts"
VSIX_ROOT = ARTIFACTS_ROOT / "vsix"
