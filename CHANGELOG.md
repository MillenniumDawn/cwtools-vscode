### Unreleased

#### Engine

* Release string-table read locks before building cached AST records. (#494)

* Report syntax, directive and reference diagnostics for a single `--rules` file using the same loader as rules directories, retaining recovered rules. (#863)
* Remove redundant scope-resolver borrows so the engine passes Rust 1.99 Clippy without changing scope resolution.
* Reclaim removed localisation URI references and recycle file records after sweeping their old sites, bounding metadata growth across repeated refreshes. (#846)
* Avoid interning speculative parser keys, removing discarded numeric and boolean entries from bare clause values. (#547)
* Return `RequestFailed` instead of `ServerNotInitialized` for navigation and rename refusals. (#828)
* Hover and go-to-definition on a variable look its definitions up in a name-keyed index instead of scanning every file's defined variables under the info lock. (#872)
* Test fixtures stop assuming Unix paths: the position-resolver test writes its rules into a `tempfile` directory instead of a hard-coded `/tmp`, and every report test builds its repository root through the existing `abs()` helper. (#885)
* The per-edit ignored-file check reads the default exclude patterns from a shared static instead of building a whole `FileManagerConfig` per call. (#873)
* Cap cumulative text snapshots for references and rename requests, refusing incomplete navigation when the request budget is exhausted. The conservative 5× peak-memory reservation means a closed UTF-8 file larger than 25.6 MiB cannot fit within the 128 MiB request budget. (#569)
* Reuse formatted text when constructing whole-file format edits, avoiding reparsing and the multi-edit planner. (#557)
* Memoize subtype merges per file and precompute matcher key groups. (#492)
* Harden cache corruption handling, pin the `.cwb` archive layout with a golden fixture, and benchmark cache serialization and loading. (#520)
* A submod can name its parent mods (`parentMods` init option, in load order). The server indexes them between the base game and the workspace with base-game provenance, so their scripted effects, triggers, variables, ideas, flags and localisation resolve and complete, and they never count toward CW261. A workspace file at the same path, or under a `replace_path` in the workspace's `descriptor.mod`, shadows the parent's copy. Parent files are never validated or diagnosed, and are readable but not editable through the server. (#786)
* Report syntax errors in `.cwt` rules files (an unclosed clause or quote) when loading a rules directory, as the new CW604. They reach `cwtools rules`, `validate` and the editor at startup, not only once the file is opened, and the live lint now reports them with the same code. The rules the parser recovered still load. (#847)
* Alias `value[...]` patterns now resolve regardless of ASCII case, like the `enum[...]` arm, so `political_power` on rules side matches any casing of a value-set member. (#483)

#### Extension

* Measure watched-file exclusions from configured parent, vanilla and rules roots as well as the workspace (the rules root for `.cwt` files only), refreshing roots on restart or rules-folder changes. Add a real create/change/delete watcher regression. (#862)
* When the client drops watched-file events, skipped directory names (`.git`, `.claude`, `target`, `dist`, `out` and the rest) no longer count in the folders above the served workspace root. A mod checked out under a folder with one of those names, such as `.claude/worktrees/<mod>`, used to index at startup but ignore later external edits, checkouts and deletes. (#835)
* Report graph image export failures in the webview instead of leaving rejected promises unhandled, and allow a later export to succeed. (#834)
* Discard delayed plaintext language upgrades after editor focus changes or teardown, before sending stale focus notifications. (#837)
* Exercise automatic server crash/restart recovery in real extension-host tests, including all command contexts and a request to the replacement process. (#838)
* Extend the graph unit tests to cover exportImage's data-URI strip and the panel's saveImage and saveJson writes to the chosen file. (#523)
* The loaded-files tree is built on a `Map`, so a folder or file named `constructor`, `toString` or `__proto__` shows up instead of vanishing, integer-like names keep the server's order, and "Reveal active file" is one lookup in a URI index instead of a `Uri.parse` per node. (#876)
* Pin the client-required server executeCommand names in a shared contract and verify the live server advertises each one. (#522)
* Classify the shipped graph webview libraries as runtime dependencies and make dependency review fail for both runtime and development scopes. (#572)
* Add the `cwtools.parentMods` setting: the parent mods of a submod workspace, in load order. The docs replace the multi-root advice, which never gave a second folder's definitions to the first. (#786)
* Rules in the resolved rules folder now reload automatically after `.cwt` edits, and changing `cwtools.rules_folder` to a valid folder applies without restarting the window. Set `cwtools.rules.autoReload` to `false` to keep manual reloads. (#605)
* A late file type reply no longer marks the wrong editor, or no editor, as a graph file. Closing the last editor or switching tabs clears the graph state at once, a queued switch is dropped once a newer one supersedes it, and the pending tab-switch timer is cleared on shutdown. (#837)
* The graph, Fix All and Format Workspace commands now come back after the language server crashes and restarts on its own. They stayed hidden until a manual restart. (#838)
* The Set graph depth prompt now accepts only a whole number of at least 1. Blank, zero, negative, fractional and unsafe values are rejected with a message naming the minimum, so they no longer replace the remembered depth or send a request the server refuses. (#841)

#### Tooling

* Host coverage now launches in a dedicated POSIX process group and terminates descendants on timeout or interruption, including children whose parent exits on SIGTERM. A SIGTERM or SIGHUP sent to the script itself gets the same cleanup, and a second signal during it no longer stops it before the kill step. (#851)
* `npm run bench:node` measures again: the client hot-path benchmark registered its three benches under Vitest 5 without running them, so it passed in a few hundred milliseconds with no results. Each bench is now run and its result asserted. (#878)
* The Open VSX publish job replaces `HaaLeo/publish-vscode-extension` (stuck on a Node 20 target the runner force-upgrades with a deprecation warning) with `ovsx publish` from the dev dependencies through build.py, so the upload retries like the Marketplace publish does. The App token steps pass `client-id` instead of the deprecated `app-id`, with the App ID secret kept as the fallback. Both registry tokens now reach only their publish step, not the `npm ci` before it. (#812)
* The release PR body lists every pull request merged since the last stable tag, as the Utility Tool's release PR does. Dependabot bumps are left out. (#794)
* The VSIX smoke test now requires a nonempty `cwtools-server.exe` in the `win-x64` directory and a nonempty `cwtools-server` in every other platform directory, as the extension's resolver does. Flat layouts check each executable name present. A directory holding only other files, an empty binary, or the wrong suffix fails the package gate. (#850)
* The VSIX smoke test rejects any file under `bin/server` that is not one of those executables, such as a stray `.pdb` in the flat directory or in a platform directory, even if earlier build checks are bypassed. It reports unexpected files and missing or empty executables in one run. (#852)

### 3.4.7

* Strengthen atomic-write, dispatch-routing, and activation regression tests. (#693)

#### Engine

* Scripted-localisation completion inside a defined_text block now offers the name/text fields instead of dumping workspace variables; variables are still offered inside text. (#785)
* Keyed cardinality checks use a map once a block's rule list is wide, instead
  of scanning every rule for every field. (#582)
* Consolidate client capability negotiation and remove redundant UTF-16 test
  wrappers. (#765)

#### Tooling

* guard.py learned --compare [REV] (default: merge-base with main), which builds the CLI from a temporary worktree at that revision and diffs the current validation against it on the same corpus and rules, so input drift cancels out. (#607)
* Add a scheduled, dry-run-by-default retention policy that keeps the newest
  30 automated `v*-pre.*` GitHub releases for roughly a week of rollback and
  cleans up their tags. This tracks the current Publish workflow shape; the
  superseded `v*-nightly.*` shape is left untouched. (#622)
* Fuzz the `.cwb`, `.cwe`, and `.cwv` cache readers in the CI smoke job. (#627)
* The PR build now smoke-tests the packaged VSIX (required files present; test corpora, build output, and source maps absent) and reports every missing expected platform instead of just the first; a lone flat server binary in the single-platform universal layout passes. (#524)

#### Extension

* Add the cwtools.enable setting (default true) so the extension can be switched off per workspace; when disabled, activation starts no server and registers no client or watchers. (#505)
* Keep graph commands gated by the live server capability and remove an unused
  webview export. (#765)

### 3.4.6

#### Tooling

* Update `qs` past the versions affected by the reported denial-of-service
  advisories.
* CI Cargo tools are pinned and each executable is cached by version separately
  from the Cargo registry. (#510)
* Test graph import errors and unavailable exports through the bundled webview.
  (#565)
* VSIX packaging now fails when staged server platforms are not in the release
  target list, keeping the release matrix and `VSIX_TARGETS` in sync. (#584)
* Cover flat and per-platform server staging and package target selection in
  build-tool tests. (#563)
* Reject VSIX files whose archived version differs from the release version
  before publishing to GitHub or the Marketplace. (#550)

#### Extension

* Multi-root activation now selects the first folder containing `descriptor.mod`
  for game detection, relative `rules_folder` resolution, and language-server
  scanning instead of authorizing an unrelated first folder. (#694)
* Malformed or incomplete graph JSON imports and exports before a graph loads
  now report errors instead of silently failing or throwing. (#565)
* Load the generated graph stylesheet for webview tooltips instead of keeping
  a copy of the dependency CSS. (#586)
* Add the live `cwtools.diagnostics.workspaceWide` setting (default `true`) to
  control publishing diagnostics for closed files, and offer **Show Output**
  the first time a scan reaches the closed-file diagnostics budget. (#608)

#### Engine

* Send one client notification when a workspace scan holds back closed-file
  diagnostics at the publication budget, and report held-back counts only when
  the closed-file budget is actually exceeded. (#608)
* Apply parser edits in one pass against the original text, skipping invalid or
  overlapping ranges instead of risking a replacement panic. (#577)
* Alias-branch budget errors inside expanded inline scripts now point to the
  caller and name the script line where validation stopped. (#579)
* Graph construction now bounds dense expansion to 4,000 edges and 64 use sites
  per expanded node, reports resulting omissions in graph details, and indexes
  per-file owner lookup. (#570)
* Graph use-site collection now bounds open-buffer and indexed matches before
  materializing graph results while preserving complete references and rename
  results. (#783)
* Type-keyed fields such as `<resource> = float` now reject unknown keys once
  the type index is complete, so a fake resource in state history is flagged.
  (#766)
* Share one token scanner for rename, references, and highlights while preserving
  case matching and source columns. (#764)
* Share the LSP's retry-until-scan-finishes loop while preserving command
  deadlines and cancellation behavior. (#760)
* Consolidate base-game state locking while preserving reload and localization
  fallback behavior. (#761)
* Share one parser quote-stripping helper across engine crates. (#763)
* Consolidate CLI config-field selection while preserving flag precedence and
  configuration announcements. (#759)
* Remove the deprecated workspace walker while retaining discovery coverage for sorting, ignore globs, symlinks, and file budgets. (#758)
* Delete the unused `RuleSetBuilder` and the orphaned `alias_exact_for` /
  `alias_category` accessors; callers use `alias_exact()` /
  `alias_categories()` directly. (#762)
* Requests now observe preceding document notifications without blocking later
  requests, while cancellation remains prompt. (#775)
* Formatter position lookups now share line-prefix scans, keeping long single-line
  value lists linear in their source length. (#702)

### 3.4.5

#### Extension

* The CWTools log channel is created only for enabled workspaces and now uses
  VS Code log levels. (#507)
* Graph restoration no longer overwrites a newer graph request after a delayed
  response. (#695)

#### Tooling

* Add the `/fix-issue` Claude Code skill (`.claude/skills/fix-issue`): it
  fetches a GitHub issue, gathers the context it touches, plans the fix,
  then implements, verifies, and opens the PR after approval.
* Host tests use the extension-host runner's TDD `suite`/`test` globals
  throughout; importing `describe`/`it` from the top-level mocha package broke
  the smoke suite because those helpers are not bound to the Mocha instance
  @vscode/test-cli runs. (#754)
* Client hot-path benchmarks now use Vitest 5's test-context benchmark API.
  (#737)
* Cargo-deny now allows BSD-3-Clause for the existing zstd dependency graph.
  (#743)
* `validate` now treats equivalent vanilla-cache game aliases as the same game
  while still warning for different and unknown identifiers. (#738)
* Restoring a graph panel now disposes the existing panel and its command
  handlers before wiring the replacement, so window reloads do not leave
  duplicate save commands behind. (#599)
* Marketplace publishing now passes the VSCE token through the child environment
  instead of command arguments. (#513)

#### Engine

* Hover, inlay titles and graph labels keep their localisation text per
  file, so renaming or deleting a key in an open loc file drops the old text
  on that edit instead of at the next full scan, editing one file no longer
  drops another file's (or the base game's) translation of the same key, a
  deleted loc file takes its text with it, and an edited file keeps its place
  among the other files' translations of a shared key, so an inlay or graph
  label does not flip to another file's text while typing. (#476)
* Parser source positions now account for a leading BOM, so line-1 fixes and
  formatting edits preserve the original file text. (#555)
* Vanilla cache instances are now accepted only from files inside the configured
  base-game directory. (#591)
* Bare values now keep their source range tight to the value, so comments and
  blank lines are preserved by formatting. (#546)
* Live vanilla indexing now preserves the real source URI for each base-game
  instance, matching cache-backed sessions. (#580)
* Modifier key validation now lazily lowercases ASCII keys while preserving Unicode matching. (#598)
* Watched-file batches, open-document revalidation and `didClose` now take a
  validation slot per file instead of per batch, so an edit made while a
  large batch runs (a `git checkout` touching many files) no longer waits for
  the whole batch before its diagnostics appear. (#477)

* Find-references and rename on a localisation key no longer read and parse
  the whole localisation tree, then every script file, on the pump task for
  each request (289 MB / 2 944 files per request on Millennium Dawn). The loc
  index now keeps every definition site per key, so references answer the
  definition half from memory, and usages come from a streamed scan on the
  blocking pool that holds only the files in flight. Rename still vi