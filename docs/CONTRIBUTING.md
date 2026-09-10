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

Other commands: `package` packages a vsix without publishing, `package-prebuilt` packages the binaries already staged by CI (one vsix per platform plus a universal fallback), `publish-prebuilt` publishes what `package-prebuilt` produced, `publish-marketplace` and `publish-github` are its two halves (CI runs each as its own job so one registry cannot cancel the other; the Marketplace half uploads one vsix per `vsce publish` call with `--skip-duplicate` and retries, because a batched upload of every platform times out on the gallery often enough to matter), and `release-prebuilt` does package plus publish.

### The two channels

Publishing is automatic on both channels; neither needs anything run from your machine. It is one workflow, `.github/workflows/publish.yml`, with one run per push to `main` (and on a `v*` tag). Its `check` job decides the channel once, `verify` fails the run before the 45-minute matrix if a release is missing a publish token, and then three jobs publish whatever `check` picked, in parallel: `Publish: VS Code Marketplace`, `Publish: Open VSX`, `Publish: GitHub`.

That fan-out is the point. They used to be sequential steps in one job, so a Marketplace timeout aborted the job before Open VSX was reached and the extension never appeared there at all. Now one registry failing leaves the others alone, every target publishes the same smoke-tested bytes from a single `package` job, and re-running one red job re-publishes one target (`--skip-duplicate` on both registries, and a GitHub release that already exists is skipped rather than recreated, so a re-run never destroys the first attempt's release).

Only those three jobs serialize, each on a concurrency group keyed on the channel (`publish-marketplace-release`, `publish-marketplace-pre-release`, and so on). The build and package jobs of two runs may overlap; they only produce artifacts. It used to be one workflow-level group, and GitHub keeps one running plus one *pending* run per group and cancels the older pending one — so with pushes to `main` landing minutes apart, a release run queued behind a pre-release build was cancelled by the next push, silently, since `release-failed` ignores cancels on purpose. Per-channel groups mean a release never shares a queue with a pre-release; pre-release publishes still supersede one another, which is what you want from them.

**Pre-release**, on every push to `main` that is not a release commit. Versions follow VS Code's convention: stable takes the even minors, pre-release the odd minor directly above. With stable at `3.4.0`, pre-releases are `3.5.<run number>` — the extension version stays numeric because VS Code rejects SemVer prerelease suffixes, and only the Git tag carries the `-pre.<attempt>` suffix so reruns stay unique. `prerelease_identity_from` in [`scripts/build/build.py`](../scripts/build/build.py) is where that lives; it refuses to run when the stable minor is odd.

**Release**, by merging the release PR. `.github/workflows/release-pr.yml` keeps a `release/version-bump` branch and PR up to date on every push to `main`: [`scripts/build/release_pr.py`](../scripts/build/release_pr.py) promotes the changelog's `### Unreleased` section to a version heading and moves `extension/package/package.json` to match. The bump defaults to `patch`; dispatch the workflow with `release_type: minor|major` for anything else. A minor bump skips the odd line (`3.4.0` → `3.6.0`) so releases stay on even minors.

That branch is regenerated from `origin/main` and force-pushed on every push, so edits made on it are discarded — correct the release notes in `main`'s `### Unreleased` section instead.

Two runs of it can race the merge button. The run for the push *before* a release merge checks out a `main` that still has the Unreleased bullets; by the time it looks for an open PR, the release PR has merged, so it used to open a second "Release x.y.z" for a release that was already out. Two guards close that: the push step re-fetches `origin/main` and yields when it moved since checkout (the newer run is queued behind it on the same concurrency group and takes over), and the "nothing to release" path — the one the release merge itself lands on — closes whatever PR is still open on `release/version-bump` and deletes the branch.

The PR is opened by `cwtools-release-bot`, an org-owned GitHub App, so no individual is its author and it still gets real checks on the merge commit — an App installation token triggers workflows where `GITHUB_TOKEN` does not. It needs `Contents` and `Pull requests` write on this repo, plus the `RELEASE_PR_APP_ID` and `RELEASE_PR_APP_PRIVATE_KEY` secrets; without them the job fails at the token step rather than falling back to anything. `publish.yml` uses the same App to open a fix PR when a release fails.

Merging the release PR is what cuts the release: `check` sees the manifest version matching the top changelog heading with no tag for it yet, and the run publishes the release instead of a pre-release. The tag is created by `Publish: GitHub`, not before it — so a tag means *published*, and a release that failed to publish is simply retried by the next push to `main`.

Be clear about what that retry is: it builds the commit that *retried*, not the release commit. Any PRs merged between the failed release and the retry ship under the release's version and notes. That is the trade for never needing a hand-run step, and it only comes up after a genuine publish failure — when it does, the fix PR's "bump the version if the fix changes shipped code" rule is the way out.

When a release-channel job fails, `release-failed` opens a draft `fix/release-v<x.y.z>` pull request off the release commit, listing which jobs went red with links to their logs. Merging it re-runs the release. Two things it will tell you, and both are worth knowing in advance:

* If `Publish: GitHub` succeeded and only a registry failed, the tag already exists, so `check` will *not* retry. Re-run that one job, or dispatch `publish.yml` against the tag (`gh workflow run publish.yml --ref v<x.y.z>`): it rebuilds from the tag and every target publishes with skip-duplicate, so the ones that already went out are left alone.
* If the fix changes shipped code, bump the version rather than reusing it. Publishes are idempotent, so a registry that already accepted the version silently keeps the broken upload.

A `check` job that itself errors skips everything downstream, `release-failed` included: the run is red on the Actions tab and no pull request appears.

`npm run build -- release` still works as the manual fallback: it checks the CHANGELOG has a section for the top version, refuses a dirty tree or an existing tag, then pushes `v<x.y.z>`, which `publish.yml` picks up through its tag trigger.

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
npm run test:rules-sync  # network-free rules sync host label
npm run test:coverage  # host label with validated V8 coverage
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

CI gates on `test:node`, `test`, `test:smoke`, `test:host` and
`test:rules-sync` in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml),
along with the engine suite, cargo-deny, and the diagnostics guards. The sample
workspace detects as `stellaris` (its `common/species_classes` content marker),
so the hover and completion suites fetch real rules on activation and run in CI
like everything else.

`test:coverage` uses c8 (V8 coverage) to write an HTML report to `coverage/`. Open `coverage/index.html` for line-by-line browsing, or point the [Coverage Gutters](https://marketplace.visualstudio.com/items?itemName=ryanluker.vscode-coverage-gutters) extension at `coverage/lcov.info` to see it inline. The command removes the previous host report first, runs the `host` label (its file list is a superset of `unit`'s and `smoke`'s, and the run needs a built `cwtools-server` binary and network access, same as `npm run test:host`), and fails if that run produces no host/common source coverage or a zero statement, branch, function, or line total. Its rendered summary names the measured label and counts only `extension/src/host` and `extension/src/common`; bundled dependencies and modules measured only by Vitest are filtered out. After mocha finishes, the instrumented host exits so V8 can flush; a leftover process tree is killed if Electron hangs. CI renders rust, host, and node coverage as one compact overview in the job summary and sticky PR comment, with per-file tables in collapsed sections. The raw reports are the `rust-coverage`, `coverage-html`, and `coverage-node` artifacts. It's all local/OSS, no external service. No percentage threshold gates a merge (see issue #7), but CI's `Host coverage` step runs `test:coverage` with no continue-on-error, so that sanity check failing fails the build.
