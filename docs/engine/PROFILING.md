# Profiling

See `ARCHITECTURE.md` for the loc system architecture and `BUILD.md`
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
AST and a sidecar (`.cwe`) with the recovered parse errors, the source's content
hash and its `# cwtools-ignore` codes. Loading interns its strings into the
current process's `StringTable`.

`<cache_dir>` is the LSP's `cacheDir` initializationOption when the client sends
one. Otherwise, and always for the CLI, it is `XDG_CACHE_HOME/cwtools`, else
`%LOCALAPPDATA%\cwtools`, else `~/.cache/cwtools` (`~/Library/Caches/cwtools` on
macOS), else a temp dir. The base-game index (`.cwv`) lives under the same root.
The fingerprint covers the game, the workspace root and the cache format
version. The ruleset is not in it: a `.cwb` entry is a parsed AST and `.cwt`
rules cannot change how a script file parses, so editing the rules keeps the
cache warm.

On Unix, disk-file entries use the path, mtime, size, device, inode, and ctime, so a
warm hit parses nothing and reads no source, in the CLI load or the LSP scan.
The CLI still reads a file to print the source lines of its diagnostics. The LSP takes the
content hash it uses to match fixable edits to a buffer, and the inline-ignore
codes, from the sidecar. It still stats each file and charges that size against
the scan byte budget. A miss, or a file whose key cannot be read, reads the
source, parses it and rewrites the entry. Inline scripts under
`common/inline_scripts/` are reloaded from the cache the same way each scan.

Windows has no path key, so both binaries read every file and look the cache up
by content hash. Stable `std` offers no file id (`file_index` and
`volume_serial_number` are behind `windows_by_handle`) and no ChangeTime
(`windows_change_time`). Creation time is not an identity, because NTFS tunneling
can give a recreated file its old one, and a write time can be set back. A
same-size edit with a restored mtime would then serve a stale AST, which the Unix
key catches through ctime.

The CLI caches both indexing and validation, but still drops the first
AST set before localisation indexing to keep the large-workspace memory bound.
A stale `.cwv` rebuild reuses the base-game install's entries, parsing only its
new or changed source files.

## Localisation indexing and alias candidate replacement

Alias candidate replacement groups repeated raw keys before expanding patterns,
then restores source/pattern order within each referenced-name bucket. It keeps
all positions, duplicate patterns, case-sensitive affixes and ASCII-folded name
lookups. Rulesets with no alias patterns skip grouping.

`LocIndex::build_scoped` reuses each raw key's lowercased `Arc` across translations
and its command-selection pass. The memo lasts only for the build;
single-language indexes skip it. English precedence, Unicode lowercasing and
shared key allocations are unchanged.

Measured against `a561b53e` on Linux x86-64, i9-12900K, Rust 1.99.0, with the
existing release profile. Inputs were frozen script/localisation exports from
Millennium Dawn `bcbf8ed2ef59ad58068feecaea68bff1c06c2552` and the HOI4 rules at
`40a563ccfded2f56d32e83d8def1a20c27fcdb1d`. There were 7,653 scripts, 3,074
localisation files, 2,364,950 entries and 240,190 normalized keys. No base game
was configured.

Before and after binaries were built separately, then run in alternating order
with warm page caches and CPU affinity fixed to CPU 0. These Criterion point
estimates use 15 samples for localisation and 20 for indexing:

| Workload | Before | After |
| --- | ---: | ---: |
| Localisation index build | 574.53 ms | 464.50 ms |
| Iran focus-tree replacement | 15.428 ms | 12.344 ms |
| Iran alias-candidate replacement alone | 7.4465 ms | 4.0890 ms |
| France events replacement | 7.3998 ms | 6.1024 ms |
| Plane-equipment replacement | 1.3836 ms | 1.0900 ms |
| Synthetic many-type replacement, no alias patterns | 77.862 us | 79.159 us |

Run `cargo bench -p cwtools_driver --bench workspace_load -- loc_index` and
`cargo bench -p cwtools_lsp --bench validate -- index_parsed_file` from `engine/`,
with `CWTOOLS_PROJECTS` pointing at the frozen inputs. Localisation excludes
output destruction; file replacement includes clearing its prior results. The
per-file cases use single-file index state, not a full running server workspace.
These are component timings, not end-to-end editor latency or memory savings.
Both temporary maps add build-time storage; no memory reduction is claimed.

## Localisation file-record churn (#846)

`LocLocations` releases a removed file's URI immediately and keeps its ID
retired until the old sites have been swept. Only then can another file reuse
that ID. Files with no sites can be recycled on removal without a sweep.
The site sweep threshold remains one eighth; retired-record count also counts
against that threshold because `insert_single` can discard tombstoned sites.
This avoids retaining records indefinitely when their sites disappear that way.
Reclamation visits retired IDs rather than scanning all live file records.
Buffers retain their high-water capacity for reuse; this is not shrink-to-fit.

Run the deterministic storage regressions from `engine/`:

```sh
cargo test -p cwtools_lsp churn -- --nocapture
cargo test -p cwtools_lsp loc_locations
```

Measured on Linux x86-64 with stable Rust 1.99.0, debug test binaries, against
`ea647d32afe1241bf87369f0a0b58b077d58c4ac` with the same two churn workloads.
Each workload inserts/removes one key at the same URI 100,000 times; the second
also keeps 10,000 distinct files and keys alive throughout. The baseline fails
the new bounds after printing its measurements; the fixed version passes.

| Workload | Before file slots | After file slots | Before file-vector capacity bytes | After file-vector + ID-queue capacity bytes | Sweeps before / after |
| --- | ---: | ---: | ---: | ---: | ---: |
| Empty after each removal | 100,000 | 1 | 3,145,728 | 96 + 32 | 100,000 / 100,000 |
| 10,000 mostly-live files | 110,000 | 11,429 | 3,145,728 | 393,216 + 16,384 | 69 / 69 |

File-vector capacities were 131,072 slots before, and 4 / 16,384 after;
`size_of::<File>()` was 24 bytes in both versions. Queue capacities after
were 4 / 2,048 `u32`s each for the free and retired queues. These numbers
measure reserved element storage, excluding allocator headers, hash maps,
site vectors, and URI allocations. The tests reuse one caller-owned URI Arc;
its strong-reference count after churn falls from 100,001 to 1. Real refreshes
can allocate a new Arc for each read, so this fixture understates their old
URI allocation retention. A separate weak-reference regression verifies that
removal releases the URI even before its retired ID is eligible for reuse.

Peak process RSS was sampled with Linux `os.wait4(pid, 0)[2].ru_maxrss` (KiB)
for direct executions of the saved before/after test binaries, one exact test
per fresh child process. Three interleaved before/after pairs gave:

| Workload | Before peak RSS range (KiB) | After peak RSS range (KiB) |
| --- | ---: | ---: |
| Empty churn | 10,624–10,972 | 8,896–8,960 |
| Mostly-live churn | 13,776–14,072 | 12,080–12,200 |

Build processes were excluded. RSS includes the test harness and allocator
behavior, and the baseline exits with the expected failing assertion. These
small synthetic process measurements are not a heap-allocation profile or an
estimate of a real editing session's RSS. No timing improvement is claimed.
