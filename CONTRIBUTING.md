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

Other commands: `package` packages a vsix without publishing, `release` tags and does a full build and publish, `release-prebuilt` packages binaries already staged by CI.

## Running and debugging

Open the repo in VS Code and launch **Quick update, Build and Launch Extension** (or the Debug variant) from the Run panel. That builds and opens an Extension Development Host with the extension loaded. Point it at a mod folder under a game directory to see validation.

## Tests

```bash
npm test               # extension tests through the VS Code host
npm run test:host      # host-based suite
npm run test:coverage  # unit suite with V8 coverage
```

`test:coverage` uses c8 (V8 coverage) to write an HTML report to `coverage/` — open `coverage/index.html` for line-by-line browsing, or point the [Coverage Gutters](https://marketplace.visualstudio.com/items?itemName=ryanluker.vscode-coverage-gutters) extension at `coverage/lcov.info` to see it inline. The numbers count only the hand-written client source (`client/extension`, `client/common`); dependencies are filtered out so the figures mean something. CI renders the same summary as a markdown table in the job summary and as a sticky PR comment, and uploads the HTML report as the `coverage-html` artifact. It's all local/OSS, no external service. Coverage is informational, not a merge gate (see issue #7).
