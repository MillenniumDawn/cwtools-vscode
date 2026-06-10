# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

CWTools MD Edition is the Millennium Dawn fork of the CWTools VS Code extension. It provides language services for Paradox Interactive game modding, supporting games like Stellaris, Hearts of Iron IV, Europa Universalis IV, Crusader Kings II/III, Victoria 2/3, and Imperator: Rome. The extension offers syntax validation, autocomplete, tooltips, localization checking, and visual graph analysis for game scripts.

## Architecture

This is a VS Code extension with a TypeScript client and a Rust language server.

### Server
- The `cwtools-rs` workspace in the cwtools repo builds to a single standalone binary (`cwtools-server`) launched over stdio.
- The Rust server source is a git submodule at `submodules/cwtools` (Rust under `submodules/cwtools/cwtools-rs`).

### Frontend (TypeScript)
- **Client Extension** (`client/extension/`): VS Code extension host and commands
- **Webview** (`client/webview/`): Graph visualization using Cytoscape.js
- **Test Suite** (`client/test/`): Host-based extension tests using the sample mod

### Build System
- **Build orchestrator** (`build/build.ts`, run via `tsx`): builds and deploys the Rust server, compiles the client, packages and publishes the vsix. No .NET toolchain is involved.
- **TypeScript compilation**: `tsc` type-checks and emits the per-file client output (which the tests run against); `esbuild` (`build/esbuild.ts`, run via `tsx`) bundles the two shipped artifacts: the extension host (`extension.js`) and the webview graph (`graph.js`).
- **Release packaging**: Creates `.vsix` files with `vsce package --no-dependencies`. The client is bundled, so `node_modules` is excluded from the vsix (`release/.vscodeignore`). CI builds the Rust server per platform and `release-prebuilt` packages the staged binaries into one vsix.

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

The Rust server builds from the `cwtools-rs` workspace, which the build expects as a sibling checkout (`../cwtools/cwtools-rs`) by default. Set `CWTOOLS_RUST_WORKSPACE` to build from the submodule (`submodules/cwtools/cwtools-rs`) or elsewhere.

### Available Build Commands
Run as `npm run build -- <command>` or `./build.sh <command>`:
- `quick`: Build the server + client for local development
- `package`: Package a vsix without publishing
- `release`: Tag, then full build + publish
- `release-prebuilt`: Package server binaries already staged by CI, then publish

### Testing
Run `npm test` for the host-based extension tests, which use the sample mod in `client/test/sample/`. `npm run test:host` runs the full host suite; `npm run test:coverage` adds V8 coverage.

## Key Files

- `package.json`: Node.js dependencies and scripts for the TypeScript client
- `release/package.json`: VS Code extension manifest and configuration
- `build/build.ts`: build/package/release orchestrator
- `build/esbuild.ts`: esbuild bundler driver for the client (extension + webview)
- `.vscode-test.mjs`: host test runner config (labeled: unit/host)
- Build scripts: `build.cmd` (Windows) / `build.sh` (Unix)

## Development Workflow

1. Run `git submodule update --init --recursive`, then `./build.sh quick` for initial setup and server compilation
2. Use `npm run compile` for TypeScript changes during development
3. Debug by launching "Quick update, Build and Launch Extension" in VS Code
4. Test with sample Paradox game mod files in `client/test/sample/`
5. Run tests with `npm test` before committing changes

## CWTools Integration

The extension bundles the Rust `cwtools-server` binary, whose source is pulled in via the `submodules/cwtools` git submodule. The build compiles and deploys it into `release/bin/server`.