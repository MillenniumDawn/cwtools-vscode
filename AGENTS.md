# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository. `AGENTS.md` and `CLAUDE.md` are the same document; change one and copy it to the other.

## Project Overview

CWTools MD Edition is the Millennium Dawn fork of the CWTools VS Code extension. It provides language services for Paradox Interactive game modding, supporting games like Stellaris, Hearts of Iron IV, Europa Universalis IV, Crusader Kings II/III, Victoria 2/3, and Imperator: Rome. The extension offers syntax validation, autocomplete, tooltips, localization checking, and visual graph analysis for game scripts.

## Architecture

Both halves live here: a VS Code extension with a TypeScript client, and the Rust language server it drives.

### Server

- `engine/` is the Rust workspace and the whole engine. It builds `cwtools-server`, the standalone LSP binary the extension launches over stdio, and `cwtools`, the batch CLI.
- The workspace shape: `parser` builds the AST, `rules` loads the `.cwt` config, `index` builds the cross-file lookups, `validation` is the rule engine plus the per-game validators, and `driver` is the shared load-and-validate pipeline both binaries call. The CLI is batch (one `Session` per run); the LSP keeps its own incremental state and does not use `Session`.
- Start with [ARCHITECTURE.md](docs/engine/ARCHITECTURE.md) for the crate map and the CLI-vs-LSP split, and [ERROR_CODES.md](docs/engine/ERROR_CODES.md) for what a CWxxx code means.
- Run the CLI from source with `cargo run -p cwtools_cli -- validate --game hoi4 --directory <mod> --rules <rules>/Config`.

### Frontend (TypeScript)

- **Extension host** (`extension/src/host/`): VS Code extension host and commands
- **Webview** (`extension/src/webview/`): Graph visualization using Cytoscape.js
- **Test suite** (`extension/test/`): Node and extension-host tests plus sample workspaces
- **Package inputs** (`extension/package/`): VS Code manifest, localization, grammars, themes, and snippets

### Guards and corpora

- `python3 scripts/guard.py md` and `scripts/md-baseline.csv` are the diagnostics regression gate against a pinned real mod (Millennium Dawn). `python3 scripts/guard.py vanilla` and `scripts/vanilla-baseline.csv` are the tier over a committed synthetic base game, for the checks that need one (CW113, CW222, CW227, CW229, CW250, CW500).
- Two sibling checkouts matter, expected next to this repo (override the location with `CWTOOLS_PROJECTS`): `cwtools-hoi4-config` holds the `.cwt` rules, which are not bundled, so a behavior change is as likely to belong there as here; `Millennium-Dawn` is the pinned corpus the guard validates.

### Build System

- **Build orchestrator** (`scripts/build/build.py`): builds and deploys the Rust server, compiles the client, packages and publishes the vsix. No .NET toolchain is involved.
- **TypeScript compilation**: `tsc` type-checks and emits the per-file client output (which the tests run against); `esbuild` (`scripts/build/esbuild.py`) bundles the two shipped artifacts: the extension host (`extension.js`) and the webview graph (`graph.js`).
- **Release packaging**: Assembles `dist/extension/` from checked-in package inputs and generated binaries, then creates `.vsix` files under `artifacts/vsix/`. The client is bundled, so `node_modules` is excluded by `extension/package/.vscodeignore`.

## Development Commands

### Building

```bash
# Windows
./build.cmd quick

# Unix/Linux
./build.sh quick
```

### TypeScript Client

```bash
npm install
npm run compile    # tsc + esbuild (bundle extension + webview)
npm run check      # typecheck + lint
npm test           # Run VS Code extension tests
```

The Rust server builds from the in-repo `engine` workspace. Set `CWTOOLS_RUST_WORKSPACE` to build from another checkout.

### Available Build Commands

Run as `npm run build -- <command>` or `./build.sh <command>`:

- `quick`: Build the server + client for local development
- `package`: Package a vsix without publishing
- `package-prebuilt`: Package server binaries already staged by CI, no publish
- `publish-prebuilt`: Publish the vsixes `package-prebuilt` produced
- `release`: Push a `v*` tag; the tag-triggered workflow builds, smoke-tests, and publishes
- `release-prebuilt`: `package-prebuilt` followed by `publish-prebuilt`

### Testing

