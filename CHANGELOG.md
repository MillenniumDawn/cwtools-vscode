### Unreleased

#### Engine

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

* Fail the client check when a host test is missing from every `vscode-test` label. (#880)

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
  blocking pool that holds only the files in flight. Rename still visits every
  loc file for `$key$` references and unconfigured languages, but streams
  them the same way. Listed definitions follow `localisation.languages`, the
  scope goto and hover already use, also for a file the watcher brings in
  between scans; a key the base game also defines falls back to that
  definition when the mod's file is deleted; and a definition whose file
  changed without a watcher event is found where it is now rather than
  dropped. Removing a file from the index is O(1). (#474)

#### Tooling

* The CLI now distinguishes usage, discovery, and empty-input failures from
  validation findings with their documented exit codes. (#493)
* Package input copying now removes stale files from `dist/extension` without
  deleting generated build artifacts. (#561)
* `fix` now warns when a supplied vanilla cache was built for another game.
  (#499)

#### Engine

* Ctrl+Click on `localization_key` in scripted localisation jumps to the loc
  entry even when the ruleset does not type that folder. HOI4 still points
  `type[scripted_loc]` at Stellaris's `common/scripted_loc`, so those files
  never matched and the key was not a loc ref. (#725)
* CodeLens reference counts for scripted effects (and other type-pattern aliases
  such as scripted triggers) now include `my_se = yes` call sites. Those uses are
  keys, not `field = <type>` values, so the reverse index never saw them and the
  lens stayed at 0. Call sites are cached as non-schema leaf keys and reclassified
  when the instance set changes or after a workspace scan, without walking ASTs
  or reading files on each lens resolve.

#### Extension

* Graph labels now repaint when the VS Code webview theme changes. (#585)

#### Extension

* Changing settings that require startup now prompts to reload the window (#504).

### 3.4.4

#### Tooling

* The release pipeline no longer races itself. Merging a release PR could leave
  a duplicate "Release x.y.z" PR open: the `Release PR` run for the push
  *before* the merge checked out a `main` that still had the Unreleased
  bullets and, by the time it looked for an open PR, the release had merged,
  so it opened a second one (#721 was that duplicate). The push step now
  yields when `main` moved since checkout, and the run with nothing to release
  closes any PR left on the branch. Separately, `Publish` serialized every run
  on one workflow-level group, and GitHub cancels the older pending run in a
  group, so a release queued behind a pre-release build was cancelled by the
  next push to `main` with no fix PR to show for it; only the three publish
  jobs serialize now, each on a group keyed on the channel. `Publish: GitHub`
  skips a release that already exists instead of deleting and recreating it on
  a re-run, and the hand-dispatched `publish-marketplace.yml` is gone — it
  published without `--pre-release`, so a pre-release tag would have gone out
  on the stable channel; dispatch `publish.yml` against the tag instead.

### 3.4.3

#### Tooling

* Publishing works again, and is now one `Publish` workflow instead of three.
  No stable release had ever published: `tag-release.yml` capped the reusable
  `release.yml` at `contents: read` while its publish job asked for
  `contents: write`, and a called workflow may only narrow the caller's token,
  so every run died at startup in under a second. Nothing had reached Open VSX
  either, from either channel — on the pre-release path the Marketplace step ran
  first in the same job and timed out on `/_apis/gallery` uploading all six
  VSIX files in one `vsce publish` call, which aborted the job before the Open
  VSX step. `pre-release.yml`, `release.yml`, and `tag-release.yml` are replaced
  by `publish.yml`: `check` picks the channel once, `verify` fails the run
  before the 45-minute matrix when a release is missing a publish token, and
  the Marketplace, Open VSX, and GitHub Releases each publish as their own job
  from one packaged artifact, so a failure names itself and re-running one job
  re-publishes one target. The Marketplace publish now uploads one VSIX per
  call with `--skip-duplicate` and retries, a release commit no longer also
  publishes a pre-release, the Git tag is created by the GitHub publish job (so
  a failed release is retried by the next push to `main`), and a failed release
  opens a draft `fix/release-v<x.y.z>` pull request naming the jobs that broke.
* Guard setup failures now explain the expected input checkouts and overrides.
  Validation also times out with the retained log tail, and vanilla guard pins are
  checked for clean revisions. (#515)

### 3.4.2

#### Engine

* LSP command progress coverage now waits for the startup scan's progress stream
  to close before issuing a re-index command. (#690)
* Reading an open document's text no longer copies the buffer. The LSP requests
  that fire at cursor-movement and scroll cadence — code actions, inlay hints,
  code lenses, document links, highlights, folding and selection ranges,
  formatting and semantic tokens — each took a full copy of the file every time
  they ran, and the semantic-token full and delta requests took two, so a large
  focus-tree or events file paid a document-sized allocation per request. They
  now share the open buffer. (#473)

#### Tooling

* Regression tests cover failed fix writes without losing source bytes or
  counting failed edits, plus incompatible and malformed error-cache sidecars.
  (#520, #693)
* Host tests require the activation API, CWTools command IDs, expected graph
  nodes and edges after creation and restoration, and nonempty, correct hover
  content within the latency limit. (#693, #717)

### 3.4.1

#### Tooling

* Publishing is now automatic on two channels. Every push to `main` publishes a
  pre-release to the Marketplace, Open VSX, and GitHub Releases, versioned on
  the odd minor above stable with the run number as the patch; releases are cut
  by merging an auto-maintained release PR, which promotes the changelog's
  `### Unreleased` section and moves the manifest, after which the merge is
  tagged and published. The nightly workflow is replaced by the pre-release one.
  (#696)
* Guard baselines now warn when corpus, rules, or vanilla inputs drift from
  their recorded revisions. (#611)
* The release PR is now opened by a GitHub App rather than a personal access
  token, so no individual is its author and its checks still run on the merge
  commit. (#710)

#### Extension

* Graph webviews now block remote images and form submissions without breaking
  graph rendering. (#602)

#### Engine

* The LSP's string table no longer grows with every keystroke. Mid-edit parses —
  both the debounced document validation and the speculative re-parse behind
  hover, goto, completion, semantic tokens and inlay hints — now intern strings
  the table has never seen into a per-document region that is released with the
  document, instead of appending to a table that had no removal at all. Inline
  script `$ARG$` substitutions follow the document's region too. Strings already
  in the base table still resolve to their existing ids, so ids stay comparable
  across documents; `doc_tokens` is now keyed by case-folded content hash rather
  than by interned id, which also fixes a stale diagnostic when a name first seen
  mid-edit was later defined elsewhere. (#475)
* LSP hover localisation text is escaped before entering Markdown, so mod values
  render as literal text. (#592)
* CLI `fix --apply` and `format --apply` now write atomically and fail on
  unreadable files. (#496)
* Show graph now discards superseded requests, so an older response cannot
  replace the graph selected by the user. (#588)

#### Engine

* Type dispatch now honors `starts_with` and `type_key_prefix` for validation
  and navigation. (#581)
* Bare-value lists now stay inline when their rendered width fits, and wrap at
  the configured width otherwise. CLI `--max-line-width` and the
  `cwtools.formatting.maxLineWidth` setting control that width. (#554)
* Cargo dependency advisory checks now deny yanked crates. (#594)
* `documentSymbol` and the other LSP position handlers now resolve a document's
  columns entirely through the one line index the request already builds, and no
  longer rescan the document per node, per cursor lookup, or per workspace file.
  A `document_symbol` criterion bench over a ~1 MB script file covers the
  handler in ASCII and mixed-encoding variants. (#471)
* Request handlers and document notifications now run off tower-lsp's message
  pump, which polls them on the same task that reads stdin and writes stdout. A
  long request no longer stops `didChange`, diagnostic publishes or anything
  else from being handled, and `$/cancelRequest` and
  `window/workDoneProgress/cancel` now reach a scan that is already running
  instead of waiting for it to finish. Notifications keep the order the client
  sent them, on a single worker. (#470)
* Code lens resolution no longer reads referencing files from disk. The
  reference index and the open-document walk now record where a referenced name
  starts, not just the enclosing key, so a lens answers from indexed data
  instead of re-reading every referencing file to recover the value column. The
  open-document walk also runs on a snapshot rather than holding the document
  store, the ruleset and the config for its duration, and no longer allocates a
  string per leaf. (#472)

#### Extension

* VSIX smoke tests now require a universal fallback package and fail when
  packaging produces no VSIX. (#560)
* Start the language server only when an opened workspace folder has a root
  `descriptor.mod`. Unrelated folders and nested test fixtures no longer
  trigger startup. (#655)
* Marketplace publishing now requires the locally installed `vsce` CLI instead
  of allowing `npx` to download it. (#596)

### 3.4.0

#### Extension

* A crashed language server is now restarted through the existing restart
  budget instead of being stopped permanently on the first broken pipe. (#675)
* The workspace-command gating host tests wait for the initial scan instead of
  racing server startup, and report the notification text and the client's
  state when one does fail. (#675)
* The source manifest now tracks the latest release version and tagged builds.
  (#512)
* Graph, workspace fix, and workspace format commands now clear their
  availability when the language server stops and explain how to restart it.
  (#506)
* `cwtools.rules_folder` is now application-scoped, so workspace settings
  cannot redirect the rules folder. (#539)

#### Engine

* Server-to-client requests now outlive the task that issued them, so a
  debounced validation aborted by the next keystroke no longer panics the
  server with `receiver already dropped` when the editor answers. (#675)
* LSP authorization no longer lets a rules directory above a workspace
  authorize files outside that workspace. (#539)
* Platform packaging now rejects a flat server binary when staging a universal
  VSIX. (#549)
* CLI validation now rejects a missing `--vanilla` directory instead of loading
  an empty base-game index. (#490)
* The CLI restores Unix's default SIGPIPE handling, so closed output pipes no
  longer cause a broken-pipe panic. (#498)
* Whole-file formatting now replaces saturated columns through the end of
  long final lines. (#553)
* LSP: removing a multi-root primary promotes and rescans the first
  surviving root; removing the final root still clears workspace state.
  (#661)
* LSP: formatWorkspace to a client that advertises
  `workspace.workspaceEdit.documentChanges` now has integration coverage
  asserting the open file's exact version, `null` for closed files, and no
  legacy `changes` map. (#663)
* Graph webview file navigation is pinned by tests: relative paths are refused,
  outside-root paths wait for confirmation, and 1-based graph positions reveal
  the clamped 0-based range. (#664)
* Host tests now drive the registered workspace command handlers and the
  language client's executeCommand middleware against a fake client, pinning
  the exact requests, capability gates, cancellation, success, and failure
  behavior, and verify formatWorkspace returns through a real
  workspace/applyEdit request. (#665)
* Strict path matching (`path_strict`) now distinguishes the documented
  `dlc/<id>/<pattern>` shape from unrelated relative parents, while absolute
  logical paths keep the suffix fallback. (#666)
* The rules feature test now asserts bounded variable/value field parsing
  through `ast_to_ruleset` with mandatory matches, covering every bounded form
  in `field_parser` (variable/value/scope-marker/int/float) and exact
  `is_int`/`is_32bit`/`min`/`max` values including `-inf`/`inf`, zero,
  negative, and off-by-one neighboring finite bounds. (#667)

#### Coverage

* Rust coverage includes the language server, enforces the 91.5% line floor,
  and removes stale summaries on failed runs. (#662)
* Node and extension-host coverage now enforce per-metric floors to catch
  regressions. (#526)
* Node coverage now reports `documentLanguage.ts`, while extension-host
  coverage drops it to keep the reports disjoint. (#642)

### 3.3.0

#### Extension

* `publish-prebuilt` now refuses to delete an existing GitHub release unless
  the run is a tag push. (#508)
* Remaining GitHub workflows now set `persist-credentials: false` on every
  `actions/checkout`. (#574)
* Release, marketplace, CodeQL, and build-bench checkouts now set
  `persist-credentials: false`. (#574)
* Opening a missing or invalid file from the files trename still visits every
  loc file for `$key$` references and unconfigured languages, but streams
  them the same way. Listed definitions follow `localisation.languages`, the
  scope goto and hover already use, also for a file the watcher brings in
  between scans; a key the base game also defines falls back to that
  definition when the mod's file is deleted; and a definition whose file
  changed without a watcher event is found where it is now rather than
  dropped. Removing a file from the index is O(1). (#474)

#### Tooling

* The CLI now distinguishes usage, discovery, and empty-input failures from
  validation findings with their documented exit codes. (#493)
* Package input copying now removes stale files from `dist/extension` without
  deleting generated build artifacts. (#561)
* `fix` now warns when a supplied vanilla cache was built for another game.
  (#499)

#### Engine

* Ctrl+Click on `localization_key` in scripted localisation jumps to the loc
  entry even when the ruleset does not type that folder. HOI4 still points
  `type[scripted_loc]` at Stellaris's `common/scripted_loc`, so those files
  never matched and the key was not a loc ref. (#725)
* CodeLens reference counts for scripted effects (and other type-pattern aliases
  such as scripted triggers) now include `my_se = yes` call sites. Those uses are
  keys, not `field = <type>` values, so the reverse index never saw them and the
  lens stayed at 0. Call sites are cached as non-schema leaf keys and reclassified
  when the instance set changes or after a workspace scan, without walking ASTs
  or reading files on each lens resolve.

#### Extension

* Graph labels now repaint when the VS Code webview theme changes. (#585)

#### Extension

* Changing settings that require startup now prompts to reload the window (#504).

### 3.4.4

#### Tooling

* The release pipeline no longer races itself. Merging a release PR could leave
  a duplicate "Release x.y.z" PR open: the `Release PR` run for the push
  *before* the merge checked out a `main` that still had the Unreleased
  bullets and, by the time it looked for an open PR, the release had merged,
  so it opened a second one (#721 was that duplicate). The push step now
  yields when `main` moved since checkout, and the run with nothing to release
  closes any PR left on the branch. Separately, `Publish` serialized every run
  on one workflow-level group, and GitHub cancels the older pending run in a
  group, so a release queued behind a pre-release build was cancelled by the
  next push to `main` with no fix PR to show for it; only the three publish
  jobs serialize now, each on a group keyed on the channel. `Publish: GitHub`
  skips a release that already exists instead of deleting and recreating it on
  a re-run, and the hand-dispatched `publish-marketplace.yml` is gone — it
  published without `--pre-release`, so a pre-release tag would have gone out
  on the stable channel; dispatch `publish.yml` against the tag instead.

### 3.4.3

#### Tooling

* Publishing works again, and is now one `Publish` workflow instead of three.
  No stable release had ever published: `tag-release.yml` capped the reusable
  `release.yml` at `contents: read` while its publish job asked for
  `contents: write`, and a called workflow may only narrow the caller's token,
  so every run died at startup in under a second. Nothing had reached Open VSX
  either, from either channel — on the pre-release path the Marketplace step ran
  first in the same job and timed out on `/_apis/gallery` uploading all six
  VSIX files in one `vsce publish` call, which aborted the job before the Open
  VSX step. `pre-release.yml`, `release.yml`, and `tag-release.yml` are replaced
  by `publish.yml`: `check` picks the channel once, `verify` fails the run
  before the 45-minute matrix when a release is missing a publish token, and
  the Marketplace, Open VSX, and GitHub Releases each publish as their own job
  from one packaged artifact, so a failure names itself and re-running one job
  re-publishes one target. The Marketplace publish now uploads one VSIX per
  call with `--skip-duplicate` and retries, a release commit no longer also
  publishes a pre-release, the Git tag is created by the GitHub publish job (so
  a failed release is retried by the next push to `main`), and a failed release
  opens a draft `fix/release-v<x.y.z>` pull request naming the jobs that broke.
* Guard setup failures now explain the expected input checkouts and overrides.
  Validation also times out with the retained log tail, and vanilla guard pins are
  checked for clean revisions. (#515)

### 3.4.2

#### Engine

* LSP command progress coverage now waits for the startup scan's progress stream
  to close before issuing a re-index command. (#690)
* Reading an open document's text no longer copies the buffer. The LSP requests
  that fire at cursor-movement and scroll cadence — code actions, inlay hints,
  code lenses, document links, highlights, folding and selection ranges,
  formatting and semantic tokens — each took a full copy of the file every time
  they ran, and the semantic-token full and delta requests took two, so a large
  focus-tree or events file paid a document-sized allocation per request. They
  now share the open buffer. (#473)

#### Tooling

* Regression tests cover failed fix writes without losing source bytes or
  counting failed edits, plus incompatible and malformed error-cache sidecars.
  (#520, #693)
* Host tests require the activation API, CWTools command IDs, expected graph
  nodes and edges after creation and restoration, and nonempty, correct hover
  content within the latency limit. (#693, #717)

### 3.4.1

#### Tooling

* Publishing is now automatic on two channels. Every push to `main` publishes a
  pre-release to the Marketplace, Open VSX, and GitHub Releases, versioned on
  the odd minor above stable with the run number as the patch; releases are cut
  by merging an auto-maintained release PR, which promotes the changelog's
  `### Unreleased` section and moves the manifest, after which the merge is
  tagged and published. The nightly workflow is replaced by the pre-release one.
  (#696)
* Guard baselines now warn when corpus, rules, or vanilla inputs drift from
  their recorded revisions. (#611)
* The release PR is now opened by a GitHub App rather than a personal access
  token, so no individual is its author and its checks still run on the merge
  commit. (#710)

#### Extension

* Graph webviews now block remote images and form submissions without breaking
  graph rendering. (#602)

#### Engine

* The LSP's string table no longer grows with every keystroke. Mid-edit parses —
  both the debounced document validation and the speculative re-parse behind
  hover, goto, completion, semantic tokens and inlay hints — now intern strings
  the table has never seen into a per-document region that is released with the
  document, instead of appending to a table that had no removal at all. Inline
  script `$ARG$` substitutions follow the document's region too. Strings already
  in the base table still resolve to their existing ids, so ids stay comparable
  across documents; `doc_tokens` is now keyed by case-folded content hash rather
  than by interned id, which also fixes a stale diagnostic when a name first seen
  mid-edit was later defined elsewhere. (#475)
* LSP hover localisation text is escaped before entering Markdown, so mod values
  render as literal text. (#592)
* CLI `fix --apply` and `format --apply` now write atomically and fail on
  unreadable files. (#496)
* Show graph now discards superseded requests, so an older response cannot
  replace the graph selected by the user. (#588)

#### Engine

* Type dispatch now honors `starts_with` and `type_key_prefix` for validation
  and navigation. (#581)
* Bare-value lists now stay inline when their rendered width fits, and wrap at
  the configured width otherwise. CLI `--max-line-width` and the
  `cwtools.formatting.maxLineWidth` setting control that width. (#554)
* Cargo dependency advisory checks now deny yanked crates. (#594)
* `documentSymbol` and the other LSP position handlers now resolve a document's
  columns entirely through the one line index the request already builds, and no
  longer rescan the document per node, per cursor lookup, or per workspace file.
  A `document_symbol` criterion bench over a ~1 MB script file covers the
  handler in ASCII and mixed-encoding variants. (#471)
* Request handlers and document notifications now run off tower-lsp's message
  pump, which polls them on the same task that reads stdin and writes stdout. A
  long request no longer stops `didChange`, diagnostic publishes or anything
  else from being handled, and `$/cancelRequest` and
  `window/workDoneProgress/cancel` now reach a scan that is already running
  instead of waiting for it to finish. Notifications keep the order the client
  sent them, on a single worker. (#470)
* Code lens resolution no longer reads referencing files from disk. The
  reference index and the open-document walk now record where a referenced name
  starts, not just the enclosing key, so a lens answers from indexed data
  instead of re-reading every referencing file to recover the value column. The
  open-document walk also runs on a snapshot rather than holding the document
  store, the ruleset and the config for its duration, and no longer allocates a
  string per leaf. (#472)

#### Extension

* VSIX smoke tests now require a universal fallback package and fail when
  packaging produces no VSIX. (#560)
* Start the language server only when an opened workspace folder has a root
  `descriptor.mod`. Unrelated folders and nested test fixtures no longer
  trigger startup. (#655)
* Marketplace publishing now requires the locally installed `vsce` CLI instead
  of allowing `npx` to download it. (#596)

### 3.4.0

#### Extension

* A crashed language server is now restarted through the existing restart
  budget instead of being stopped permanently on the first broken pipe. (#675)
* The workspace-command gating host tests wait for the initial scan instead of
  racing server startup, and report the notification text and the client's
  state when one does fail. (#675)
* The source manifest now tracks the latest release version and tagged builds.
  (#512)
* Graph, workspace fix, and workspace format commands now clear their
  availability when the language server stops and explain how to restart it.
  (#506)
* `cwtools.rules_folder` is now application-scoped, so workspace settings
  cannot redirect the rules folder. (#539)

#### Engine

* Server-to-client requests now outlive the task that issued them, so a
  debounced validation aborted by the next keystroke no longer panics the
  server with `receiver already dropped` when the editor answers. (#675)
* LSP authorization no longer lets a rules directory above a workspace
  authorize files outside that workspace. (#539)
* Platform packaging now rejects a flat server binary when staging a universal
  VSIX. (#549)
* CLI validation now rejects a missing `--vanilla` directory instead of loading
  an empty base-game index. (#490)
* The CLI restores Unix's default SIGPIPE handling, so closed output pipes no
  longer cause a broken-pipe panic. (#498)
* Whole-file formatting now replaces saturated columns through the end of
  long final lines. (#553)
* LSP: removing a multi-root primary promotes and rescans the first
  surviving root; removing the final root still clears workspace state.
  (#661)
* LSP: formatWorkspace to a client that advertises
  `workspace.workspaceEdit.documentChanges` now has integration coverage
  asserting the open file's exact version, `null` for closed files, and no
  legacy `changes` map. (#663)
* Graph webview file navigation is pinned by tests: relative paths are refused,
  outside-root paths wait for confirmation, and 1-based graph positions reveal
  the clamped 0-based range. (#664)
* Host tests now drive the registered workspace command handlers and the
  language client's executeCommand middleware against a fake client, pinning
  the exact requests, capability gates, cancellation, success, and failure
  behavior, and verify formatWorkspace returns through a real
  workspace/applyEdit request. (#665)
* Strict path matching (`path_strict`) now distinguishes the documented
  `dlc/<id>/<pattern>` shape from unrelated relative parents, while absolute
  logical paths keep the suffix fallback. (#666)
* The rules feature test now asserts bounded variable/value field parsing
  through `ast_to_ruleset` with mandatory matches, covering every bounded form
  in `field_parser` (variable/value/scope-marker/int/float) and exact
  `is_int`/`is_32bit`/`min`/`max` values including `-inf`/`inf`, zero,
  negative, and off-by-one neighboring finite bounds. (#667)

#### Coverage

* Rust coverage includes the language server, enforces the 91.5% line floor,
  and removes stale summaries on failed runs. (#662)
* Node and extension-host coverage now enforce per-metric floors to catch
  regressions. (#526)
* Node coverage now reports `documentLanguage.ts`, while extension-host
  coverage drops it to keep the reports disjoint. (#642)

### 3.3.0

#### Extension

* `publish-prebuilt` now refuses to delete an existing GitHub release unless
  the run is a tag push. (#508)
* Remaining GitHub workflows now set `persist-credentials: false` on every
  `actions/checkout`. (#574)
* Release, marketplace, CodeQL, and build-bench checkouts now set
  `persist-credentials: false`. (#574)
* Opening a missing or invalid file from the files tree now shows an error that
  names the path. (#589)
* Release bundles compile the `CWTOOLS_TEST_*` rules-fetch env overrides out of
  the extension, and a git ref that begins with `-` is rejected. (#571)
* Host tests now verify diagnostics reach the editor, including diagnostic codes,
  ranges, and a clean-file case. (#517)
* The weekly rules-pin workflow runs npm and tests with read-only contents
  permissions before a separate write-enabled PR job. (#542)
* Language server restarts now re-read live settings during initialization. (#566)
* The diagnostics signature cache now retains a full 2,000-file workspace publish
  pass, avoiding repeat diagnostics. (#567)
* Development webview bundles now expose a development `process.env.NODE_ENV`.
  (#583)
* Issue templates now request reproduction steps, environment details, rules
  revision, and server logs for extension bugs, while maintenance issues use
  the `Task` type. (#629)
* Game detection no longer exposes an unused vanilla-folder flag. (#631)
* The extension exports `deactivate()`, which stops the language client. VS Code
  awaits that, unlike the disposal of `context.subscriptions`, so the LSP
  shutdown/exit handshake now finishes and the server exits on its own instead
  of being left to die with the extension host. (#502)
* A daily and manually dispatchable nightly workflow publishes smoke-tested
  Linux, macOS, Windows, and universal VSIX prereleases from `main` without
  using Marketplace or Open VSX credentials.
* Show graph reports when the active file has no graph. (#568)
* Graph overlays keep rendering for malformed entity types with empty segments
  or no abbreviation. (#551)
* Release workflow scopes Marketplace PATs to publishing steps instead of the
  whole job. (#543)
* Pins the `cytoscape-elk` Git dependency to a full commit SHA. (#545)
* EU5 workspaces in Europa Universalis V paths now detect as eu5 instead of eu4. (#552)
* The shared Cargo registry cache now restores the newest same-OS cache when
  the current `Cargo.lock` hash misses, avoiding a full dependency download.
  (#511)
* `npm run build -- release` now refuses untracked files and a `HEAD` that is
  not present on `origin/main`, naming the paths or commit before tagging.
  (#514)
* CI now runs the network-free `rules-sync` host label after staging the server;
  the weekly rules-pin gate builds that server and runs the same suite only when
  a pin changes. (#521)
* The workspace manifest CI gate now has focused tests for its checks and
  setup failure paths using test-owned files and mocked tools. (#632)
* VS Code cache keys now use the stable version only when the upstream response
  has a numeric semver; malformed responses use the existing unknown fallback.
  (#601)
* `scripts/guard.py