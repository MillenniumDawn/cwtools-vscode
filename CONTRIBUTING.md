# Contributing

## Layout

This repo is the VS Code extension. The language server it drives is the `cwtools-rs` workspace in the [cwtools](https://github.com/MillenniumDawn/cwtools) repo, which ships as a standalone per-platform binary.

The TypeScript client lives in `client/`: `extension/` (the host), `webview/` (the Cytoscape graph), and `test/` (tests plus the sample mod). The build is driven by `build/build.ts` (run via `tsx`). The Rust server source is pulled in as a submodule at `submodules/cwtools`.

## Prerequisites

- Node 20
- Rust (stable) with cargo, for the server
- git, for the submodules

## Getting the source

```bash
git clone --recursive https://github.com/MillenniumDawn/cwtools-vscode
# or, in an existing checkout:
git submodule update --init --recursive
```

## Building

```bash
npm install        # client dependencies
./build.sh quick   # Unix; build.cmd on Windows
```

`quick` builds and deploys the Rust server into `release/bin/server`, compiles the client, and leaves a runnable extension in `release/`.

The client is bundled with esbuild (`build/esbuild.ts`): `tsc` type-checks and emits the per-file output the tests run against, then esbuild produces the two shipped bundles (`extension.js`, `webview/graph.js`). `npm run compile` does both; `npm run check` runs the typecheck and lint.

The Rust server builds from the `cwtools-rs` workspace. By default the build looks for it as a sibling checkout (`../cwtools/cwtools-rs`), which matches CI. To build from the submodule or another checkout, point it there:

```bash
CWTOOLS_RUST_WORKSPACE=submodules/cwtools/cwtools-rs ./build.sh quick
```

Other commands: `package` packages a vsix without publishing, `package-prebuilt` packages the binaries already staged by CI (one vsix per platform plus a universal fallback), `publish-prebuilt` publishes what `package-prebuilt` produced, `release-prebuilt` does both, and `release` tags and does a full build and publish.

## Syntax highlighting

The TextMate grammars under `release/syntaxes/` are vendored from
[cwtools/paradox-syntax](https://github.com/cwtools/paradox-syntax) and
re-synced with [`tools/sync-paradox-syntax.sh`](tools/sync-paradox-syntax.sh).
The script expects an upstream checkout as a sibling (`../paradox-syntax`);
override with `PARADOX_SYNTAX_SRC=...` if you keep it elsewhere. After
syncing, eyeball the diff before committing: the grammars are mostly
mechanical, but scope names sometimes change.

Themes under `release/themes/` are owned in this repo. Highlighting runs off a
single merged `paradox.tmLanguage.json` with each game's keywords folded in, so
re-vendoring means merging the upstream per-game grammars into it rather than
copying them across.

## Rules pins

Each game in `client/extension/games.ts` names its rules repo and a bundled
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
npx tsx build/rulesPins.ts
```

It reads each repo's default branch head, rewrites both pin sets, and increments
the manifest revision. [`rules-pins.yml`](.github/workflows/rules-pins.yml)
runs the same script weekly and opens a PR with a compare link for everything
that moved. Read those diffs before merging: the commits in `rules-pins.json`
are what installed extensions fetch on their next activation. Nothing
auto-merges here, unlike the engine submodule bump.

## Running and debugging

Open the repo in VS Code and launch **Quick update, Build and Launch Extension** (or the Debug variant) from the Run panel. That builds and opens an Extension Development Host with the extension loaded. Point it at a mod folder under a game directory to see validation.

## Tests

```bash
npm run test:node      # node-only unit tests for the pure modules (vitest, fast)
npm test               # unit label: VS Code API, no language server
npm run test:smoke     # unit plus activation against the real server
npm run test:host      # everything, including hover and completion
npm run test:coverage  # unit label with V8 coverage
npm run test:node:coverage  # vitest coverage into coverage-node/
npm run bench:node     # client hot-path benchmarks
```

Two layers. `test:node` runs under vitest with no Electron and owns the pure
modules (`engine.ts`, `executable.ts`, `games.ts`, the signature/settings
helpers, the manifest and nls guards). The rest run in a real extension host
against the sample mod in `client/test/sample/`, picked by label from
`.vscode-test.mjs`.

CI gates on `test:node`, `test` and `test:smoke`. It does not gate on
`test:host`: the hover and completion suites assert on rule-driven data, and the
sample workspace has no game name in its path and no game-specific content dir,
so it detects as the generic `paradox` language, which has no rules repo to
clone. Those suites fail until the fixture is made identifiable as a game (or is
pointed at a local `cwtools.rules_folder` and a vanilla install).

`test:coverage` uses c8 (V8 coverage) to write an HTML report to `coverage/` — open `coverage/index.html` for line-by-line browsing, or point the [Coverage Gutters](https://marketplace.visualstudio.com/items?itemName=ryanluker.vscode-coverage-gutters) extension at `coverage/lcov.info` to see it inline. The numbers count only the hand-written client source (`client/extension`, `client/common`); dependencies are filtered out so the figures mean something. CI renders the node coverage summary as a markdown table in the job summary and as a sticky PR comment, and uploads the raw report as the `coverage-node` artifact. It's all local/OSS, no external service. Coverage is informational, not a merge gate (see issue #7).
