# Contributing

## Layout

This repo holds both halves of CWTools. `extension/` contains the TypeScript host, webview, tests, and checked-in VSIX package inputs. `engine/` contains the Rust workspace that builds the standalone CLI and language server. Product documentation is under `docs/`, and generated extension files go under `dist/`.

## Prerequisites

- Node 26
- Rust (stable) with cargo, for the server
- Python 3.12 or newer, for the helpers under `scripts/`

## Getting the source

```bash
git clone https://github.com/MillenniumDawn/cwtools-vscode
```

## Building

```bash
npm install        # client dependencies
./build.sh quick   # Unix; build.cmd on Windows
```

`quick` builds the Rust server, assembles the client, and leaves a runnable extension in `dist/extension/`.

The client is bundled with esbuild (`scripts/build/esbuild.py`): `tsc` type-checks and emits the per-file output the tests run against, then esbuild produces the two shipped bundles (`extension.js`, `webview/graph.js`). `npm run compile` does both; `npm run check` runs the typecheck and lint.

The Rust server builds from the in-repo `engine` workspace. To build from another checkout, point it there:

```bash
CWTOOLS_RUST_WORKSPACE=../some-other-cwtools/engine ./build.sh quick
```

Other commands: `package` packages a vsix without publishing, `package-prebuilt` packages the binaries already staged by CI (one vsix per platform plus a universal fallback), `publish-prebuilt` publishes what `package-prebuilt` produced, and `release-prebuilt` does both.

`release` cuts a release: it checks the CHANGELOG has a section for the top version, refuses a dirty tree or an existing tag, then pushes `v<x.y.z>`. Everything after that is CI. The tag push triggers `.github/workflows/release.yml`, which builds the server on every platform, packages one vsix per platform plus the universal fallback, smoke-tests them, and publishes. Nothing is built or published from your machine.

## Syntax highlighting

