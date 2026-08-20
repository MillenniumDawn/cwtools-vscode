#!/usr/bin/env bash
#
# Corpus guard. Validates a pinned mod corpus and diffs the report against a
# committed baseline, so a change that was meant to leave diagnostics alone has
# to prove it.
#
# Inputs are two git checkouts, pinned by SHA in the baseline header:
#
#   corpus  a real HOI4 mod. Kaiserreich-4-Development by default;
#           scripts/md-guard.sh drives this script at Millennium Dawn and a
#           baseline of its own, so a change has two real mods to move rather
#           than one, and they do not report the same codes
#   rules   the .cwt ruleset (cwtools-hoi4-config/Config)
#
# The default run has no vanilla game install. --vanilla needs a Steam copy of
# HOI4, which no CI runner and no second machine can be assumed to have, and
# the guard wants a reproducible diff. The cost is that CW113, CW222, CW227,
# CW229, CW250 and CW500 never fire here: they can only answer against the
# mod+base-game union, so without one they stay silent. scripts/vanilla-guard.sh
# is the second tier that covers them, over a synthetic base game committed
# alongside it, and it drives this script through --vanilla.
#
#   scripts/corpus-guard.sh              check against the baseline
#   scripts/corpus-guard.sh --bless      rewrite the baseline (see CONTRIBUTING)
#   scripts/corpus-guard.sh --help       flags and env vars
#
# Exit codes: 0 baseline matched, 1 diagnostics drifted, 2 the run never
# happened (missing corpus, missing binary, validator crashed).

set -euo pipefail

# Byte order for sort, byte semantics for sed/awk. The baseline is sorted on
# disk, so a locale change must not reorder it.
export LC_ALL=C

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(dirname -- "$script_dir")

# Sibling checkouts of this repo. Override either with the env var.
projects=${CWTOOLS_PROJECTS:-$HOME/Documents/github-projects}
corpus=${CWTOOLS_CORPUS:-$projects/Kaiserreich-4-Development}
rules=${CWTOOLS_RULES:-$projects/cwtools-hoi4-config/Config}
# Base game to index for reference resolution. Empty by default; see the header.
vanilla=${CWTOOLS_VANILLA:-}
baseline=${CWTOOLS_BASELINE:-$script_dir/corpus-baseline.csv}
bin=${CWTOOLS_BIN:-$repo_root/cwtools-rs/target/release/cwtools}
game=${CWTOOLS_GAME:-hoi4}
# Script the baseline header tells the reader to re-bless with. A wrapper that
# drives this one (vanilla-guard.sh) sets it to its own name.
guard_name=${CWTOOLS_GUARD_NAME:-corpus-guard.sh}
bless=0
build=1

usage() {
  cat <<'EOF'
Usage: corpus-guard.sh [options]

  --corpus DIR     mod corpus to validate     (env CWTOOLS_CORPUS)
  --rules DIR      .cwt ruleset directory     (env CWTOOLS_RULES)
  --vanilla DIR    base game to index         (env CWTOOLS_VANILLA)
  --baseline FILE  committed baseline report  (env CWTOOLS_BASELINE)
  --bin PATH       cwtools binary             (env CWTOOLS_BIN)
  --game NAME      game id, default hoi4      (env CWTOOLS_GAME)
  --no-build       use --bin as-is instead of rebuilding it first
  --bless          overwrite the baseline with this run's report
  -h, --help       this text

Exit 0 if the report matches the baseline, 1 if it drifted, 2 on a setup
problem. Flags win over env vars.
EOF
}

while [ $# -gt 0 ]; do
  case $1 in
    --corpus) corpus=${2:?--corpus needs a directory}; shift 2 ;;
    --rules) rules=${2:?--rules needs a directory}; shift 2 ;;
    --vanilla) vanilla=${2:?--vanilla needs a directory}; shift 2 ;;
    --baseline) baseline=${2:?--baseline needs a file}; shift 2 ;;
    --bin) bin=${2:?--bin needs a path}; shift 2 ;;
    --game) game=${2:?--game needs a name}; shift 2 ;;
    --no-build) build=0; shift ;;
    --bless) bless=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "corpus-guard: unknown argument '$1'" >&2; usage >&2; exit 2 ;;
  esac
