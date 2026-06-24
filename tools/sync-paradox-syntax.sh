#!/usr/bin/env bash
# Re-vendor the TextMate grammars from cwtools/paradox-syntax.
#
# Pulls the latest syntaxes/ from the upstream tboby.paradox-syntax extension
# and copies them into release/syntaxes/, overwriting the vendored copies
# without touching any of the new themes this repo adds.
#
# Run from the repo root: ./tools/sync-paradox-syntax.sh
#
# The script is intentionally a thin rsync: it does not try to be clever
# about merge conflicts. If a file diverges in this repo, diff before
# committing and resolve by hand.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
src="${PARADOX_SYNTAX_SRC:-../paradox-syntax}"
dst="${repo_root}/release/syntaxes"

if [[ ! -d "${src}/syntaxes" ]]; then
	echo "error: '${src}/syntaxes' not found." >&2
	echo "       Clone the upstream repo next to this one, or set PARADOX_SYNTAX_SRC." >&2
	exit 1
fi

# Preserve the cwt grammar (this repo owns it) and only mirror the paradox grammars.
shopt -s nullglob
copied=()
for f in "${src}/syntaxes/"*.tmLanguage.json; do
	base="$(basename "$f")"
	cp -f "$f" "${dst}/${base}"
	copied+=("$base")
done

echo "Updated ${#copied[@]} grammar file(s) in release/syntaxes/:"
printf '  %s\n' "${copied[@]}"
echo
echo "Reminder: do not add 'tboby.paradox-syntax' to extensionPack again —"
echo "this repo ships its own grammars. Themes live in release/themes/ and"
echo "are owned here, not mirrored."