The TextMate grammars under `extension/package/syntaxes/` are vendored from
[cwtools/paradox-syntax](https://github.com/cwtools/paradox-syntax) and
re-synced with [`scripts/sync_paradox_syntax.py`](../scripts/sync_paradox_syntax.py).
The script expects an upstream checkout as a sibling (`../paradox-syntax`);
override with `PARADOX_SYNTAX_SRC=...` if you keep it elsewhere. After
syncing, eyeball the diff before committing: the grammars are mostly
mechanical, but scope names sometimes change.

Themes under `extension/package/themes/` are owned in this repo. Highlighting runs off a
single merged `paradox.tmLanguage.json` with each game's keywords folded in, so
re-vendoring means merging the upstream per-game grammars into it rather than
copying them across.

## Rules pins

Each game in `extension/src/host/games.ts` names its rules repo and a bundled
fallback commit (`repoRef`). `rules-pins.json` is the reviewed runtime manifest:
it carries the revision and the exact commit for every supported game. Nothing
tracks a branch. A fresh cache is a `git init` plus a shallow fetch of that one
commit, and a cache already holding the selected pin is left alone.

On activation, the extension checks the reviewed manifest in the background
and uses the newest valid result. The manifest can change only a known game's
full commit SHA, never a repo URL or a branch. A failed or invalid refresh
keeps the cached manifest (or the bundled fallback), so an upstream push cannot
reach users on its own and an offline activation cannot move a cache backwards.

Refresh the pins with:

```bash
python3 scripts/build/rules_pins.py
```

It reads each repo's default branch head, rewrites both pin sets, and increments
the manifest revision. [`rules-pins.yml`](../.github/workflows/rules-pins.yml)
runs the same script weekly and opens a PR with a compare link for everything
that moved. Read those diffs before merging: the commits in `rules-pins.json`
are what installed extensions fetch on their next activation. Nothing
auto-merges here.

## Running and debugging

Open the repo in VS Code and launch **Quick update, Build and Launch Extension** (or the Debug variant) from the Run panel. That builds and opens an Extension Development Host with the extension loaded. Point it at a mod folder under a game directory to see validation.

## Tests

```bash
npm run test:node      # node-only unit tests for the pure modules (vitest, fast)
npm test               # unit label: VS Code API, no language server
npm run test:smoke     # unit plus activation against the real server
npm run test:host      # everything, including hover and completion
npm run test:coverage  # unit label with validated V8 coverage
npm run test:node:coverage  # vitest coverage into coverage-node/
npm run test:native    # unit label in a visible window, on purpose
npm run bench:node     # client hot-path benchmarks
pytest                 # the Python helpers under scripts/
```

Two layers. `test:node` runs under vitest with no Electron and owns the pure
modules (`engine.ts`, `executable.ts`, `games.ts`, the signature/settings
helpers, the manifest and nls guards). The rest run in a real extension host
against the sample mod in `extension/test/workspaces/stellaris/`, picked by label from
`.vscode-test.mjs`. Helper scripts under `scripts/` (including `scripts/build/`)
are tested from `tests/scripts/`.

### The Python helpers

`scripts/` and its tests have their own toolchain: ruff, black, pylint, mypy
and pytest, all configured in [`pyproject.toml`](../pyproject.toml) and pinned
in [`requirements-dev.txt`](../requirements-dev.txt). Install them into one
environment so mypy can see pytest's types:

```bash
python3 -m pip install -r requirements-dev.txt
ruff check scripts tests/scripts
black --check scripts tests/scripts
pylint scripts tests/scripts
mypy scripts tests/scripts
pytest
```

The `Python lint & tests` CI job runs exactly that list, and the pre-commit
hooks in [`.pre-commit-config.yaml`](../.pre-commit-config.yaml) run it on the
files you touch. mypy is in strict mode and pylint has to stay at 10.00.

`scripts/build/*.py` import each other by bare name, so pytest puts that
directory on `sys.path` (`pythonpath` in `pyproject.toml`) and pylint resolves
it through `source-roots`. The standalone entry points in `scripts/` have no
sibling imports and load by path from `tests/scripts/conftest.py`, which is
also what keeps `scripts/coverage.py` from shadowing the `coverage` package.

### The extension-host window

Every host label runs through `scripts/build/hosttest.py`, which picks a display
backend before it hands off to `vscode-test`. The point is that running the
tests should not throw a VS Code window onto the desktop you are working on.

On Linux the backend is `xvfb-run -a`. If `xvfb-run` is not installed the run
fails and tells you how to install it; it does not quietly fall back to a
visible window. macOS and Windows have no equivalent (Electron has no supported
headless desktop mode, and Xvfb is Linux-only), so they run natively and print a
notice saying so. Issue #406 tracks closing that gap, most likely with a
container.

`CWTOOLS_TEST_DISPLAY` overrides the choice:

| value    | effect |
| -------- | ------ |
| `xvfb`   | force `xvfb-run -a`, and fail if it is missing |
| `ozone`  | pass `--ozone-platform=headless` to Electron. No system package, but the flag is an undocumented Chromium detail, so it is opt-in |
| `native` | a real window, same as `npm run test:native` |

`npm run test:native` is the documented escape hatch for debugging something
you can only see, and it takes the same arguments as the other commands
(`npm run test:native -- --label host`). Unrecognized arguments pass straight
through to `vscode-test`, so `--grep` works everywhere.

CI runs the same commands with no wrapper of its own: the runner resolves xvfb
on the Linux runners. A green Linux run is not evidence about native Windows or
macOS behavior, and the host suites are not currently run on either.

The Rust side has its own suite and its own gates:

```bash
cd engine
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

Anything that touches the parser, the rule engine, a validator or the ruleset
types also runs the diagnostics guards from the repo root
(`python3 scripts/guard.py md` and `python3 scripts/guard.py vanilla`). They
validate the pinned Millennium Dawn mod (and a synthetic vanilla fixture) and
diff the report against a committed baseline, so a change meant to leave
diagnostics alone has to prove it. See
[the engine contributor guide](engine/CONTRIBUTING.md) for the flags, the
pinned input revisions and when re-blessing a baseline is appropriate.

CI gates on `test:node`, `test`, `test:smoke` and `test:host` in
[`.github/workflows/ci.yml`](../.github/workflows/ci.yml), along with the
engine suite, cargo-deny, and the diagnostics guards. The sample
workspace detects as `stellaris` (its `common/species_classes` content marker),
so the hover and completion suites fetch real rules on activation and run in CI
like everything else.

`test:coverage` uses c8 (V8 coverage) to write an HTML report to `coverage/`. Open `coverage/index.html` for line-by-line browsing, or point the [Coverage Gutters](https://marketplace.visualstudio.com/items?itemName=ryanluker.vscode-coverage-gutters) extension at `coverage/lcov.info` to see it inline. The command removes the previous host report first and fails if the `unit` label produces no host/common source coverage or a zero statement, branch, function, or line total. Its rendered summary names the measured label and counts only `extension/src/host` and `extension/src/common`; bundled dependencies and modules measured only by Vitest are filtered out. After mocha finishes, the instrumented host exits so V8 can flush; a leftover process tree is killed if Electron hangs. CI renders rust, host, and node coverage as one compact overview in the job summary and sticky PR comment, with per-file tables in collapsed sections. The raw reports are the `rust-coverage`, `coverage-html`, and `coverage-node` artifacts. It's all local/OSS, no external service. Coverage is informational, not a merge gate (see issue #7).