done

die() { echo "corpus-guard: $*" >&2; exit 2; }

[ -d "$corpus" ] || die "corpus not found: $corpus"
[ -d "$rules" ] || die "rules not found: $rules"
[ -z "$vanilla" ] || [ -d "$vanilla" ] || die "vanilla not found: $vanilla"

if [ "$build" -eq 1 ]; then
  echo "corpus-guard: cargo build --release -p cwtools_cli"
  (cd "$repo_root/cwtools-rs" && cargo build --release -p cwtools_cli) \
    || die "release build failed; nothing to validate with"
fi
[ -x "$bin" ] || die "cwtools binary not found: $bin (build it, or pass --bin)"

# Absolute, symlink-resolved corpus root. The report prints whatever path it
# was handed, and that string is what gets stripped back out below.
corpus=$(CDPATH='' cd -- "$corpus" && pwd -P)
rules=$(CDPATH='' cd -- "$rules" && pwd -P)
[ -z "$vanilla" ] || vanilla=$(CDPATH='' cd -- "$vanilla" && pwd -P)

# Provenance of the inputs. A moved corpus or ruleset is the usual reason a
# diff appears out of nowhere, so it goes in the baseline and in the failure
# report rather than being left for someone to work out.
describe() {
  local dir=$1 sha dirty
  sha=$(git -C "$dir" rev-parse --short HEAD 2>/dev/null) || { echo "not a git checkout"; return; }
  dirty=$(git -C "$dir" status --porcelain 2>/dev/null | head -n 1)
  if [ -n "$dirty" ]; then echo "$sha (dirty)"; else echo "$sha"; fi
}
corpus_rev=$(describe "$corpus")
rules_rev=$(describe "$rules")
[ -z "$vanilla" ] || vanilla_rev=$(describe "$vanilla")

work=$(mktemp -d "${TMPDIR:-/tmp}/corpus-guard.XXXXXX")
keep=0
cleanup() { if [ "$keep" -eq 0 ]; then rm -rf "$work"; else echo "corpus-guard: artifacts in $work"; fi; }
trap cleanup EXIT

raw=$work/report.csv
current=$work/current.csv

echo "corpus-guard: $corpus [$corpus_rev]"
echo "corpus-guard: $rules [$rules_rev]"

# --no-vanilla-cache alongside --vanilla: the auto-managed cache under the OS
# cache dir would make the report depend on what an earlier run left behind.
vanilla_args=()
if [ -n "$vanilla" ]; then
  echo "corpus-guard: $vanilla [$vanilla_rev]"
  vanilla_args=(--vanilla "$vanilla" --no-vanilla-cache)
fi

# Exit 1 means "found errors", which is the normal case here. Anything above
# that (2 config, 3 discovery, 4 empty input, 101 panic) means no usable report.
status=0
"$bin" validate \
  --game "$game" \
  --directory "$corpus" \
  --rules "$rules" \
  "${vanilla_args[@]+"${vanilla_args[@]}"}" \
  --report-type csv \
  --output-file "$raw" >"$work/validate.log" 2>&1 || status=$?
if [ "$status" -gt 1 ]; then
  keep=1
  sed -n '1,40p' "$work/validate.log" >&2
  die "validate exited $status, no report to compare"
fi
[ -s "$raw" ] || { keep=1; die "validate wrote no report to $raw"; }

