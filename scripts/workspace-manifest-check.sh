#!/usr/bin/env bash
#
# Workspace manifest inheritance gate. Fails when a member crate drifts from
# the posture #157 codified:
#
#   - repository.workspace = true (cargo metadata must not report null)
#   - [lints] workspace = true
#   - tempfile / assert_cmd / predicates only via { workspace = true }
#
# Exit 0 clean, 1 on drift, 2 on setup failure.

set -euo pipefail

export LC_ALL=C

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(dirname -- "$script_dir")
ws_root=${CWTOOLS_RS:-$repo_root/cwtools-rs}
crates_dir=$ws_root/crates

die() {
	echo "workspace-manifest-check: $*" >&2
	exit 2
}
fail() {
	echo "workspace-manifest-check: $*" >&2
	exit 1
}

[ -d "$crates_dir" ] || die "crates dir not found: $crates_dir"
[ -f "$ws_root/Cargo.toml" ] || die "workspace Cargo.toml not found: $ws_root/Cargo.toml"

command -v cargo >/dev/null || die "cargo not on PATH"
command -v jq >/dev/null || die "jq not on PATH"

errors=0
note() {
	echo "workspace-manifest-check: $*" >&2
	errors=$((errors + 1))
}

# Every path member must inherit repository and lints.
shopt -s nullglob
for manifest in "$crates_dir"/*/Cargo.toml; do
	crate=$(basename -- "$(dirname -- "$manifest")")
	if ! grep -q '^repository\.workspace = true$' "$manifest"; then
		note "$crate: missing repository.workspace = true"
	fi
	if ! awk '
    /^\[lints\]$/ { in_lints=1; next }
    /^\[/ { in_lints=0 }
    in_lints && /^workspace = true$/ { found=1 }
    END { exit found ? 0 : 1 }
  ' "$manifest"; then
		note "$crate: missing [lints] workspace = true"
	fi
	# Shared test deps must not be version-pinned at the crate level.
	if grep -E -q '^(tempfile|assert_cmd|predicates)[[:space:]]*=' "$manifest"; then
		if ! grep -E -q '^(tempfile|assert_cmd|predicates) = \{ workspace = true' "$manifest"; then
			note "$crate: tempfile/assert_cmd/predicates must use { workspace = true }"
		fi
	fi
done

# cargo metadata is the source of truth for inherited package fields.
meta=$(cargo metadata --manifest-path "$ws_root/Cargo.toml" --format-version 1 --no-deps)
missing=$(printf '%s\n' "$meta" | jq -r '
  .packages
  | map(select(.source == null and .repository == null) | .name)
  | .[]
')
if [ -n "$missing" ]; then
	while IFS= read -r name; do
		[ -n "$name" ] || continue
		note "cargo metadata: $name has repository=null"
	done <<<"$missing"
fi

# Workspace root must declare the shared test deps and the deny-warnings lints.
root=$ws_root/Cargo.toml
for dep in tempfile assert_cmd predicates; do
	if ! grep -q "^${dep} = " "$root"; then
		note "workspace Cargo.toml missing [workspace.dependencies] $dep"
	fi
done
if ! grep -q '^\[workspace.lints.rust\]$' "$root"; then
	note "workspace Cargo.toml missing [workspace.lints.rust]"
fi
if ! grep -q '^\[workspace.lints.clippy\]$' "$root"; then
	note "workspace Cargo.toml missing [workspace.lints.clippy]"
fi

if [ "$errors" -ne 0 ]; then
	fail "$errors inheritance check(s) failed"
fi

echo "workspace-manifest-check: ok"
