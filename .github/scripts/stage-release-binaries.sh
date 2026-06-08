#!/usr/bin/env bash
#
# Stage pre-built server binaries into the release directory.
#
# Usage:
#   stage-release-binaries.sh <artifacts-dir> <release-dir>
#
# <artifacts-dir>  Directory containing server-* subdirectories (from
#                  actions/download-artifact with pattern: server-*).
# <release-dir>    The release/ tree whose bin/server/cwtools-server/ will
#                  receive one subdirectory per platform.
#
# The script is platform-agnostic: it picks up every server-* directory it
# finds, so adding or removing a platform only requires changing the CI
# build matrix. No workflow edits needed here.
set -euo pipefail

artifacts_dir="${1:?Usage: $0 <artifacts-dir> <release-dir>}"
release_dir="${2:?Usage: $0 <artifacts-dir> <release-dir>}"
base="$release_dir/bin/server/cwtools-server"

shopt -s nullglob
found=0
for dir in "$artifacts_dir"/server-*/; do
    # Extract platform from directory name: "artifacts/server-linux-x64/" -> "linux-x64"
    platform="${dir#"$artifacts_dir"/server-}"
    platform="${platform%/}"

    echo "Staging $platform ..."
    mkdir -p "$base/$platform"

    for f in "$dir"*; do
        install -D "$f" "$base/$platform/$(basename "$f")"
    done
    found=1
done

if [ "$found" -eq 0 ]; then
    echo "Error: no server-* artifacts found in $artifacts_dir" >&2
    exit 1
fi

# Ensure binaries are executable (Linux/macOS).
chmod +x "$base"/*/* 2>/dev/null || true

echo "Staged platforms:"
ls -R "$base"