# Normalize the report into the comparable form:
#
#  - absolute corpus paths become corpus-relative, so the baseline means the
#    same thing in any checkout,
#  - the trailing hash column is dropped. That digest is FNV over
#    file|code|message|line-text, and `file` is the absolute path it was given,
#    so it changes with the checkout location and cannot be compared across
#    machines. The columns it summarizes are all still here,
#  - rows sort bytewise, because the validator walks files in parallel and only
#    happens to emit them in a stable order.
#
# The message field is quoted when it contains a comma, and one Kaiserreich
# path has commas in it too, so nothing here splits on commas.
{
  echo "# cwtools guard baseline. Regenerate with scripts/$guard_name --bless"
  echo "# corpus: $(basename -- "$corpus") @ $corpus_rev"
  echo "# rules:  $(basename -- "$(dirname -- "$rules")")/$(basename -- "$rules") @ $rules_rev"
  [ -z "$vanilla" ] || echo "# vanilla: $(basename -- "$vanilla") @ $vanilla_rev"
  echo "file,line,severity,code,message"
  tail -n +2 -- "$raw" \
    | awk -v root="$corpus/" '{ p = index($0, root); if (p) $0 = substr($0, 1, p - 1) substr($0, p + length(root)); print }' \
    | sed 's/,[0-9a-f]\{16\}$//' \
    | sort
} >"$current"

# Compare bodies only. The `#` header carries the input revisions, which move
# on every bless and would otherwise show up as a diff every time.
body() { awk 'seen || !/^#/ { seen = 1; print }' "$1"; }

if [ "$bless" -eq 1 ]; then
  if [ -f "$baseline" ]; then
    before=$(body "$baseline" | tail -n +2 | wc -l)
  else
    before=0
  fi
  after=$(body "$current" | tail -n +2 | wc -l)
  cp -- "$current" "$baseline"
  echo "corpus-guard: blessed $baseline ($before -> $after diagnostics)"
  exit 0
fi

[ -f "$baseline" ] || die "no baseline at $baseline (create one with --bless)"

body "$baseline" >"$work/baseline.body"
body "$current" >"$work/current.body"

if diff -q "$work/baseline.body" "$work/current.body" >/dev/null; then
  echo "corpus-guard: OK, $(tail -n +2 "$work/current.body" | wc -l | tr -d ' ') diagnostics match the baseline"
  exit 0
fi

keep=1
# -U0: with 4000 sorted rows the context lines are pure noise, and every
# changed row is self-describing anyway.
diff -U0 --label "baseline" --label "current" \
  "$work/baseline.body" "$work/current.body" >"$work/drift.diff" || true

removed=$(grep -c '^-[^-]' "$work/drift.diff" || true)
added=$(grep -c '^+[^+]' "$work/drift.diff" || true)

echo
echo "corpus-guard: FAIL, diagnostics drifted from the baseline"
echo "  baseline $(tail -n +2 "$work/baseline.body" | wc -l | tr -d ' ') diagnostics"
echo "  current  $(tail -n +2 "$work/current.body" | wc -l | tr -d ' ') diagnostics"
echo "  -$removed +$added rows"

# Per-code gone/new counts, so a 400-row diff still says what moved. Counted
# separately rather than netted: a rule whose message text changed keeps its
# count and would otherwise report as no change at all.
echo
echo "  by code (gone/new):"
{
  grep '^-[^-]' "$work/drift.diff" | awk '{ if (match($0, /,CW[0-9][0-9][0-9],/)) print substr($0, RSTART + 1, 5) " 1 0"; else print "(no-code) 1 0" }'
  grep '^+[^+]' "$work/drift.diff" | awk '{ if (match($0, /,CW[0-9][0-9][0-9],/)) print substr($0, RSTART + 1, 5) " 0 1"; else print "(no-code) 0 1" }'
} | awk '{ gone[$1] += $2; new[$1] += $3; seen[$1] = 1 } END { for (c in seen) printf "    %-10s -%d +%d\n", c, gone[c], new[c] }' | sort

echo
echo "  first 40 diff lines:"
sed -n '3,42p' "$work/drift.diff" | sed 's/^/    /'
echo
echo "  full diff:   $work/drift.diff"
echo "  full report: $current"
echo "  if the change is intended, re-bless: scripts/corpus-guard.sh --bless"
exit 1
