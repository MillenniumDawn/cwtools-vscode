#!/usr/bin/env bash
#
# Millennium Dawn corpus tier. Same engine and same flags as the Kaiserreich
# tier, a different mod, and the two overlap less than a second corpus sounds
# like it would. MD is the only side reporting CW105, CW255, CW262 and CW268,
# Kaiserreich the only side reporting CW122, CW248, CW251 and CW280, and the
# codes they share land in nothing like the same proportions: MD is 4651 CW272
# against Kaiserreich's 78, Kaiserreich 2725 CW223 against MD's 1. A change
# that moves one baseline and leaves the other alone has usually found
# something real.
#
# The corpus is public, so unlike the Kaiserreich tier this one needs no
# credentials and runs on a fork's pull request too.
#
#   scripts/md-guard.sh              check against the baseline
#   scripts/md-guard.sh --bless      rewrite the baseline
#   scripts/md-guard.sh --no-build   skip the release build first
#
# Flags are passed through to corpus-guard.sh, which does the work and owns the
# exit codes: 0 matched, 1 drifted, 2 the run never happened. A --corpus of
# your own still wins, since the last one on the line is the one that sticks.

set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
projects=${CWTOOLS_PROJECTS:-$HOME/Documents/github-projects}

# --corpus rather than CWTOOLS_CORPUS: an exported one is almost always
# pointing at Kaiserreich, and reading it here would validate that mod against
# this baseline and report the whole thing as drift. CWTOOLS_BASELINE is the
# other way round: passing --baseline unconditionally would swallow it, and
# writing the before-baseline somewhere else is exactly what it is for.
export CWTOOLS_GUARD_NAME=md-guard.sh
exec "$script_dir/corpus-guard.sh" \
  --corpus "$projects/Millennium-Dawn" \
  --baseline "${CWTOOLS_BASELINE:-$script_dir/md-baseline.csv}" \
  "$@"
