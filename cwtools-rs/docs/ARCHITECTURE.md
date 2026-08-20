# Architecture

cwtools-rs is a Rust workspace (under `cwtools-rs/`) of 15 crates: a parser, a
rule engine, per-game validators, a localisation subsystem, and two front ends
(the `cwtools` CLI and the `cwtools-server` LSP). This doc maps the crates, the
load pipeline, and the lockstep sites for adding a game or an error code. For the
diagnostic catalog see [`ERROR_CODES.md`](ERROR_CODES.md).

## Crate map

Layer 0 (leaves, no cwtools dependencies):

- `profiling`: tracing subscriber + RSS sampling for the CLI/LSP binaries (see `PROFILING.md`).
- `error_codes`: the shared `CW###` catalog. Deliberately dependency-free so
  `validation` and `localization` share the same codes without a dependency edge.
- `string_table`: the string interner. `StringTable::new()` builds a fresh sharded
  table; `Clone` shares that instance only. AST keys are `u32` ids into it.
- `game`: the `Game` enum plus scope/link data (the `ScopeDef` tables, the scope engine
  with `ScopeId`/`ScopeContext`/transitions, and the config-driven `ScopeRegistry`).

Then, roughly bottom-up:

- `parser`: Paradox script text to an arena AST (on `string_table`).
- `file_manager`: file discovery + parse orchestration (which dirs/files to walk,
  the exclude globs; on `parser`, `string_table`).
- `cache`: rkyv+zstd on-disk AST cache (`.cwb`), plus the per-file workspace parse
  cache both front ends share (`workspace.rs`) (on `parser`, `string_table`).
- `rules`: `.cwt` rule loading, giving a `RuleSet` of types/aliases/enums and
  scope/link inputs (on `game`, `parser`, `string_table`, `error_codes`, `file_manager`).
- `localization`: `.yml` loc parsing, `LocService`/`LocIndex`, loc reference and
  scope validation (on `error_codes`, `game`, `file_manager`, `parser`).
- `index`: `TypeIndex`/`VarIndex`/`FileIndex` + value-set/complex-enum collection
  plus the vanilla cache payload (on `parser`, `file_manager`, `string_table`, `rules`, `cache`).
- `validation`: the rule engine and per-game validators; emits `ValidationError`s
  (on `error_codes`, `parser`, `rules`, `string_table`, `game`, `index`, `localization`).
- `info`: the incremental per-file index (`InfoService`) backing LSP hover, goto,
  and find-references (on `parser`, `string_table`, `rules`, `index`).
- `driver`: the shared load-and-validate pipeline both front ends call
  (on `validation`, `index`, `localization`, and the layers below).
- `lsp`: the tower-lsp server, `cwtools-server` (on `driver`, `validation`, `info`,
  `cache`, `profiling`, and below).
- `cli`: the `cwtools` binary (on `driver`, `validation`, `info`, `profiling`, and below).

The dependency graph is acyclic. `error_codes` and `game` sit at the bottom with no
cwtools dependencies, so everything above can key off them.

## The batch pipeline

The full load pipeline lives in `crates/driver/src/lib.rs`:

1. Load the `.cwt` rules into a `RuleSet`.
2. Discover and parse the mod files (sharing one `StringTable`, so interned ids match).
3. Build the `TypeIndex` (plus the variable index and, when a vanilla install or
   cache is given, the vanilla index).
4. Expand the modifier keys valid in `alias_name[modifier]` slots.
5. Build the loc index (`LocIndex`) over the mod + vanilla loc keys.
6. Build the per-run `ScopeRegistry` from the config's scopes and links.
7. Validate every file against that prebuilt state.
8. Report the definitions nothing referenced (CW239/CW231).

Steps 3 through 7 are the reusable primitives (`index_game_dir`,
`build_scope_registry_arc`, `Prepared`/`validate_prepared`). Both front ends call
them directly, so the order can't drift the way it did before.

