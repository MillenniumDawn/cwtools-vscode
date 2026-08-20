# Profiling

See `docs/ARCHITECTURE.md` for the loc system architecture and `BUILD.md`
for build instructions.

## Runtime profiling

The workspace is instrumented with [`tracing`](https://docs.rs/tracing); the
subscriber and its helpers live in the `cwtools_profiling` crate. It is off by
default and turns on when either of these is set, so normal runs stay quiet:

- `RUST_LOG`: standard env-filter (e.g. `RUST_LOG=cwtools_validation=info`).
  Output goes to **stderr** so it never corrupts the LSP's stdout JSON-RPC
  channel.
- `CWTOOLS_PROFILE`: turn on the profiling report (span timings at `info` plus
  RSS samples at phase boundaries) without spelling out a filter. Truthy values:
  `1`, `true`, `yes`, `on`. The LSP owns stdout for JSON-RPC and the VS Code
  client never drains the server's stderr, so under `CWTOOLS_PROFILE` the output
  is routed to a bounded in-memory ring buffer (4 MB, oldest bytes dropped)
  instead of stderr; the client fetches it with the export-profiling-log command.
  The buffer routing applies to every binary and only the LSP drains it, so on
  the CLI `CWTOOLS_PROFILE` gets you the `[profile]` RSS report and no span
  timings. For CLI timings use `RUST_LOG` and leave `CWTOOLS_PROFILE` unset;
  setting both still buffers.

## Run a profiled validate

```plaintext
RUST_LOG=info cargo run --release -p cwtools_cli -- \
  validate --game hoi4 --directory <mod> --rules <config>
```

With `RUST_LOG=info` the subscriber prints a span-close line for every
instrumented hot path, with its busy/idle time. The instrumented paths today:

- `parse_string` (parser) — one span per file parsed
- `collect_type_instances` (index) — one span per file indexed
- `TypeIndex::merge` (index) — one span per file merged into the type index
- `load` (index, `vanilla_cache`) — the vanilla cache read at startup
- `post_process` (rules) — the single ruleset post-processing pass
- `validate_ast_with_loc` / `validate_prepared` (validation) — one span per file validated
- `count_and_validate_children` / `validate_leaf` / `validate_alias_usage` (validation, TRACE) — one span per block, leaf, or alias usage. Off under `RUST_LOG=info`. Set `RUST_LOG=cwtools_validation=trace` to attribute time to a phase inside the file.
- `merged_rules_for_type` (validation) — one span per typed definition's subtype merge
- `parse_and_validate` / `index_parsed_file` (lsp) — one span per file the server validates
- `semantic_tokens_full_impl` / `semantic_tokens_full_delta_impl` / `semantic_tokens_range_impl` (lsp) — one span per semantic-tokens request
- the other request and scan entry points (lsp) — `did_open`/`did_change`/`did_close`, `completion_impl` and the completion builders under it, `debounced_validate`, `validate_entire_workspace_inner`, `rebuild_and_publish_loc`, `did_change_watched_files_impl`

Filter to a single crate to cut noise:

```plaintext
RUST_LOG=cwtools_validation=info cargo run --release -p cwtools_cli -- validate ...
RUST_LOG=cwtools_index=info,cwtools_rules=info cargo run --release -p cwtools_cli -- validate ...
```

(Diagnostics go to stdout; the trace output goes to stderr, so redirect with
`2> trace.log` to capture timings separately.)

## CLI per-phase timings

Independent of the tracing subscriber, `cwtools validate` prints coarse
per-phase timings on stderr when `CWTOOLS_TIMINGS` is set (any value). Each line
contains `[t] <phase> <elapsed>` for the three top-level stages: `load` (the whole
`Session::load`: rules, discovery, indexing, loc, registry), `validate-config`
(the file validation pass), and `validate-loc` (loc-project diagnostics). Use it
for a quick where-did-the-time-go read without pulling in the full span output:

```plaintext
CWTOOLS_TIMINGS=1 cargo run --release -p cwtools_cli -- validate --game hoi4 ...
```

## Add a new hot path

Put `#[tracing::instrument(skip_all)]` on the function (use `skip_all` so large
args aren't formatted), and make sure the crate has `tracing` in its
`Cargo.toml` (`tracing = { workspace = true }`). It shows up under
`RUST_LOG=<crate>=info` automatically. Per-call inner loops (one span per
leaf or block) should be `level = "trace"` so an `info` profile stays a
per-file picture.

## What to look for (runtime)

- A `parse_string` or `validate_ast_with_loc` span that dominates total time points at a
  pathological file (huge or deeply nested).
- `post_process` time scales with ruleset size; it runs once, so a large number
  there is a one-off cost, not per-file.
- `collect_type_instances` adds up across files; if indexing is slow, that's the
  span to drill into.

## Per-workspace ignore globs

The LSP workspace walk consults three lists, layered in this order:

1. **Engine baseline (always on)**: toolchain junk (`.git`, `.claude`, `target`, `.vs`,
   `node_modules`, `bin`, `obj`, `out`, `dist`, `.idea`, `.vscode`, plus `resources`
   at the workspace root only, since a nested `common/resources/` is game content)
   and free-form text files (`Changelog.txt`, `README.txt`, `LICENSE.txt`,
   `README.md`, `LICENSE.md`, `*.md`). These are hard-coded and cannot be
   disabled per-workspace — they exist because matching them otherwise
   wastes validator time on files that almost never contain script.
2. **User file globs**: forwarded by the extension from `cwtools.ignore_patterns`
   and `cwtools.errors.ignorefiles` (in `settings.json`) into
   `initializationOptions.ignoreFilePatterns`. Re-read on every
   `workspace/didChangeConfiguration`.
3. **User directory globs**: same key handling, `initializationOptions.ignoreDirectories`.
   The VS Code extension has no setting feeding it today; another client can send it.

Both user lists default to empty and extend (not replace) the engine
baseline.

A pattern with no separator names a file or directory, and matches it at any
depth: `*.md`, `Changelog.txt`, `temp*`. A pattern with one addresses a place in
the tree, and is matched against the path relative to the mod or workspace root:
`common/**/*.txt`, `**/skip.txt`, `gfx/interface/`. There, `*` and `?` stay
inside a single name while `**` spans any run of directories (including none), a
leading `/` is the root the path is already relative to, and a trailing one
covers everything below. Windows separators are accepted in a pattern and read
the same way.

The CLI exposes the same two lists as repeatable flags:
`--ignore-file GLOB` and `--ignore-dir GLOB`, on `validate` and on `loc`.

## Workspace parse cache (status)

Two pieces:

**3a (in-memory pass-through) — shipped.** The LSP full-workspace scan runs
two passes over every file. Pass 1 parses to populate the type index, then hands
its `Vec<Option<ParsedFile>>` to validation in pass 2. The ASTs are dropped before
the profile/RSS-summary block. Net effect: ~4-6s shaved off the scan,
steady-state RSS unchanged.

**3b (on-disk persisted) — shipped.** The LSP and CLI share the parse cache
under `<cache_dir>/parse-cache/<root-fingerprint>/`, one namespace per
mod/workspace or base-game install. Each entry contains a self-contained `.cwb`
AST and a recovered-parse-error sidecar (`.cwe`). Loading interns its strings
into the current process's `StringTable`.

`<cache_dir>` is the LSP's `cacheDir` initializationOption when the client sends
one. Otherwise, and always for the CLI, it is `XDG_CACHE_HOME/cwtools`, else
`%LOCALAPPDATA%\cwtools`, else `~/.cache/cwtools` (`~/Library/Caches/cwtools` on
macOS), else a temp dir. The base-game index (`.cwv`) lives under the same root.
The fingerprint covers the game, the workspace root and the cache format
version. The ruleset is not in it: a `.cwb` entry is a parsed AST and `.cwt`
rules cannot change how a script file parses, so editing the rules keeps the
cache warm.

On Unix, disk-file entries use the path, mtime, size, device, inode, and ctime, so a
warm hit skips reading the source. Other platforms use a content hash after the
read. The CLI caches both indexing and validation, but still drops the first
AST set before localisation indexing to keep the large-workspace memory bound.
A stale `.cwv` rebuild reuses the base-game install's entries, parsing only its
new or changed source files.