Two test layers. `npm run test:node` runs the fast node-only unit tests under `extension/test/unit/`. The rest run in a real extension host against `extension/test/workspaces/stellaris`, picked by label from `.vscode-test.mjs`: `npm test` runs the `unit` label, `npm run test:smoke` adds activation against the real server, and `npm run test:host` adds hover and completion. CI gates on all four.

Every host label goes through `scripts/build/hosttest.py`, which picks a display backend so a local run does not open a VS Code window on your desktop. On Linux that is `xvfb-run -a`, and a missing `xvfb-run` fails with install instructions rather than falling back to a visible window. macOS and Windows have no such backend yet, so they run natively and print a notice (#406). `CWTOOLS_TEST_DISPLAY` overrides the choice with `xvfb`, `ozone` (Electron's headless Ozone backend, no system package), or `native`. `npm run test:native` is the explicit visible-window runner for platform debugging.

Single test: `npx --no-install vitest run extension/test/unit/engine.test.ts -t 'name'` for the node layer; for the host layer, `npm run compile` then `node scripts/build/python.mjs scripts/build/hosttest.py --label unit --grep 'name'` (unrecognized arguments pass through to `vscode-test`). Watch modes: `npm run test:watch` and `npm run test:node:watch`.

Coverage: `npm run test:coverage` (unit label, V8 coverage into `coverage/`) and `npm run test:node:coverage` (into `coverage-node/`). The two runs cover disjoint modules (engine.ts and executable.ts are vitest-owned), and `scripts/build/coverage_summary.py` renders rust, host, and node into the PR comment.

Host suites must not import extension modules directly: the host runs the esbuild bundle, so a direct import is a second copy of the module. Reach the extension's own modules through its activation API (`graphPanelModule()` in `extension/test/support/utils.ts`).

### The Python helpers

`scripts/` and `tests/scripts/` are their own toolchain: ruff, black, pylint, mypy and pytest, configured in `pyproject.toml` and pinned in `requirements-dev.txt` (`python3 -m pip install -r requirements-dev.txt`, one environment so mypy can see pytest's types). Run `ruff check scripts tests/scripts`, `black --check scripts tests/scripts`, `pylint scripts tests/scripts`, `mypy scripts tests/scripts`, and `pytest` before calling a change to them done. mypy is strict and pylint has to stay at 10.00; the `Python lint & tests` CI job runs that same list.

Tests are pytest-native: plain functions, `pytest.raises`, `tmp_path`, `monkeypatch`, and `@pytest.mark.parametrize` rather than `unittest.TestCase`. `scripts/build/*.py` import each other by bare name, so they are importable directly (`pythonpath` in `pyproject.toml`, `source-roots` for pylint). The standalone entry points in `scripts/` load by path from fixtures in `tests/scripts/conftest.py`, which is what keeps `scripts/coverage.py` from shadowing the `coverage` package.

## Before you call an engine change done

From `engine/`:

```plaintext
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

While iterating, scope tests down: `cargo test -p cwtools_validation`, or `cargo test -p cwtools_parser <substring>` for one test. Packages are named `cwtools_*`; the short directory names under `engine/crates/` won't resolve.

Then, from the repo root, the guards. Run both for anything that touches the parser, the rule engine, a validator, or the ruleset types:

```plaintext
python3 scripts/guard.py md
python3 scripts/guard.py vanilla
```

The test suite proves the code compiles and behaves. The guards prove the *diagnostics* did not move, which is the thing a "this changes nothing" refactor is easy to believe and hard to demonstrate. Details are in [the engine contributor guide](docs/engine/CONTRIBUTING.md).

Three things to know about them:

- Each committed baseline is pinned to specific corpus and rules revisions, recorded in its `#` header and printed on every run. When either checkout has moved on you get a diff that has nothing to do with your change. Capture your own before-baseline against the current inputs (`CWTOOLS_BASELINE=/tmp/before.csv python3 scripts/guard.py md --bless` on a clean tree) and compare against that instead.
- Re-blessing a committed baseline is for changes that are *meant* to move diagnostics, and the commit message has to say which codes moved and why. A re-bless with no explanation reads as a regression someone papered over.
- The vanilla tier covers base-game-dependent codes (CW113, CW222, CW227, CW229, CW250, CW500) that never fire in the md tier. A change that moves one baseline and not the other is usually telling you something.

## Key Files

- `package.json`: Node.js dependencies and scripts for the TypeScript client
- `extension/package/package.json`: VS Code extension manifest and configuration
- `scripts/build/build.py`: build/package/release orchestrator
- `scripts/build/esbuild.py`: esbuild bundler driver for the client (extension + webview)
- `.vscode-test.mjs`: host test runner config (labeled: unit/smoke/host)
- `scripts/build/hosttest.py`: display-backend picker in front of `vscode-test`
- `vitest.config.ts`: node-only unit test config
- `pyproject.toml`: ruff/black/pylint/mypy/pytest config for `scripts/` and `tests/scripts/`
- `requirements-dev.txt`: the pinned versions of those five
- `engine/Cargo.toml`: the Rust workspace manifest; all crates inherit its version
- `scripts/guard.py`: the diagnostics regression gate
- Build scripts: `build.cmd` (Windows) / `build.sh` (Unix)

## Development Workflow

1. Run `./build.sh quick` for initial setup and server compilation
2. Use `npm run compile` for TypeScript changes during development
3. Debug by launching "Quick update, Build and Launch Extension" in VS Code
4. Test with sample Paradox game mod files in `extension/test/workspaces/stellaris/`
5. Run tests with `npm test` before committing changes

## Conventions

Every substantive PR or push updates `CHANGELOG.md` as part of the change, engine work included. New version headings (`### x.y.z`) are numbered from the latest git tag, not the previous heading; unreleased work goes under `### Unreleased`. `scripts/build/build.py` copies the changelog into `dist/extension/` and reads the top version heading for the release version.

## Releasing

Being reworked as part of the repo merge. Until that lands, the extension side is unchanged: `npm run build -- release` checks the CHANGELOG, refuses a dirty tree or an existing tag, then pushes `v<x.y.z>`, and `.github/workflows/release.yml` does the matrix build, the smoke test and the publish. The engine's own tag-and-archive release process no longer applies now that it has no repo of its own.

## Performance work

Claim an improvement only with before and after numbers taken the same way. A rebuild evicts the page cache, so a run straight after `cargo build` is not comparable to one before it; interleave the two binaries instead of measuring them in blocks.

Benches live in `engine/crates/*/benches` and run under criterion. `cargo bench -p cwtools_driver --bench rules_hot` covers the editor hot paths and needs a rules checkout. `cargo bench -p cwtools_driver --bench validate_hot` covers the batch validation inner loop. [PROFILING.md](docs/engine/PROFILING.md) covers the `CWTOOLS_PROFILE` instrumentation.

## Don't

- Don't silence a lint, a type error or a failing check to get to green. Fix the cause, even when it predates your change.
- Don't add a dependency without checking it passes `cargo deny` and is actually used (`cargo machete`). Both run in CI.
- Don't widen a public API for one caller. Most crates here are internal to the workspace.
- Don't hardcode a Unix-style absolute path (`Path::new("/ws")`, a bare `"/foo/bar"`) in a test that reaches `Url::from_file_path` or `Path::is_absolute`. A leading `/` with no drive letter isn't absolute on Windows, so CI fails there even though Linux/macOS are fine. Build the fixture with the `abs()` helper already in `engine/crates/cli/src/report.rs` / `engine/crates/cli/src/config.rs` (or add an equivalent local one).
- Don't assert an exact result from `Url::to_file_path` or `std::fs::canonicalize` without checking it holds on Windows. `to_file_path` yields a path only when the first segment is a drive letter (`http://localhost/etc/passwd` converts on Unix, not Windows; `http://localhost/C:/Windows` converts on both), and `canonicalize` collapses a `dir/..` pair even when `dir` doesn't exist, so a climbing `..` reports `OutsideWorkspace` there where Linux reports `Unresolvable`. Pick a fixture that converts on both platforms, assert the shared guarantee (`is_err()`, containment) rather than a platform-specific variant, or gate the assert `#[cfg(unix)]` with a comment saying why (see `engine/crates/lsp/src/access.rs`). CI runs the full test suite on `windows-latest`, so a Unix-only assumption here turns every PR red.
- Don't change a type that gets serialized into a cache without bumping the format version next to it (`FORMAT_VERSION` in `engine/crates/cache/src/io.rs` for `.cwb` and `ERRORS_FORMAT_VERSION` for its `.cwe` sidecar, `CACHE_VERSION` in `engine/crates/index/src/vanilla_cache.rs` for `.cwv`, `CACHE_VERSION` in `engine/crates/cache/src/workspace.rs` for the parse cache's fingerprint). The bump is what turns an old cache into a clean miss instead of a load error.
