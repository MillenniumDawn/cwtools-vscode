# Contributing

The active codebase is the Rust workspace in `engine/`.

## Local checks

`.pre-commit-config.yaml` at the repo root installs format, lint, and test
hooks for every toolchain. The fixers rewrite the files you stage and the
commit picks up the result; the gates still fail the commit on anything they
can't fix. Commands mirror CI so local failures match it.

- Rust (`engine/`): `cargo fmt` formats the workspace in place (so committed
  code is always rustfmt-clean) and `cargo clippy -D warnings` gates every
  commit; `cargo test --workspace` gates every push (see
  `.github/workflows/ci.yml`).
- TypeScript (`extension/`): `eslint --fix` applies autofixes on
  commit (the same rules as `npm run lint`, `eslint .`).
- Python (`scripts/`, `tests/scripts/`): `ruff check --fix` and `black` reformat/fix on commit;
  `pylint` and `mypy` gate every commit; `pytest` gates every push. All five
  read `pyproject.toml`.

One-time setup:

```sh
pipx install pre-commit          # or: pip install --user pre-commit
pre-commit install --hook-type pre-commit --hook-type pre-push
# The Python checks must be on PATH, in one environment so mypy can see
# pytest's types (the Rust and TypeScript ones come from the repo's own
# toolchains):
python3 -m pip install -r requirements-dev.txt
npm install                      # provides eslint via node_modules/.bin
```

The hooks fire only on the files they cover, so a docs-only change runs nothing.
Because the fixers edit your staged files, a commit that triggers one will need
to be run again so the fixed version is the one committed. You can still run the
same commands by hand (`cargo fmt --all` from `engine/`, `npx --no-install
eslint`, `ruff check --fix scripts`, ...) when you want a faster loop. Bypass
the hooks in a pinch with `git commit --no-verify` (use sparingly).

## Running checks by hand

From `engine/`:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

Coverage is optional while we build history, with an 85% line target. It needs
`cargo install cargo-llvm-cov` (CI pins 0.8.7); the script says so and stops if
it isn't there:

```sh
COVERAGE_THRESHOLD=85 python3 ../scripts/coverage.py
```

The command writes `target/coverage/lcov.info`, which is the source for CI and
local review diffs.

## Diagnostics guard

The test suite says the code still compiles and behaves. The diagnostics guard
says the *diagnostics* didn't move. Anything that touches the parser, the rule
engine or a validator should run it, because a refactor that was supposed to
change nothing is easy to believe and hard to prove.

It validates the pinned Millennium Dawn mod and diffs the report against a
committed baseline (`scripts/md-baseline.csv`, 9208 diagnostics as of writing).
Run it from the repo root:

```sh
python3 scripts/guard.py md
```

Exit 0 means the report matched. Exit 1 prints what moved: row counts, a
per-code gone/new tally, and the first 40 lines of the diff, with the full diff
written to a temp dir. Exit 2 means the run never happened (missing corpus,
missing binary, validator crashed).

Two inputs, both git checkouts, looked for side by side under
`~/Documents/github-projects` (point `CWTOOLS_PROJECTS` at wherever you keep
them):

- corpus: [Millennium-Dawn](https://github.com/MillenniumDawn/Millennium-Dawn)
- rules: [cwtools-hoi4-config](https://github.com/cwtools/cwtools-hoi4-config), the `Config` directory

Override either on its own with `--corpus` / `--rules` or `CWTOOLS_CORPUS` /
`CWTOOLS_RULES`; `--help` lists the rest. The revisions the baseline was taken
against are recorded in its `#` header, and the script prints the revisions it
actually ran on, so an input that has moved on is visible before you go hunting
through the diff.

### Vanilla tier

CW113, CW222, CW227, CW229, CW250 and CW500 compare script against the union of
the mod's definitions and the base game's, so without a base game they report
nothing at all and the md baseline never covers them. `cwtools validate` says so
on stderr, and in the `github` and `sarif` reports, but a silent check is still
a check nothing guards.

The second tier fills that in:

```sh
python3 scripts/guard.py vanilla
```

Same script underneath, same flags, same exit codes, against a synthetic base
game, mod and ruleset committed under `scripts/vanilla-fixture/` and a baseline
of its own (`scripts/vanilla-baseline.csv`, 5 diagnostics). No game install, so
it runs anywhere. The fixture is deliberately small: one reference per family
that resolves and one that doesn't, so a change that stops a check reporting and
a change that makes it report everything both move the baseline. Re-bless it the
same way, `python3 scripts/guard.py vanilla --bless`, and say why in the commit
message.

Adding a family to the fixture is the way to keep it honest as more checks go
behind the base-game gate. Run it alongside the md guard for anything that
touches those six codes.

Each baseline records its own corpus and rules revisions and is re-blessed on
its own. CI reads each tier's revisions out of the header of the baseline that
tier checks against.

When a change is *meant* to move diagnostics, re-bless the baselines it moved
in the same commit and say in the message which codes moved and why:

```sh
python3 scripts/guard.py md --bless
python3 scripts/guard.py vanilla --bless
git add scripts/md-baseline.csv scripts/vanilla-baseline.csv
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

Then, from `engine/`:

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
