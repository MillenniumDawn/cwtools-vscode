# cwtools

A library for parsing, editing, and validating Paradox Interactive script files.

> **Fork notice:** This is a fork of [cwtools/cwtools](https://github.com/cwtools/cwtools). The original F# library (NuGet packages, .NET Standard) lives at the upstream repo. Please give them their love as well for inspiring this wonderful project.

> **Game support:** Right now we predominantly support **Hearts of Iron IV**. The validator is built in Rust and HOI4 is where it's complete and tested. **Stellaris** also ships native validators (CW108/109/110/120/227/229/231/250 plus the if/else and set_name checks CW236/237/253). The other games (EU4, EU5, CK2/CK3, Vic2/Vic3, Imperator) parse, but their validation and per-game rules are partial while we get the foundation right. Full multi-game parity is tracked in the [issues](https://github.com/MillenniumDawn/cwtools-vscode/issues).

## What it does

The engine parses Paradox script and localisation, indexes the mod (and
optionally the base game install), and validates it against a `.cwt` ruleset.
Both binaries drive the same pipeline.

`cwtools-server`, the language server:

- Diagnostics as you type, a full workspace scan at startup, and an idle-gated background rescan.
- Completion, hover, goto definition, find references, document highlight, rename.
- Document and workspace symbols, folding and selection ranges, document links.
- Quick fixes, fix-all within a document, and `fixAllWorkspace` across every file.
- Semantic tokens (full, delta and range), inlay hints, and color swatches.
- Commands to reload the rules, rebuild or clear the caches, re-index, and generate missing localisation stubs.
- Graph data behind the extension's focus, tech and event tree view.
- Long commands report progress and can be cancelled mid-run.

`cwtools`, the CLI:

- `validate` checks a mod against a ruleset. A workspace of mods is detected and layered by load order.
- `loc` checks localisation `.yml` on its own, with `--ignore-file`, `--ignore-dir` and `--loc-language` to narrow the scan. The file-level checks need nothing else; add `--game` and `--rules` and it loads the ruleset's scopes and links too, so a `[command]` chain that names nothing or leaves a scope its next link doesn't accept is reported (CW226/CW260/CW266). It reads the ruleset, never the game files, so those chains start from an unknown scope: `validate` is what checks them where each key is used.
- `fix` applies the machine-applicable fixes, dry-run by default.
- `cache-vanilla` pre-indexes a base game install so later runs skip re-parsing it.
- `parse`, `discover`, `rules`, `serialize` and `deserialize` inspect one file, a tree, or a `.cwb` cache.
- `explain CWxxx` prints what one code means, `list-codes` prints the whole catalog, and `completions <shell>` writes a completion script.
- Reports as text, CSV, JSON, GitHub Actions annotations or SARIF, with severity and code filters, hash baselines, and settings from a `cwtools.toml`.
- `validate --file` and `--since <git-ref>` scope the report to the files you touched, for a pre-commit hook or a PR job.

Both reuse the on-disk caches: parsed ASTs (`.cwb`) and the base game index,
each kept until its inputs change.

## Install

`cwtools-server` is the language server behind the editor integration. The VS
Code extension in this repo bundles its own copy, so you only need the
standalone binary to wire cwtools into a different editor.

Standalone archives shipped from the engine's own repo before it moved in here;
the release process is being reworked as part of the repo merge. Until that
lands, build from source. It is `cargo build --release` from this directory,
with no other prerequisites, and [BUILD.md](BUILD.md) covers the details.

`cwtools` is the command-line validator:

```plaintext
cwtools validate --game hoi4 --directory path/to/mod --rules path/to/cwtools-hoi4-config/Config
```

`cwtools --help` lists the other subcommands (`fix`, `loc`, `cache-vanilla`, ...).

The `.cwt` rules are a separate repo, not bundled. HOI4 uses
[cwtools-hoi4-config](https://github.com/cwtools/cwtools-hoi4-config); point
`--rules` at its `Config` directory.

## Where the code lives

This directory is the whole engine, a Rust workspace of 15 crates under
`crates/`. `parser` turns script text into an arena AST over the `string_table`
interner, `rules` loads the `.cwt` config into a `RuleSet`, `index` builds the
cross-file lookups, `localization` covers the `.yml` side, and `validation` is
the rule engine plus the per-game validators that emit the CWxxx diagnostics.
`driver` assembles those into the load-and-validate pipeline both front ends
call. `lsp` is the `cwtools-server` binary, with `info` holding the incremental
per-file index behind its hover, goto and find-references, and `cli` is the
`cwtools` binary. The rest are small and sit underneath: `game` (the `Game`
enum, scopes and links), `error_codes` (the shared code catalog), `cache` (the
on-disk AST cache), `file_manager` (file discovery), and `profiling`.

The diagnostics regression guards live in the repo root's `scripts/`, one
directory up.

## Documentation

- [Architecture](docs/ARCHITECTURE.md) — the crate map, the batch pipeline, the CLI-vs-LSP split, and LSP features like the idle-gated background reindex.
- [CWXXX error/warning code reference](docs/ERROR_CODES.md) — full catalog of diagnostic codes emitted by the Rust validator.
- [Math expressions](docs/MATH_EXPRESSIONS.md) — the HOI4 math-block operators and what the validator enforces inside one.
- [Profiling guide](PROFILING.md) — how to measure validation performance.

## Projects that use CW Tools
#### [Stellaris tech tree](http://www.draconas.co.uk/stellaristech): https://github.com/draconas1/stellaris-tech-tree
An interactive tech tree visualiser that uses CW Tools to parse the vanilla tech files, and extract localisation.
#### [SC Mod Manager](https://github.com/WojciechKrysiak/SCModManager): https://github.com/WojciechKrysiak/SCModManager/tree/feature/PortToAvalonia/PDXModLib/Utility
A mod manager that uses CW Tools for parsing and manipulating mod files.
