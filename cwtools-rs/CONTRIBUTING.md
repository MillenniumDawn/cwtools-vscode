# Contributing

The active codebase is the Rust workspace in `cwtools-rs/`.

## Pre-commit hooks

We use [pre-commit](https://pre-commit.com) to run the same checks CI does, before
they leave your machine. Config is `.pre-commit-config.yaml` at the repo root.

Install once per clone:

```sh
pipx install pre-commit          # or: pip install --user pre-commit
pre-commit install --hook-type pre-commit --hook-type pre-push
```

What runs:

- **on commit**: `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- **on push**: `cargo test --workspace --all-features --no-fail-fast`

fmt and clippy keep commits fast; the full test suite gates the push. All three
mirror `.github/workflows/test.yml`, so a green local run means a green CI lint/test.
Each is filed under `cwtools-rs/**.rs` (clippy and the tests also `.toml`), so a
docs-only change runs none of them.

Bypass in a pinch with `git commit --no-verify` / `git push --no-verify`, but don't
make a habit of it. CI runs the same checks plus `cargo machete` and `cargo deny`.

## Running checks by hand

From `cwtools-rs/`:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

Coverage is optional while we build history, with an 85% line target. It needs
`cargo install cargo-llvm-cov` (CI pins 0.8.7); the script says so and stops if
it isn't there:

```sh
COVERAGE_THRESHOLD=85 bash ../scripts/coverage.sh
```

The command writes `target/coverage/lcov.info`, which is the source for CI and
local review diffs.

## Corpus guard

The test suite says the code still compiles and behaves. The corpus guard says
the *diagnostics* didn't move. Anything that touches the parser, the rule engine
or a validator should run it, because a refactor that was supposed to change
nothing is easy to believe and hard to prove.

It validates a pinned real mod and diffs the report against a committed
baseline (`scripts/corpus-baseline.csv`, 4718 diagnostics as of writing). Run it
from the repo root, one level up from here:

```sh
./scripts/corpus-guard.sh
```

Exit 0 means the report matched. Exit 1 prints what moved: row counts, a
per-code gone/new tally, and the first 40 lines of the diff, with the full diff
written to a temp dir. Exit 2 means the run never happened (missing corpus,
missing binary, validator crashed).

Two inputs, both git checkouts, looked for side by side under
`~/Documents/github-projects` (point `CWTOOLS_PROJECTS` at wherever you keep
them):

- corpus: [Kaiserreich-4-Development](https://github.com/Kaiserreich/Kaiserreich-4-Development)
- rules: [cwtools-hoi4-config](https://github.com/cwtools/cwtools-hoi4-config), the `Config` directory

Override either on its own with `--corpus` / `--rules` or `CWTOOLS_CORPUS` /
`CWTOOLS_RULES`; `--help` lists the rest. The revisions the baseline was taken
against are recorded in its `#` header, and the script prints the revisions it
actually ran on, so an input that has moved on is visible before you go hunting
through the diff.

No `--vanilla`. It would need a Steam install of HOI4, which puts the guard out
of reach of CI and of anyone else's machine, and the point here is a
reproducible diff rather than vanilla coverage.

### Vanilla tier

The cost of that is a blind spot. CW113, CW222, CW227, CW229, CW250 and CW500
compare script against the union of the mod's definitions and the base game's,
so without a base game they report nothing at all and the corpus baseline never
covers them. `cwtools validate` now says so on stderr, and in the `github` and
`sarif` reports, but a silent check is still a check nothing guards.

The second tier fills that in:

```sh
./scripts/vanilla-guard.sh
```

Same script underneath, same flags, same exit codes, against a synthetic base
game, mod and ruleset committed under `scripts/vanilla-fixture/` and a baseline
of its own (`scripts/vanilla-baseline.csv`, 5 diagnostics). No game install, so
it runs anywhere. The fixture is deliberately small: one reference per family
that resolves and one that doesn't, so a change that stops a check reporting and
a change that makes it report everything both move the baseline. Re-bless it the
same way, `./scripts/vanilla-guard.sh --bless`, and say why in the commit
message.

Adding a family to the fixture is the way to keep it honest as more checks go
behind the base-game gate. Run it alongside the corpus guard for anything that
touches those six codes.

### Millennium Dawn tier

One real mod is one mod's worth of coverage, and the two big HOI4 mods do not
write the same script. The third tier validates
[Millennium Dawn](https://github.com/MillenniumDawn/Millennium-Dawn) against a
baseline of its own (`scripts/md-baseline.csv`, 11563 diagnostics as of
writing):

```sh
./scripts/md-guard.sh
```

Same script underneath, same flags, same exit codes, same ruleset. It expects
`Millennium-Dawn` beside the other checkouts under `CWTOOLS_PROJECTS`, and it
passes a `--corpus` of its own rather than reading `CWTOOLS_CORPUS`, since an
exported one is almost always pointing at Kaiserreich and would have this tier
validate that mod against the wrong baseline. Pass `--corpus` yourself to move
it.

The overlap between the two corpora is smaller than a second mod sounds like it
would be. Kaiserreich is the only side reporting CW122, CW248, CW251 and CW280,
Millennium Dawn the only side reporting CW105, CW255, CW262 and CW268, and the
codes they share come out in nothing like the same proportions: 4651 CW272 in
MD against 78 in Kaiserreich, 2725 CW223 in Kaiserreich against 1 in MD. A
change that moves one baseline and leaves the other alone has usually found
something. Run both.

Each baseline records its own corpus and rules revisions and is re-blessed on
its own, so the two rules pins can sit on different commits. CI reads each
tier's revisions out of the header of the baseline that tier checks against.

When a change is *meant* to move diagnostics, re-bless the baselines it moved
in the same commit and say in the message which codes moved and why:

```sh
./scripts/corpus-guard.sh --bless
./scripts/md-guard.sh --bless
git add scripts/corpus-baseline.csv scripts/md-baseline.csv
```

A re-bless that isn't explained in the commit message is indistinguishable from
a regression someone papered over.

The report's `hash` column is dropped on the way into the baseline. That digest
is FNV over `file|code|message|source-line`, and `file` is the absolute path the
validator was handed, so the same diagnostic hashes differently out of two
checkouts. The columns it summarizes are all still in the baseline.

## Fuzzing

Both parsers eat files off the Steam Workshop, which means the input is whatever
someone else's tooling produced. Two targets live in `fuzz/`:

- `parse_string` over `cwtools_parser`, the script parser.
- `parse_loc_text` over `cwtools_localization`. This one reaches further than
  it looks: `parse_loc_text` calls `parse_entry`, which calls
  `parse_loc_elements` on every value, so the `$ref$` and `[...]` Jomini
  command parser is covered too.

`cargo-fuzz` needs nightly. One-time setup:

```sh
rustup toolchain install nightly
cargo +nightly install cargo-fuzz --locked
```

Then, from `cwtools-rs/`:

```sh
mkdir -p fuzz/corpus/parse_string fuzz/corpus/parse_loc_text
cargo +nightly fuzz run parse_string   fuzz/corpus/parse_string   fuzz/seeds/parse_string
cargo +nightly fuzz run parse_loc_text fuzz/corpus/parse_loc_text fuzz/seeds/parse_loc_text
```

The `mkdir` is once per clone. `fuzz/corpus/` is gitignored, so it isn't there
after a fresh checkout, and libFuzzer refuses to start on a corpus directory
that doesn't exist rather than creating one.

The first directory is the working corpus libFuzzer grows, the second is the
committed seeds. Order matters: new inputs are only written to the first one,
which is what keeps them out of `seeds/`. Ctrl-C when you're bored, or bound it
with `-- -max_total_time=300`.

A crash writes the input to `fuzz/artifacts/<target>/` and prints the path.
Replay it with `cargo +nightly fuzz run <target> <that-file>`.

### Seeds

`fuzz/seeds/` is committed and holds real `.txt`, `.cwt` and `.yml` files
pulled from `testfiles/`, plus two regression seeds for crashes that already
happened:

- `parse_string/regression_deep_nesting_300.txt`: clause nesting 300 deep,
  past the 256 `MAX_CLAUSE_DEPTH` cap. Unbounded recursion here used to blow
  the stack, which aborts the process rather than returning an error.
- `parse_loc_text/regression_jomini_lone_quote.yml`: `[GetName(')]`. A single
  unpaired `'` in a Jomini param produced a reversed slice range and panicked.

Both are fixed. The seeds are there so they stay fixed. Add a seed whenever a
fuzz run finds something, in the same commit as the fix.

### In CI

The `fuzz-smoke` job replays the seed corpus on every PR with `-runs=0`, which
executes each seed once and exits. It takes seconds and is deterministic.

It is not a fuzzing campaign, deliberately. A long run finds new inputs, which
means it fails on PRs that had nothing to do with the bug, and the failure isn't
reproducible from the PR alone. Run the campaigns by hand, or on a schedule off
the PR path, and commit what they find as seeds.