Step 8 is the one check a single file can't answer. Whether a
`should_be_used` type's instance (or a Stellaris technology) is ever referenced
depends on every other file, so step 7 records each `<type>` reference the rule
engine resolves (`validate_prepared_tracking_uses`), the driver merges those into
one set, and `references::check_unused_instances` then flags each file's own
definitions that the set doesn't cover. The whole thing is skipped for a config
that marks no type `should_be_used`, which is every non-Stellaris config today.
The LSP runs the same shape against a store it keeps current: the workspace
scan records every file's uses and checks each file against the merged set,
and a per-file revalidation replaces its own file's entry, re-checks that
file, and sweeps the open files that define an instance whose used-status
changed. Same staleness contract as CW100: open files update on edit, closed
files catch up on the next scan.

### CLI vs LSP

`Session` (in `driver`) bundles the primitives into the CLI's batch model: load
everything from disk once into immutable-after-load state, then validate the whole
set (`validate_all`). One `Session` per CLI run.

`validate --file`/`--since` scope the report, not the load: the directory is still
indexed whole, because the cross-file checks need it. `validate_selected` then
skips validating the files outside the scope, but only while the CW239/CW231 use
pass is off — that pass judges each definition against the references from every
file, so a partial run there would invent unused-definition errors. The report
filter in the CLI is what makes a scoped run correct either way.

The LSP does NOT use `Session`. Its index is mutable and incremental (single files
are re-indexed on each edit behind an `RwLock`, with no whole-workspace re-parse),
which doesn't fit `Session`'s load-once ownership. Instead the LSP holds its own
workspace state and builds a `Prepared` from the same shared primitives per
validation. Same sequence, different ownership.

