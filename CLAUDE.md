# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

CWTools MD Edition is the Millennium Dawn fork of the CWTools VS Code extension. It provides language services for Paradox Interactive game modding, supporting games like Stellaris, Hearts of Iron IV, Europa Universalis IV, Crusader Kings II/III, Victoria 2/3, and Imperator: Rome. The extension offers syntax validation, autocomplete, tooltips, localization checking, and visual graph analysis for game scripts.

## Architecture

This is a VS Code extension with a TypeScript client and a language server that comes in two engines. The `cwtools.engine` setting (`rust` or `fsharp`) picks which one the extension launches.

### Server engines
- **Rust** (default): the `cwtools-rs` workspace in the cwtools repo. Builds to a single standalone binary (`cwtools-server`) launched over stdio.
- **F#**: `src/Main` (server), `src/LSP` (LSP protocol), `src/Languages`, `src/CSharpExtensions` (C# helpers). Wraps the CWTools library and stays as a fallback.
- The CWTools library and the Rust port are a git submodule at `submodules/cwtools` (Rust under `submodules/cwtools/cwtools-rs`). The F# server references CWTools by project reference; `src/Main/cwtools.local.props` can repoint it at a local checkout.

### Frontend (TypeScript)
- **Client Extension** (`client/extension/`): VS Code extension host and commands
- **Webview** (`client/webview/`): Graph visualization using Cytoscape.js
- **Test Suite** (`client/test/`): Host-based extension tests, plus a host-free engine parity harness in `client/test/parity/`

### Build System
- **FAKE build script** (`build/Program.fs`): Cross-platform F# build automation. `QuickBuild` builds and deploys both engines.
- **TypeScript compilation**: `tsc` type-checks and emits the per-file client output (which the tests run against); `esbuild` (`build/esbuild.ts`, run via `tsx`) bundles the two shipped artifacts: the extension host (`extension.js`) and the webview graph (`graph.js`).
- **Release packaging**: Creates `.vsix` files with `vsce package --no-dependencies`. The client is bundled, so `node_modules` is excluded from the vsix (`release/.vscodeignore`). CI builds the Rust server per platform and `ReleasePrebuilt` packages the staged binaries into one vsix.

## Development Commands

### Building
```bash
# Windows
./build.cmd QuickBuild

# Unix/Linux  
./build.sh QuickBuild

# Debug build
./build.cmd QuickBuildDebug
```

### TypeScript Client
```bash
npm install
npm run compile    # tsc + esbuild (bundle extension + webview)
npm run check      # typecheck + lint
npm test           # Run VS Code extension tests
```

The Rust server builds from the `cwtools-rs` workspace, which the build expects as a sibling checkout (`../cwtools/cwtools-rs`) by default. Set `CWTOOLS_RUST_WORKSPACE` to build from the submodule (`submodules/cwtools/cwtools-rs`) or elsewhere.

### Available Build Targets
- `QuickBuild`: Build both engines + client for local development (Release)
- `QuickBuildDebug`: Same, Debug
- `DryRelease`: Package a vsix without publishing
- `Release`: Full build + publish
- `ReleasePrebuilt`: Package server binaries already staged by CI

### Testing
Run `npm test` for the host-based extension tests, which use the sample mod in `client/test/sample/`. `npm run test:parity` runs the host-free F#-vs-Rust parity suite (see `client/test/parity/README.md`); `test:engine:fsharp` and `test:engine:rust` run the host suite against a specific engine.

## Key Files

- `package.json`: Node.js dependencies and scripts for the TypeScript client
- `release/package.json`: VS Code extension manifest and configuration
- `cwtools-vscode.slnx`: .NET solution with the F# server projects
- `build/Program.fs`: FAKE build automation
- `build/esbuild.ts`: esbuild bundler driver for the client (extension + webview)
- `.vscode-test.mjs`: host test runner config (labeled: unit/host/fsharp/rust)
- Build scripts: `build.cmd` (Windows) / `build.sh` (Unix)

## Development Workflow

1. Run `git submodule update --init --recursive`, then `./build.sh QuickBuild` for initial setup and server compilation
2. Use `npm run compile` for TypeScript changes during development
3. Debug by launching "Quick update, Build and Launch Extension" in VS Code
4. Test with sample Paradox game mod files in `client/test/sample/`
5. Run tests with `npm test` before committing changes

## CWTools Integration

The extension bundles both the Rust port and the F# CWTools library, pulled in via the `submodules/cwtools` git submodule. The build compiles and deploys both engines so `cwtools.engine` can switch between them without a rebuild.