# Contributing

## Layout

This repo is the VS Code extension. The language server it drives comes in two engines:

- **Rust** (default): the `cwtools-rs` workspace in the [cwtools](https://github.com/MillenniumDawn/cwtools) repo. Ships as a standalone per-platform binary.
- **F#**: `src/Main`, `src/LSP`, `src/Languages`, `src/CSharpExtensions`, wrapping the CWTools library. Kept as a fallback.

The TypeScript client lives in `client/`: `extension/` (the host), `webview/` (the Cytoscape graph), and `test/` (tests plus the sample mod). The `cwtools.engine` setting picks which server the extension launches. The cwtools library and the Rust port are pulled in as a submodule at `submodules/cwtools`.

## Prerequisites

- .NET 9 SDK
- Node 20
- Rust (stable) with cargo, for the Rust engine
- git, for the submodules

## Getting the source

```bash
git clone --recursive https://github.com/MillenniumDawn/cwtools-vscode
# or, in an existing checkout:
git submodule update --init --recursive
```

## Building

```bash
npm install            # client dependencies
./build.sh QuickBuild  # Unix; build.cmd on Windows
```

`QuickBuild` builds and deploys both engines into `release/bin/server`, compiles the client, and leaves a runnable extension in `release/`. Use `QuickBuildDebug` for debug binaries.

The Rust server builds from the `cwtools-rs` workspace. By default the build looks for it as a sibling checkout (`../cwtools/cwtools-rs`), which matches CI. To build from the submodule or another checkout, point it there:

```bash
CWTOOLS_RUST_WORKSPACE=submodules/cwtools/cwtools-rs ./build.sh QuickBuild
```

Other targets: `DryRelease` packages a vsix without publishing, `Release` does a full build and publish, `ReleasePrebuilt` packages binaries already staged by CI.

## Running and debugging

Open the repo in VS Code and launch **Quick update, Build and Launch Extension** (or the Debug variant) from the Run panel. That builds and opens an Extension Development Host with the extension loaded. Point it at a mod folder under a game directory to see validation.

## Tests

```bash
npm test               # extension tests through the VS Code host
npm run test:host      # host-based suite
npm run test:parity    # host-free F#-vs-Rust engine parity
npm run test:engine:fsharp
npm run test:engine:rust
```

The parity suite treats the F# server as the spec and checks the Rust port against it. Its `[rust]` tests are expected to fail until the port reaches parity. See `client/test/parity/README.md`.

## Building against a local cwtools

To use a local cwtools checkout instead of the submodule for the F# server, create `src/Main/cwtools.local.props` (gitignored):

```xml
<Project>
  <PropertyGroup>
    <UseLocalCwtools Condition="'$(UseLocalCwtools)' == ''">True</UseLocalCwtools>
    <CwtoolsPath>../../../cwtools/CWTools/CWTools.fsproj</CwtoolsPath>
  </PropertyGroup>
</Project>
```

Amend the path to your checkout. The default assumes cwtools sits next to this repo. Without this file, `Main.fsproj` references the submodule copy instead.