Both front ends read the same base-game cache, and both have to consume all of
it. `vanilla_cache::load` returns the type instances plus a `VanillaCacheAux`,
the very type the writer packs, and the LSP routes that payload in one place
(`Backend::stage_vanilla_payload`) which destructures it exhaustively. A field
added to the cache is then a compile error on the side that ignores it, rather
than a check that quietly answers differently in the editor: CW113 was dead in
the LSP for as long as it was because the load dropped `file_paths` (#283), and
`var_names` is still deliberately unrouted there (#306). The file index CW113
resolves against is built on both sides by `driver::build_file_index`, over the
workspace root and the base game together, and only when base-game data is
present. Half an index would flag every reference into vanilla content.

One capability is CLI-only today: `Session` loads the mod's
`common/inline_scripts` bodies so an `inline_script` call site can be validated
against what it pulls in (CW274 and the body's own diagnostics). The LSP keeps no
such registry yet and passes `inline_scripts: None`, which accepts a call site
without expanding it.

### Background reindex

A long-running session drifts: files deleted while the server had no watcher event,
a settings change that only lands on the next scan. To catch that, the LSP runs a
periodic quiet rescan (`background_reindex_loop` in `scan/reindex.rs`, spawned once
from `initialized`). It re-runs the same `validate_entire_workspace` the startup scan
does, but `quiet`: no loading bar, though diagnostics still publish, so an error
fixed outside the editor clears.

The loop is idle-gated. Each cycle re-reads the effective interval, sleeps it out,
then waits for the user to go idle before running (`should_run_background_pass`:
the initial index is ready, no scan already running, and at least
`backgroundReindexIdleSeconds` (default 15s) since the last activity).
`mark_activity` resets that idle clock on edits, completion, hover,
and navigation, so a background pass never competes with a request the user is
waiting on. The re-entrancy guard (`scan_in_progress`) means a background pass and a
foreground scan can't overlap; the loser skips.

The cadence is the `backgroundReindexIntervalMinutes` initializationOption (default
`0`, which disables the loop entirely: a client that never sends the option gets no
background passes). It is also live-updatable through `workspace/didChangeConfiguration`
(`config.rs`), so toggling the setting takes effect without a restart. The
`reindexWorkspace` executeCommand forces an immediate foreground rescan on demand;
it reports "already in progress" when it loses the guard instead of silently
no-oping. `CWTOOLS_REINDEX_INTERVAL_SECS` / `CWTOOLS_REINDEX_IDLE_SECS` override the
interval and idle window for tests.

## Per-game validators

Generic rule validation runs first (the `.cwt` engine in `validation/src/rule_core`).
Then `run_game_validators` (`validation/src/per_game/mod.rs`) adds:

- `common` checks (duplicate `unique` type keys, CW261),
- cross-game `structural` hints (empty `if`/`limit`, `NOT` misuse, redundant booleans), then
- a dispatch on `Game`: `stellaris` (full validators), `hoi4` (cleanup hints),
  and `_ =>` common-only for every other game, EU4 included.

The `_ =>` fallback is intentional: a game with no per-game module still gets the
common + structural checks. Scope and link behavior is config-driven, not hardcoded
per game. `scopes.cwt` and `links.cwt` load through `ScopeRegistry`
(`game/src/scope_registry.rs`), so the scope checks (CW104/105/106, CW243-245,
CW247, CW248, CW260) work for any game that ships those files.

## Adding a new game

A new `Game` variant touches these sites, in lockstep:

1. `game/src/constants.rs`: add the variant, its `Display` arm, a `from_str` arm,
   and a `scope_defs` arm (point it at a `*_SCOPES` table, or `&[]` for a
   config-only game like HOI4).
2. `game/src/scope_engine/links.rs`: add a `load_scope_links` arm (a hardcoded link
   fallback, or `{}` when the config supplies everything).
3. `game/src/scope_registry.rs`: no new arm. The registry is generic; it reads the
   game's data through `scope_defs()` and `load_scope_links()`, so steps 1 and 2 cover it.
4. `validation/src/per_game/mod.rs`: add a dispatch arm only if the game gets a
   dedicated validator module. Otherwise the `_ =>` default handles it.
5. `validation/src/per_game/structural.rs`: add a CW223 message arm only if the
   game's boolean operators differ from the default (HOI4 already overrides it).
6. `localization/src/commands.rs`: usually nothing. The `Lang` set is one global
   list shared by every game, not a per-game one. Add a variant (and its arms in
   `key_to_language`, `Lang::from_name`, `Display`) only if the new game ships a
   language cwtools doesn't already recognize.
7. `localization/src/scope_validation.rs`: add the variant to `game_to_engine`'s
   pass-through list (else loc scope checks fall back to lenient HOI4).
8. `lsp/src/paths.rs`: add the Steam install-folder name to `discover_vanilla_dir`.
9. Ship `scopes.cwt` and `links.cwt` in the game's `.cwt` config (a separate repo).

The compiler catches some of these for you. The `Game` matches in `constants.rs`
(`Display`, `scope_defs`) and `scope_engine/links.rs` (`load_scope_links`) have no
`_ =>`, so a new variant won't compile until you handle them. That is the safety
net. Do not add a catch-all to silence it. The remaining sites (`from_str`,
`game_to_engine`, `discover_vanilla_dir`, the per-game dispatch, the CW223
message) have deliberate fallbacks, so a new variant compiles and behaves as the
generic default until you add its arm.

## Adding an error code

There is no central registry to update. Three edits:

1. Add one `pub const CW###_NAME: ErrorCode = ...` in `error_codes/src/lib.rs`.
2. Reference it at the emit site (in `validation` or `localization`), usually via
   `ValidationError::from_code`.
3. Add a row to `docs/ERROR_CODES.md`.

## Module layouts (the split god-files)

The largest areas are directory modules, each a thin `mod`/`lib` over focused files:

- `validation/src/rule_core/`: the `.cwt` rule engine (`matching`, `children`,
  `leaf`, `alias`, `subtype_merge`, `suggest`, `mod`). The biggest of the set.
- `game/src/scope_engine/`: `engine` (`ScopeId`/`ScopeContext`/transitions) vs
  `links` (per-game hardcoded link tables), over `mod`.
- `lsp/src/completion/`: `request` (the `textDocument/completion` handler),
  `builders` (item construction), `filter` (ranking and subsequence prefilter),
  `snippets`, `scope_names`, `cwt` (completion inside the rules files
  themselves), `loc_keys` (the prefix-searchable view of the loc-key union),
  `resolve` (lazy `completionItem/resolve`), over `mod`.
- `lsp/src/code_action/`: `payload` (Diagnostic.data codec + document quickfix),
  `create_loc_key` (CW100), `fix_all` (`fixAllWorkspace`), `ignore_code` (the
  "Ignore CWxxx in this workspace" action), over `mod`.
- `lsp/src/scan/`: `workspace` (the full scan), `reindex` (the quiet background
  pass), `loc` (loc rebuild + display collection), `vanilla` (base-game indexing
  and its loc memo), `watched` (`didChangeWatchedFiles` coalescing), over a `mod`
  that keeps the shared guards, constants and stat-signature helpers.
- `lsp/src/navigation/`: `goto`, `references`, `rename`, `symbols`, `structure`
  (folding/selection), `use_sites` (the use-site scan) and `helpers` (the shared
  range/symbol utilities), over a thin `mod`.
- `lsp/src/state.rs`: `Backend`, `DocumentState` and the document store — the
  server-wide state `main` used to carry inline.
- `lsp/src/cursor.rs`: cursor resolution types and rule→hint mapping (shared by
  hover, goto, rename).
- `info/src/`: `position` (element-at-cursor), `references` (reverse type-ref
  index), `lib` (InfoService + FileInfo).
- `index/src/`: `type_index`, `path_match`, `collect`, `variables`, `dynamic_values`,
  `vanilla_cache`, behind a thin `lib.rs` that re-exports the public surface.

## Localisation subsystem

The loc system resolves `$KEY$` references in game script files to their translated
text (shown in hover tooltips) and validates that referenced keys actually exist
(CW100/CW122), plus the loc-file checks (CW225/234/259/268/275).

### Data flow

```
.yml files on disk
       |
       v
  LocService (parses .yml -> Vec<LocFile>)
       |
       |--> LocIndex (lowercased key sets, per-language)
       |         - exists_any(key): does this key exist in any language?
       |         - missing_synced_languages(key): which languages lack it?
       |
       |--> loc_text map (FxHashMap<Arc<str>, Vec<(Lang, text)>>)
                 - used by hover to show translations
                 - rebuilt on the workspace scan, then patched per loc edit
```

### Current implementation

All loc data lives in memory:

- **`LocService`**: owns every parsed `LocFile` (the full AST of every `.yml`
  file). Built from disk during the workspace scan. Dropped after the index is
  built to free memory (~2M entries on Millennium Dawn).
- **`LocIndex`**: lowercased key sets per language + a union set. Built from
  `LocService`, then the service is dropped. Answers existence queries for
  config validation.
- **`loc_text`**: `FxHashMap<Arc<str>, Vec<(Lang, String)>>` for hover display.
  Built from `LocService` before it's dropped. Rebuilt on the full workspace
  scan, then patched on every loc edit. The per-edit merge is lossy when two
  files share a key: it overwrites that key and the next scan puts the full
  picture back.
- **`loc_live_overlay`**: per-open-file key sets for incremental `$ref$`
  checks. Updated on every loc file edit so newly-added keys resolve
  immediately without a full rescan.
- **`loc_watched_overlay`**: the same key sets for loc files changed on disk
  that are not open. Unioned with `loc_live_overlay` at the `$ref$` sites.
  Survives a scan on purpose (the scan's disk reads can predate the watched
  change).

A SQLite-backed hover-text store was considered (to skip re-parsing unchanged files
and stream results) but is not planned. The in-memory maps are cheap enough to rebuild.

## Build and profiling

See `BUILD.md` for build instructions and `PROFILING.md` for runtime profiling.
