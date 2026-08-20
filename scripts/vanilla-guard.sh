#!/usr/bin/env bash
#
# Vanilla-backed guard tier. The main corpus guard runs without a base game, so
# the check families that can only answer against the mod+base-game union never
# fire there and a change that broke one of them would go unnoticed:
#
#   CW113  a `filepath` reference that resolves to no indexed file
#   CW222  an `<event>` reference with no definition
#   CW227  a ship design's section template
#   CW229  a ship design's component template
#   CW500  any other `<type>` reference with no definition
#
# The inputs are a synthetic base game, mod and ruleset committed under
# vanilla-fixture/, so this tier needs no game install and runs anywhere the
# main guard does. It is small enough to read: one reference per family that
# resolves and one that doesn't.
#
#   scripts/vanilla-guard.sh              check against the baseline
#   scripts/vanilla-guard.sh --bless      rewrite the baseline
#   scripts/vanilla-guard.sh --no-build   skip the release build first
#
# Flags are passed through to corpus-guard.sh, which does the work and owns the
# exit codes: 0 matched, 1 drifted, 2 the run never happened.

set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
fixture=$script_dir/vanilla-fixture

# --game stellaris because CW227/CW229 are emitted by the Stellaris validator.
# The other three families are game-independent.
export CWTOOLS_GUARD_NAME=vanilla-guard.sh
exec "$script_dir/corpus-guard.sh" \
  --game stellaris \
  --corpus "$fixture/mod" \
  --rules "$fixture/rules" \
  --vanilla "$fixture/vanilla" \
  --baseline "${CWTOOLS_BASELINE:-$script_dir/vanilla-baseline.csv}" \
  "$@"
