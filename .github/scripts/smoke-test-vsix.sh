#!/usr/bin/env bash
#
# Smoke-test the packaged vsix files before they are published.
#
# Usage:
#   smoke-test-vsix.sh <vsix-dir> [expected-platform ...]
#
# <vsix-dir>          Directory holding the packaged .vsix files (temp/).
# expected-platform   Server platform directories the universal vsix must
#                     carry, e.g. linux-x64 win-x64. Optional; when given, a
#                     shrunken build matrix fails here instead of shipping.
#
# Checks, for every vsix found:
#   - the extension manifest parses
#   - the extension entrypoint is present
#   - a platform-specific vsix carries exactly its own server binary
#   - the universal vsix carries every platform the others do
set -euo pipefail

vsix_dir="${1:?Usage: $0 <vsix-dir> [expected-platform ...]}"
shift || true
expected_platforms=("$@")

# vsce names a targeted package <name>-<target>-<version>.vsix. Map those
# target ids back to the server binary directory names we stage.
target_to_platform() {
    case "$1" in
        win32-x64) echo win-x64 ;;
        linux-x64) echo linux-x64 ;;
        linux-arm64) echo linux-arm64 ;;
        darwin-x64) echo osx-x64 ;;
        darwin-arm64) echo osx-arm64 ;;
        *) echo "" ;;
    esac
}

shopt -s nullglob
vsixes=("$vsix_dir"/*.vsix)
if [ ${#vsixes[@]} -eq 0 ]; then
    echo "::error::No .vsix file found in $vsix_dir"
    exit 1
fi

universal_platforms=""
targeted_platforms=""

for vsix in "${vsixes[@]}"; do
    echo "Checking $(basename "$vsix") ..."
    workdir=$(mktemp -d)
    # VSIX archives nest all extension content under extension/.
    unzip -q "$vsix" -d "$workdir"
    root="$workdir/extension"

    if ! python3 -m json.tool "$root/package.json" > /dev/null 2>&1; then
        echo "::error::$(basename "$vsix"): extension package.json is not valid JSON"
        exit 1
    fi
    if [ ! -f "$root/bin/client/extension/extension.js" ]; then
        echo "::error::$(basename "$vsix"): missing extension entrypoint bin/client/extension/extension.js"
        exit 1
    fi

    base="$root/bin/server/cwtools-server"
    present=""
    for dir in "$base"/*/; do
        platform=$(basename "$dir")
        count=$(find "$dir" -type f | wc -l)
        if [ "$count" -eq 0 ]; then
            echo "::error::$(basename "$vsix"): no files in server binary directory $platform"
            exit 1
        fi
        present="$present $platform"
    done
    present=$(echo "$present" | tr ' ' '\n' | grep -v '^$' | sort | tr '\n' ' ')
    if [ -z "$present" ]; then
        echo "::error::$(basename "$vsix"): no server binaries at all"
        exit 1
    fi

    # A targeted package is named ...-<target>-<version>.vsix; anything else is
    # the universal fallback.
    target=$(basename "$vsix" | sed -E 's/^.*-((win32|linux|darwin|alpine)-[a-z0-9]+)-[0-9].*\.vsix$/\1/')
    platform=$(target_to_platform "$target")
    if [ -n "$platform" ]; then
        if [ "$present" != "$platform " ]; then
            echo "::error::$(basename "$vsix"): targets $target but carries [$present] instead of just $platform"
            exit 1
        fi
        targeted_platforms="$targeted_platforms $platform"
        echo "  $target: $platform only, OK"
    else
        universal_platforms="$present"
        echo "  universal: [$present] OK"
    fi

    rm -rf "$workdir"
done

if [ -n "$universal_platforms" ]; then
    for platform in $targeted_platforms; do
        case " $universal_platforms " in
            *" $platform "*) ;;
            *)
                echo "::error::universal vsix is missing $platform, which has its own package"
                exit 1
                ;;
        esac
    done
fi

for platform in "${expected_platforms[@]}"; do
    case " $universal_platforms $targeted_platforms " in
        *" $platform "*) ;;
        *)
            echo "::error::expected platform $platform was not packaged"
            exit 1
            ;;
    esac
done

echo "Smoke test passed (${#vsixes[@]} vsix)"
