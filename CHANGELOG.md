### Unreleased

#### Engine

* Server-to-client requests now outlive the task that issued them, so a
  debounced validation aborted by the next keystroke no longer panics the
  server with `receiver already dropped` when the editor answers. (#675)

#### Extension

* A crashed language server is now restarted through the existing restart
  budget instead of being stopped permanently on the first broken pipe. (#675)
* The workspace-command gating host tests wait for the initial scan instead of
  racing server startup, and report the notification text and the client's
  state when one does fail. (#675)

#### Extension

* `cwtools.rules_folder` is now application-scoped, so workspace settings
  cannot redirect the rules folder. (#539)

#### Engine

* LSP authorization no longer lets a rules directory above a workspace
  authorize files outside that workspace. (#539)
* Platform packaging now rejects a flat server binary when staging a universal
  VSIX. (#549)

#### Engine

* The CLI restores Unix's default SIGPIPE handling, so closed output pipes no
  longer cause a broken-pipe panic. (#498)

#### Engine

* Whole-file formatting now replaces saturated columns through the end of
  long final lines. (#553)

#### Engine

* LSP: removing a multi-root primary promotes and rescans the first
  surviving root; removing the final root still clears workspace state.
  (#661)

#### Coverage

* Rust coverage includes the language server, enforces the 91.5% line floor,
  and removes stale summaries on failed runs. (#662)
* Node and extension-host coverage now enforce per-metric floors to catch
  regressions. (#526)
* Node coverage now reports `documentLanguage.ts`, while extension-host
  coverage drops it to keep the reports disjoint. (#642)

#### Engine

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

#### Engine

* Strict path matching (`path_strict`) now distinguishes the documented
  `dlc/<id>/<pattern>` shape from unrelated relative parents, while absolute
  logical paths keep the suffix fallback. (#666)

#### Engine

* The rules feature test now asserts bounded variable/value field parsing
  through `ast_to_ruleset` with mandatory matches, covering every bounded form
  in `field_parser` (variable/value/scope-marker/int/float) and exact
  `is_int`/`is_32bit`/`min`/`max` values including `-inf`/`inf`, zero,
  negative, and off-by-one neighboring finite bounds. (#667)

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
* `scripts/guard.py` has end-to-end tests for clean, drift, bless, and setup
  failure paths using a stub validator and test-owned files. (#516)
* Host tests now monitor the running extension's LSP output, reject forbidden
  completions individually, and verify dotted Paradox identifiers use one word
  range. ESLint blocks runtime test imports from the extension entry point. (#518)
* `cwtools.restartServer` now gives the same EPERM/EACCES guidance as
  activation: if a restart fails because antivirus re-quarantined the server
  binary, the error dialog offers Reveal Server Binary and Antivirus Help
  instead of a raw error toast. The dialog is shared code
  (`serverBlockedDialog.ts`) so both failure paths stay in sync. (#455)
* A persistent status bar item shows the language server's state (starting,
  scanning n%, ready, stopped) even when idle. Clicking it offers Restart
  Server and Show Output. `cwtools.restartServer` stops and restarts the
  client, re-running the initial scan; `cwtools.showOutput` reveals the
  CWTools output channel. If the client's error handler gives up restarting
  after repeated crashes, the item says stopped instead of going quiet. (#440)
* Format Document and Format Selection work on Paradox script. Workspace-wide
  format is a palette command gated on the server advertising `formatWorkspace`.
  New `cwtools.formatting` settings cover indent style/size, trailing
  whitespace, and a final newline. The `[paradox]` editor default is 4 spaces
  so it matches the formatter. (#439, #441)
* Node coverage now reports `watchedFiles.ts`, `graphAvailability.ts`, and
  `trustedPaths.ts` next to the vitest suites that own them. The host report
  drops those three so the two numbers stay disjoint. (#446)
* `test:coverage` runs the `host` label instead of `unit`, so it measures
  modules like `graphPanel.ts` that only `extension.test.ts` exercises
  instead of reporting them at a misleadingly low, accidental number.
  `HOST_COVERAGE_DROPS` now matches `vitest.config.ts`'s `coverage.include`
  exactly, so vitest-owned modules (e.g. `commandProgress.ts`) stay out of
  the host report instead of leaking in with a stale, partial figure. (#220)
* `docs/CONTRIBUTING.md`'s coverage paragraph named the wrong label and called
  coverage a non-gate; it now says `test:coverage` runs `host`, and that CI's
  `Host coverage` step has no continue-on-error even though no percentage
  threshold gates a merge. (#448)
* The `pins` job's inline bash for reading and resolving the guard baseline's
  revision pins moved to `scripts/resolve_pins.py`, tested under
  `tests/scripts/`. (#381)
* `test:watch` reruns from compiled client output instead of repository
  bookkeeping, and test hosts disable Electron's crash reporter so interrupted
  sessions leave no detached Crashpad process. (#422)
* `tests/scripts/test_hosttest.py` now guards the real `hosttest.TEST_CLI` path
  when `node_modules/@vscode/test-cli` is installed, skipping cleanly when it
  is not. (#423)
* The host hover test for quoted `value[…]` localisation keys runs again. The
  in-repo engine already resolves those values through their unquoted
  localisation keys. (#219)
* The skipped `pop_faction_flag` completion test runs again. It was diagnosed
  as a per-file value set, but the index is workspace-wide; the real cause was
  requesting completion mid-token, which the subsequence filter narrows down
  to a same-file match. The test now uses an empty-token position and asserts
  a cross-file flag comes back too, backed by a new engine regression test
  that merges flags set in two different files. (#237)
* The Python helpers under `scripts/` get a real toolchain. `pyproject.toml` holds the ruff, black, pylint, mypy and pytest config, `requirements-dev.txt` pins the versions CI and the pre-commit hooks install, and `mypy.ini` is gone. ruff runs a wider rule set, mypy runs strict, and pylint is new and at 10.00. The suites in `tests/scripts/` are pytest-native rather than `unittest.TestCase`, and CI runs them with pytest in a `Python lint & tests` job of its own instead of `unittest discover` inside the Rust lint job. (#387)
* Extension-host tests no longer open a VS Code window on the developer's desktop. Every label runs through `scripts/build/hosttest.py`, which uses `xvfb-run` on Linux and fails with install instructions when it is missing rather than falling back to a visible window. macOS and Windows still run natively and say so. `CWTOOLS_TEST_DISPLAY` selects `xvfb`, `ozone` or `native`, and `npm run test:native` is the explicit visible-window runner. CI drops its own `xvfb-run` prefixes. (#406)
* Coverage comments and job summaries show one suite overview and keep per-file tables in collapsed sections. (#417)
* Build, package, release, coverage, and rules-pin helpers live under `scripts/build/` as Python. Tests moved to `tests/scripts/`. (#386)
* CI publishes extension-host coverage next to rust and node. The host runner forces the instrumented Electron process to exit after mocha and kills a leftover process tree if it hangs under xvfb. (#390)
* PR CI is one workflow. Test, Engine CI, cargo-deny and the diagnostics guards fold into `.github/workflows/ci.yml`, cargo-deny runs in the rust lint job, and the coverage comment reports rust and node together. (#388)
* The vanilla diagnostics guard runs on Windows as well as Linux, catching path and executable-resolution regressions in the cross-platform driver. (#383)
* Extension-host coverage follows the emitted JavaScript paths again after the repository reorganization. The command removes old artifacts first, rejects empty reports, and names its `unit` label and source scope in the rendered summary. (#402)
* CodeLens displays the number of references above type-instance definitions and opens VS Code's reference view. (#308)
* `scripts/guard.py` defaults `CWTOOLS_PROJECTS` to this repo's parent directory instead of `~/Documents/github-projects`, matching where the docs already say the sibling `Millennium-Dawn` and `cwtools-hoi4-config` checkouts live. (#464)

#### Engine

* LSP parse errors, loc diagnostics, and .cwt rule-parse errors now use the same per-file cap as validation errors. (#590)
* LSP color ranges clamp columns beyond the parser limit instead of wrapping.
  (#593)
* CLI integration coverage now exercises `cache-vanilla` writes and
  `validate --vanilla-cache` round trips. (#628)
* CW268 quote fixes skip unrepresentable ranges, and LSP position conversion
  clamps columns to the parser's `u16` limit. (#630)
* The per-reference localisation check now borrows the ruleset's loc-command set
  instead of deep-cloning it for every loc-bearing field, so a full-mod run stops
  paying ~86 allocations and a hash-set build per reference. (#548)
* LSP: document symbols, selection ranges, references, rename, inlay hints,
  document links, code actions, code lens, goto and formatting now resolve
  positions against one line index per request instead of rescanning the file
  for every node. Opening a large file no longer wedges the server: an outline
  over 100k clauses used to be quadratic in the file size. (#541)
* Ruleset duplicate types now use the first definition consistently, duplicate
  enums union their members, and built-in variable lookups avoid lowercasing
  already-lowercase names. (#488)
* Parse caches now preserve existing entries when `settings.sig` cannot be read,
  returning the read error instead of treating it as an invalidation miss. (#495)
* Corrupt parse caches are bounds-checked before their strings are interned. (#491)
* Parse cache loads also reject a clause that references itself or nests deeper
  than the parser goes, so a hand-built `.cwb` can no longer send the recursive
  AST walks into an unwinding-free stack overflow. (#540)
* LSP: closing an ignored file no longer deadlocks the server or leaves stale
  type, localisation, and watched-file indexes behind. (#469)
* Type-instance indexing releases interned node keys before recursive skip-root
  traversal and subtype hooks, avoiding writer-contention deadlocks. (#480)
* Inline script validation now caps per-file expansions to prevent hostile fan-out
  from wedging validation. (#538)
* `validate` and `loc` now fail when an explicit `--ignore-hashes` file cannot
  be read or an `--output-hashes` file cannot be written. (#489)
* LSP workspace-scan pass 2 snapshots the type index with an `Arc` pointer bump
  instead of cloning the full index while holding `info_service` for reading.
  On the current 258k-instance Millennium Dawn corpus, `snapshot_clone` measures
  5.2 ns for the snapshot instead of 84.4 ms for the deep clone. The first index
  write while pass 2 still owns its snapshot pays the copy-on-write cost instead,
  measured at 37.0 ms; later writes use the new unique copy. (#225)
* `discover_vanilla_dir` now maps `eu5` to its Steam install folder, "Europa
  Universalis V", so the editor auto-discovers an installed EU5 base game the
  same way it already does for ck3/vic3/ir. This turns on the vanilla-gated
  checks (CW113, CW222, CW500) for EU5 users with the game installed. (#339)
* Localisation scope validation now checks CK2 and VIC2 loc commands against
  their own hardcoded scope tables (`CK2_SCOPES`/`VIC2_SCOPES`) instead of
  silently falling back to HOI4's empty, config-driven one; only `None` and
  `Custom` still take the lenient HOI4 fallback. (#339)
* Re-blessed the Millennium Dawn guard baseline (`scripts/md-baseline.csv`)
  against the corpus and rules checkouts' current revisions: Millennium-Dawn
  @ e28bb72afe (was 44940966fe), cwtools-hoi4-config/Config @ 3a0a4e9 (was
  46d5886). The pinned baseline was stale; diagnostics moved 9208 -> 9760
  (-1651/+2203 rows), mostly CW272 (-1011/+1438) and CW100 (-559/+550) row
  churn from upstream mod content shifting, plus smaller moves in CW121,
  CW225, CW235, CW240, CW242, CW246, CW251, CW261, CW262, CW263, CW266,
  CW268, and CW275. The vanilla guard baseline is untouched. (#460)
* The hardcoded per-game scope tables and link loaders are gone. Every game's
  scopes and links come only from its config's `scopes.cwt`/`links.cwt`
  (`ScopeRegistry::from_config`), the way HOI4 already worked; a game with no
  config gets an empty registry and lenient scope checks. The scope-restriction
  checks (CW104/105/106, CW243-245, CW247, CW248) skip outright when no scopes
  are loaded, so a config without `scopes.cwt` no longer trips false positives
  where the hardcoded fallback used to resolve, and a `links.cwt` with no
  `scopes.cwt` warns that its links are dropped. The config-over-
  hardcoded backfill merge is deleted with the tables, and the vanilla guard
  fixture now declares a minimal `scopes`/`links` block so the guard builds its
  registry from config (none of that tier's codes resolve scopes, so this
  covers construction only). Scope-engine tests and benches run on small config
  fixtures instead of the deleted tables. (#373)
* LSP: the editor now expands `inline_script` call sites against the mod's
  `common/inline_scripts` bodies, matching `cwtools validate`. A call site's
  substituted body is validated against the caller's rules and scope, and a
  call to a script that can't be pulled in reports CW274. The registry is
  built during the workspace scan and kept current per file on an edit or a
  watched-file event, which also revalidates open files that call the changed
  script. (#259)
* The CLI driver now merges mod-file complex-enum members into the index, the
  same way it already does for value-set members and the vanilla cache does
  for both. Completion-only: batch diagnostics don't read `complex_enum_values`
  yet, so this closes an asymmetry between the driver and `collect.rs`'s index
  builder rather than changing any output. (#454)
* LSP: a test now covers the sampler guard that stops a late or refused
  progress tick from opening the scan's `workDoneProgress` stream on its own.
  (#433)
* LSP: the bar-stays-closed scan test could not actually fail — its fixture
  finished before any phase sampler's first tick, so the guard it names could
  be deleted without turning it red. It now holds the startup scan inside
  Parse (a new `CWTOOLS_PARSE_HOLD_MS`/`CWTOOLS_PARSE_HOLD_FILE` test hook,
  since Discover has no per-item counter to sample) while `cacheVanilla`
  closes the shared bar out from under it, so a stray sampler tick reopening
  a bar someone else already closed is something the test can catch, not just
  hope doesn't happen. (#434)
* The parser reprints script from the AST (indent, trailing whitespace, final
  newline). Parse errors yield no edits. `textDocument/formatting` and
  `textDocument/rangeFormatting` are advertised, `formatWorkspace` applies one
  workspace edit, and `cwtools format` dry-runs with exit 1 when files would
  change. (#439, #441)
* LSP: the startup scan's progress bar moves inside a phase instead of only at
  the six phase boundaries, so a long validation pass no longer sits on
  "Validating workspace… 70%" and reads as a hang. Every visible scan gets a
  phase sampler now, not just one a client drove with a `workDoneToken`, and
  each phase logs how long it took. A phase running past 30 seconds says so in
  the output channel, with its file count, so a slow scan is distinguishable
  from a wedged one. (#221)
* LSP progress integration tests now assert that both cancel paths close their
  client-owned `workDoneToken`, so a cancelled command cannot strand its bar.
  (#437)
* The startup-scan progress tests no longer hardcode the i18n phase labels or
  a copy of `Phase::span()`'s boundary percentages. One derives its expected
  labels from the count of distinct `Scan phase finished:` log lines, and the
  other reads each phase's opening percentage back from the `loadingBar`
  stream, so rewording a `progress.*` string or re-weighting `Phase::span()`
  no longer breaks or silently weakens either test. (#436)
* LSP workspace-scan tests now prove pass 2 leaves concurrent info-service
  and localisation-index writers unblocked while it validates against
  snapshots. (#235)
* LSP: a quiet scan carrying a command `workDoneToken` now reports its phases
  against that token again. `quiet` means "do not touch the server's own
  `loadingBar`/`$/progress` indicator", not "say nothing" — a client that
  explicitly asked for progress on a quiet call was getting none. (#435)
* Workspace discovery now supplies scripts, localisation, resources, and file
  indexes through one driver API. CLI and LSP localisation scans, signatures,
  references, and create-key actions share root-relative ignores, limits,
  symlink handling, and failure reporting. Vanilla scans keep workspace ignores
  out. (#250)
* Multi-mod discovery now honors `ignoreDirectories` (`exclude_dir_patterns`) during the walk, so single- and multi-mod workspaces agree on directory ignore semantics. (#412)
* Driver discovery smoke tests now pin unsorted multi-mod Session/direct parity, empty `include_dirs`, and global lexical order across folders. (#234)
* LSP: workspace diagnostic publishing injects its 1ms batch throttle, while normal tests skip the elapsed wait and a deterministic scan test covers the publish boundary. (#228)
* LSP: `validateWorkspace` now has a contention test that it gives up with `{ "busy": true }` when another scan holds the guard past the retry deadline. (#223)
* Localisation key changes now revalidate only other open localisation files whose cached `$ref$` set mentions a changed key. Parsed buffers are reused, while stale, missing, and fatally malformed buffers fall back to full revalidation. (#396)
* LSP: deleting a watched localisation file now revalidates affected open
  localisation and game files, so stale CW225/CW100 diagnostics return
  immediately. (#398)
* LSP: localisation cross-file revalidation now respects ignored files and
  inline `cwtools-ignore` directives. (#395)
* LSP integration tests now stop their server processes cleanly, so coverage
  includes the real server paths and the Rust coverage gate reflects the suite.
  (#404)

### 3.2.1

#### Engine

* LSP: added unit and end-to-end coverage for missing workspace roots, asserting discovery failures are logged via `window/logMessage` at ERROR level. (#231)
* Saving immediately after an edit no longer starts a second validation while that buffer's debounced validation is still pending. The `$ref$` cache keeps its 240k modifier/type names separate from live localisation keys, so adding a key rebuilds only the small overlay and leaves unrelated completion caches alone. On the committed Millennium Dawn-scale fixture, the full name build takes 76.5 ms; rebuilding a 4k-key overlay takes 0.24 ms instead of 25.7 ms to clone and merge an already-built base. A newly added self-referencing loc key also reports CW259 immediately rather than being mistaken for a modifier/type reference.

### 3.2.0

This is the first release with the engine in-repo under `engine/`. The extension and engine now share one version number; there is no separate submodule pin to report.

#### Extension

* `.pre-commit-config.yaml` is back at the repo root and now both gates and fixes. The fixers (`cargo fmt`, `ruff check --fix`, `black`, `eslint --fix`) rewrite staged files so the commit is always clean; the gates (`cargo clippy -D warnings`, `mypy`) still fail on anything they can't fix, and `cargo test` / `pytest` gate every push. Every hook matches only the files it owns, so a docs-only change runs nothing. The repo was brought to clean: the `subprocess.run` calls in `scripts/` got explicit `check=False` (PLW1510), the seven `scripts/*.py` that carry a shebang are now executable (EXE001, git mode 100755), and `scripts/guard.py` was re-formatted by black. (#382)

* Repo helper scripts live under `scripts/` as Python, so the same commands run on Linux and Windows. `python3 scripts/guard.py corpus|md|vanilla` replaces the three `*-guard.sh` wrappers; coverage, workspace-manifest, syntax-sync, vsix-smoke and binary-staging moved there too.

* The merged repository is organized by product. `extension/` owns the TypeScript host, webview, tests and checked-in VSIX inputs; `engine/` owns the Rust workspace; and product documentation is under `docs/`. Builds assemble a disposable extension under `dist/extension/` and put packaged VSIX files under `artifacts/vsix/`, so packaging no longer writes generated files or release versions into the checked-in manifest. `./build.sh quick` still builds both halves, and `CWTOOLS_RUST_WORKSPACE` can still point it at another engine checkout. The old [MillenniumDawn/cwtools](https://github.com/MillenniumDawn/cwtools) repository remains the engine history archive.

* The hoi4 rules fixture repo carries only what a bare repo needs to serve a fetch. #211 checked it in the way `git init --bare` leaves it, so the 13 `hooks/*.sample` files, `description` and `info/exclude` came along: roughly 900 lines of stock text that nothing in the suite reads and that git never runs for a local-path clone. `HEAD`, `config`, `objects/` and `refs/` are what the `rules-sync` label actually fetches from, and that is all that is left. (#214)
* Three changes toward #216, the `test:host` run that stops with no mocha summary, the extension host reporting `exited with code: 0` and the runner `Exit code: 1`. The graph tests no longer close a webview that is still starting up: every test in the two graph suites created a panel and disposed it again from a synchronous teardown, so a webview whose `ready` message was still in flight got torn down mid-handshake, about ten times a run, and teardown now waits up to two seconds for the panel to leave the `New` state first. The host label also loads a root-hook-only spec that records the in-flight test and prints it from `process.on("exit")`, so a run that dies without a summary says where it was instead of just stopping (verified against a spec that calls `process.exit` mid-test). And every label launches with `--disable-gpu`, which puts a developer machine with a working GPU on the software rendering CI has had all along (#210's logs show `Failed to send GpuControl.CreateCommandBuffer` under xvfb) and takes a GPU-process crash off the list of suspects. None of this is a fix, and #216 stays open. The abort came back twice while this was being written, both times with all three changes in place: once between the last FileExplorer test and the first hover suite, and once on the `live` label after the third of its four tests. The second one settles the question, since the live-settings suite never opens a webview at all, so the graph handshake cannot be what breaks the run; the teardown wait was still worth doing, it is just not the cause. That is also why the diagnostic spec is first in every label's file list rather than only in `host`: the abort is not host-specific and the run it happened on had nothing loaded to record it. (#216)

* The older `GraphPanel Tests` suite runs its setup and teardown as mocha hooks. It called hand-rolled `before()` and `after()` helpers inline from each `it()` body, so a test that failed before reaching its last line never ran the teardown and left a live panel and a temp directory behind for whatever ran next, which is a poor neighbour for a suite already being investigated for cross-test interference. They are `setup`/`teardown` now and run either way, and the two tests that were `async` only to await those helpers are plain functions again.

* The cytoscape render check no longer accepts a reply meant for an earlier check. `checkCytoscapeRendered` kept a single unlabelled pending promise, so once #212 gave it a one-second answer for a webview that never replies, a reply that arrived after that deadline resolved whichever check happened to be in flight by then. The poll loop asks every 25 ms, so overlap is the normal case, not the odd one, and the answer it read could describe the graph as it was a second earlier. The check now carries an id the webview echoes back, and a reply whose id does not match the pending check is dropped. Its timer is cleared once a reply lands, rather than left to fire into a settled promise a second later.

* That check also has tests now. It was the fix for a flake (#210) and shipped without any, and the host suite cannot supply them: a real webview replies, so the host only ever walks the happy path. Four node tests drive the panel against a fake webview with fake timers and pin the reply, the one-second answer when none comes, the fresh post per call the poll loop depends on, and the late reply being ignored. Each was confirmed to fail against the behaviour it describes (widening the time bound fails two of them; accepting any reply id fails the fourth).

* The hover and completion suites hook the LSP error monitor after activation instead of before it. The monitor wraps `defaultClient`'s output channel, and `defaultClient` only exists once activation has built the client, so calling it first meant the first test of a run was never monitored (later tests re-tried the hook and got it). It caught nothing in practice, because the host label runs those suites after others have already activated the extension, but it made the guard depend on file ordering for no reason.

* The skipped hover assertion for MillenniumDawn/cwtools#317 says why it is still skipped rather than that it is broken. The `value[…]` localisation preview is fixed upstream in 786a11bb and passes against a server built from it, but the pinned engine (d38febcd) predates that commit and the pin is what CI builds, so the test comes back when the pin moves. The completion skip for MillenniumDawn/cwtools#318 is unchanged; that one is still open upstream.

* The extension ships in Arabic, French, German, Italian, Spanish, Simplified Chinese and Traditional Chinese, alongside the English it has always had. Two string sets were involved and only one of them existed. The manifest side already went through `package.nls.json`, but the only translation was `package.nls.zh.json`, and several of its entries were written against wording the English file has since outgrown (the localisation-languages, error-code, ignore-files and nine game-path descriptions all say more than they did); it is refreshed and renamed to `package.nls.zh-cn.json`, since with `zh-tw` now present as well a bare `zh` would have served Simplified to Traditional readers via VS Code's region-stripping fallback. The runtime side had no translation route at all: 41 strings the extension shows itself — every warning and error toast, the graph-depth prompt, the progress-notification titles, the antivirus and trusted-path dialogs and their buttons — were inline literals. They now go through `vscode.l10n.t()` against `l10n/bundle.l10n.<locale>.json`, which the manifest points at with a new `l10n` field. The bundled server reads the same display language out of the `initialize` handshake and translates its diagnostics, progress, code-action titles and hover labels to match (MillenniumDawn/cwtools#118), so a translated window is translated end to end rather than half-and-half. Paradox script keywords and setting ids stay untranslated everywhere: `FROM`/`ROOT`/`PREV`, `cwtools.profiling`, `glob`, the `CWxxx` codes. (#118, #119, #120, #121, #122, #123, #124, #125)

* The nls guard covers all of it and finds new files by itself. It reads the locale list off disk instead of a hard-coded array, so a `package.nls.<locale>.json` is checked the moment it lands; it fails a manifest locale that has no runtime bundle, a bundle key no `l10n.t()` call passes, and a translation that drops or adds a `{0}` placeholder — which is silent at runtime and eats the value the message was naming. The two `inlayHints` keys the pseudo-locale file was missing are filled in.

* Arabic is included with the caveat that VS Code ships no Arabic language pack, so it is reachable only by setting `"locale": "ar"` in `argv.json`, and the surrounding editor chrome stays English. The translations themselves are a first pass rather than reviewed by native speakers; corrections are welcome on the per-language issues.

* The `host` label (hover and completion, on top of the existing smoke suite) now gates `pr.yml`'s `build` job. It turns out the sample workspace has detected as `stellaris` since #185 added `common/species_classes` as an (unrelated, incidental) content marker; the header comment in `.vscode-test.mjs` claiming otherwise was stale, and nothing needed to change in the fixture. The two assertions that are red against genuine engine gaps (a `value[…]` string hover, and the per-file `pop_faction_flag` value set) are now `test.skip` instead of failing, tracked at MillenniumDawn/cwtools#317 and #318 as before. (#139)
* The smoke suite's cytoscape render check no longer hangs when the graph webview is slow to start. `checkCytoscapeRendered` posts a message to the webview and resolved only on the reply, but a webview that hasn't attached its listener yet drops the message, so the reply never came and the probe promise never settled; `waitUntil` awaits the probe before it can enforce its own deadline, so the whole test sat until mocha's 30s timeout. On loaded CI runners the webview lost that startup race roughly half the time (three of five `pr.yml` runs on 2026-08-18). The check now answers not-rendered after a second with no reply, so the poll loop re-posts until the webview is up and the test budget is enforced by `waitUntil` again. (#210)
* Added a `rules-sync` host suite that exercises the real activation-triggered rules sync, which the existing suites never touch: the shared sample workspace detects as the generic `paradox` language on purpose, which has no rules repo, so `resolveRulesCache`/`fetchRulesInBackground` returned before running any sync logic. A new `client/test/sample-hoi4` fixture detects as hoi4 instead, and `games.ts` and `rulesManifest.ts` now read `CWTOOLS_TEST_HOI4_REPO`/`CWTOOLS_TEST_HOI4_REF`/`CWTOOLS_TEST_RULES_MANIFEST_URL` (unset in every real install) to point that sync at a checked-in bare repo under `client/test/fixtures/hoi4-rules.git` instead of the network, so it clones a real, known commit with zero network access and asserts the pin lands correctly and the shallow fetch left the previous commit unreachable. It still needs a real `cwtools-server` binary, since `activate()` returns before the rules sync runs when the binary is missing, so it is wired up as its own `rules-sync` label and `npm run test:rules-sync`, not folded into `test:smoke`. (#171)
* The `workspaceContains` activation events still hard-coded the narrow directory list #203 removed from the file watchers, so a mod laid out outside `events/common/decisions/...` and without a `descriptor.mod` never activated the extension. They're now keyed on extension like the watchers: any `txt`, `gui`, `gfx`, `sfx`, `asset`, or `map` file anywhere in the workspace, and `yml`, `yaml`, or `csv` under a localisation directory. (#204)
* `mapIgnoreOptions` no longer rewrites `cwtools.errors.ignorefiles` entries into `**/<name>` globs before sending them to the server. That rewrite existed because the server only matched a slashless pattern against a path's last segment; the v2.6.1 engine pin now matches such a pattern at any depth on its own (MillenniumDawn/cwtools#244), so the client-side prefixing was redundant and is dropped. User-facing behaviour is unchanged with that engine. (#196)

#### Engine

* Saving immediately after an edit no longer starts a second validation while that buffer's debounced validation is still pending. The `$ref$` cache keeps its 240k modifier/type names separate from live localisation keys, so adding a key rebuilds only the small overlay and leaves unrelated completion caches alone. On the committed Millennium Dawn-scale fixture, the full name build takes 76.5 ms; rebuilding a 4k-key overlay takes 0.24 ms instead of 25.7 ms to clone and merge an already-built base. A newly added self-referencing loc key also reports CW259 immediately rather than being mistaken for a modifier/type reference.

* The Millennium Dawn guard baseline drops 614 CW266 rows on scripted-GUI `[!callback]` localisation (`!foo_click`, `!foo_click_enabled`). The engine already resolved those against scripted-GUI effects/triggers (MillenniumDawn/cwtools#350); the baseline had not been re-blessed. Five other CW266 rows remain.

* LSP: opt-in workspace-wide diagnostics control. A new `workspaceWideDiagnostics` setting (default `true`) decides whether the scan publishes diagnostics for closed files; set it to `false` to keep the Problems panel scoped to open documents only. A new `validateWorkspace` execute command runs a full scan and returns a JSON summary (`totalFiles`, `filesWithErrors`, `totalErrors`, `totalWarnings`, `totalInfos`, `totalHints`) regardless of the setting, so "is my mod clean?" has a direct answer. The scan also caps closed-file `publishDiagnostics` traffic at 2,000 files per pass and yields every 50 publishes so a 10k-file mod cannot flood the client. (MillenniumDawn/cwtools#106)

* The server speaks Arabic, French, German, Italian, Spanish, Simplified Chinese and Traditional Chinese. It reads the display language from the `locale` the client sends in `initialize` (`initializationOptions.locale` also works for a client that sends neither), and answers in it: the 64 diagnostic messages that carry English text of their own, the scan progress and status-bar phases, the `reloadrulesconfig` / `clearAllCaches` / `reindexWorkspace` result toasts, the ten code-action titles, and the hover's `Scope` / `Resolves to` / `Required scopes` / `Localisation` / `Description` labels. A code or key a language has not translated falls back to English rather than showing a placeholder, and nothing sets a locale in the CLI, so batch runs, SARIF and the corpus baselines stay English. Paradox script keywords are left alone throughout: `NOT`/`NOR`/`AND`, `ROOT`/`PREV`/`FROM`, `set_name`, `l_language.yml`, `mean_time_to_happen` and the `CWxxx` codes all read the same in every language. (#118)

* LSP: workspace scan pass 2 no longer holds the index locks across the whole validation. It snapshots the type and loc indexes under brief read guards and validates in 256-file chunks with yields, so a keystroke that needs `info_service.write()` no longer blocks for the full scan. (MillenniumDawn/cwtools#288)

* `cwtools validate` no longer changes its diagnostics when the same mod sits on a different filesystem. Loc discovery and `.cwt` loading sort each directory, and a loc key's command-validation representative is the English entry when English exists, even if that string has no `[command]`s. A Brazilian `[Grécia]` (or any other language's commands) can no longer turn a valid `desc = EUevent.5.d` into CW267. (MillenniumDawn/cwtools#313)

* LSP: the server no longer goes silent a moment after startup. Asking for semantic tokens twice on an unedited document hit a cache fast path that re-locked a mutex it was already holding, and the thread it wedged is the one carrying stdin, dispatch and stdout together, so the whole server stopped: no further log lines, no diagnostics, no answer to any request. The last line in the output channel was whichever one the workspace scan happened to have reached, usually `Parse cache: hit (settings match)`, which made it look like an indexing failure rather than a stall in front of it. The same shape is fixed in the delta request and in the rename handler. Introduced in 2.6.0; 2.5.0 was unaffected. (MillenniumDawn/cwtools#334)

* LSP: the watched-file drain now applies the same ignore predicate as the workspace scan. Files under a directory matched by `cwtools.ignore.directories` (and the engine's directory exclusions) are skipped before they are read or validated, so an ignored directory can no longer produce diagnostics through a watcher event. (MillenniumDawn/cwtools#314)

* A localisation string that calls a scripted localisation is no longer reported as calling something that does not exist. `[ROOT.Western_Autocracy_L]` gave CW226 and `[Western_Autocracy_L]` gave CW266, on every use, in every HOI4 mod. The engine looked scripted localisations up through the ruleset's `scripted_loc` type, which the HOI4 config declares at Stellaris's `common/scripted_loc`; nothing under HOI4's `common/scripted_localisation` ever matched, so the set was always empty and an empty set read as "flag everything". Names now come from the folder itself, base game included, and a bare `[SomeName]` bracket is judged against them the same way a dotted chain is. A misspelt name still reports. (MillenniumDawn/cwtools#348)

* **Behavioral:** HOI4 `[!name]` localisation calls now resolve `name` against the direct callback keys under scripted GUIs' `effects` and `triggers` containers. Known callbacks no longer report CW226 or CW266, while an unknown callback reports the new Rust-only CW283 once callback data is available. Workspace and base-game callbacks are indexed separately from scripted localisations, and the vanilla cache format moves to version 13 to carry the base-game names. Existing `.cwv` files rebuild once. (MillenniumDawn/cwtools#350)

* **Behavioral:** a loc-typed field is judged against the English string when English exists. The MD guard baseline drops two CW267 rows on `events/EU_events.txt` (`EUevent.5.d` / `EUevent.7.d`) and one CW266 on `events/Iran.txt` (`axis_of_resistance_events.2.t`), all of which were other languages' `[command]`s winning the representative.

* **Behavioral:** with no scripted-localisation data at hand, an unknown command tail is no longer called a typo. Nothing can tell a `defined_text` from a misspelling without the names, so CW226 and CW266 stay quiet there, the way a `?`-marked variable read already does without a variable index. Two passes see that: the editor before its first workspace scan finishes, and `cwtools loc`, which reads the `.yml` files and the ruleset and walks no game files at all. `loc --game --rules` still reports CW260 for a scope link used from a scope that does not accept it.

* **Behavioral:** the vanilla cache format (`CACHE_VERSION`) moves to 12 and carries the base game's scripted-localisation names. An existing `.cwv` is a clean miss and the install is re-indexed once.

* **Behavioral:** the parse cache fingerprint (`CACHE_VERSION`) moves to 8 and now includes the display locale. The `.cwe` sidecar stores each file's diagnostics as finished text, so a cache written in one language would otherwise be replayed verbatim in another. Existing caches are a clean miss on first run after upgrading, and changing the editor's display language costs one cold re-index.

* The diagnostic catalog is enumerated once, as `cwtools_error_codes::CATALOG`, instead of being hand-mirrored in the CLI. `cwtools-rs/crates/cli/src/codes.rs` reads it, and the test that diffed the mirror against the consts moved to `error_codes` beside both.

* `cargo bench -p cwtools_driver --bench snapshot_clone` times the workspace-scan pass 2 index snapshot, the clone that is the whole `info_service` lock hold, and splits it between the type maps and the two dynamic-value indexes. On Millennium Dawn's 249k instances the clone is 41ms, of which the dynamic-value indexes are about 7%, so the maps are what a future change has to move. Runs against a synthetic index when no corpus checkout is present. (MillenniumDawn/cwtools#332)

* Workspace scan pass 2's chunked walk is now a helper with the chunk size as a parameter, so tests can assert the chunked and unchunked results are the same sequence. They run the boundary sizes (0, 1, 255, 256, 257, 300, 512), every chunk size from 1 up to one big enough to swallow the input, and pin that a cancelled walk returns nothing rather than the chunks it had finished. A companion test pins that `instances_in_file` read off a `TypeIndex` clone still answers from the snapshot after the live index compacts its slots. (MillenniumDawn/cwtools#328)

### 3.1.1

* The engine pin moves to v2.6.1, which fixes a Windows-only false positive that made 3.1.0 close to unusable there. Every server-side index is keyed on the raw URI string, and nothing canonicalised those keys, so a file reaching the server under two spellings was indexed twice. On Windows that is every open file: VS Code percent-encodes the drive colon (`file:///d%3A/mod/x.txt`) while the workspace scan builds its keys with `Url::from_file_path` (`file:///d:/mod/x.txt`), so the scan's "skip documents the editor already has open" string compare never matched and each open document was indexed a second time from disk. It stayed invisible until 2.6.0 changed CW261 to ask the type index how many times the *project* defines an id, at which point every `## unique` definition in every opened file reported as defined twice — and because the span is the whole definition block, a focus-tree file squiggled end to end. Incoming document URIs and the workspace folder URIs are now folded onto one spelling at the same boundary. (MillenniumDawn/cwtools#319)

### 3.1.0

* The engine pin moves back to `3309db2`. Dependabot PR #197 bumped it from `3309db2` to `29b9a1c`, which is an *ancestor*, so it reverted both `dcea7893` (apply LSP settings without restart) and `3309db2` (incremental semantic tokens) while two entries in this file said they had shipped. The `automerge-engine.yml` guard added in #179 refuses exactly this move, so #197 did not come through auto-merge; the fast-forward check is worth extending to whatever path did merge it. This went unnoticed because the one suite that covers live settings was never running in CI, which the entry below fixes.

* The engine pin moves the rest of the way to the tagged v2.6.0, which `release.yml` requires exactly (`git describe --tags --exact-match`). Past `3309db2`, that release adds LSP validation of `inline_script` bodies at their call site instead of only from the CLI (MillenniumDawn/cwtools#259), populates the file index so CW113 (missing `filepath`) actually fires in the editor (MillenniumDawn/cwtools#311), widens rule-aware scope inlay hints and the `cwtools-ignore` workspace action (MillenniumDawn/cwtools#275, MillenniumDawn/cwtools#282), and hardens `FilepathField` probes to stay inside the search roots alongside a Windows drive-letter parsing fix (MillenniumDawn/cwtools#233, MillenniumDawn/cwtools#307).

* Activation no longer parks forever when the server binary is missing, which is what had been hanging `pr.yml`'s check job for its full 20 minutes on every run since 2026-08-12. The "no language server binary found" path did `await window.showErrorMessage(...)`, and an error notification stays up until somebody dismisses it, so in a headless extension host that await never resolves. VS Code waits for the extension under development to finish activating before it hands control to the test runner, so mocha printed nothing at all and the job ran to the timeout. The check job builds no server (it doesn't check out the submodule), so it took that path every time. The notification is now fired and forgotten, as the same file already does for the `client.start()` failure, and the "you opened a file directly" warning gets the same treatment. Reproduced by forcing `serverExe` to resolve to nothing: before, no mocha output and a hang; after, 17 passing in 86 ms. The two host test steps in `pr.yml` also carry a `timeout-minutes` now, so the next hang fails in minutes rather than eating the job budget. (#205)

* The live-settings suite has never run in CI. `.vscode-test.mjs` gave it a second config labelled `smoke`, but `@vscode/test-cli` resolves `--label` with `config.tests.find()`, so only the first `smoke` entry ever ran and the sample-live workspace was silently dropped. Labels are unique now, and `test:smoke` passes both `--label smoke` and `--label live`. That put four previously unrun tests into the gate, two of which were failing: the fixture's localisation files had the language header and the entry on one line and carried no BOM, so nothing was indexed and every hover came back without localisation text. Both files now match the layout the main sample uses.

* The host suites poll instead of sleeping. `retryAsync` multiplied a retry count by a coarse delay, which tied poll granularity to the time budget: a check that would pass 20 ms later still paid the full 500 ms, and raising the budget for a loaded runner slowed the fast path too. It is replaced by `waitUntil`, which polls every 25 ms against a deadline, and the fixed `wait(1000)` before the cytoscape render check is gone. `waitForLSP` and `waitForLanguageServer` are deadline-based for the same reason. The full `host` label drops from 8.9 s to 5.7 s wall (4 s to 2 s of mocha), `smoke` from 3 s to 0.8 s, and the live-settings suite from 15 s to 1 s. The tight budgets those tests used to run under (three tries at 500 ms) are now 10 s, so they are less likely to fail on a busy runner rather than more.

* The hover and completion suites assert what the Rust engine actually returns. Their expectations were written against the old F# hover format and never updated, so they required a scope table reading `Any` / `Country` / `ROOT` / `THIS` while the engine renders `**Scope**: country` and `**Root**: country`. The descriptions and scopes were all being served correctly; only the assertions were stale. Both hover tests now pin the trigger name, its rule description, and the scope lines, including the ROOT/PREV pair that survives a scope change. Three completion tests that could not fail were removed or replaced: two restated the "we got items, none of them Text kind" precondition their own helper already asserts, and a third allowed 5 seconds for a request that takes single-digit milliseconds. In their place the trigger and effect contexts are asserted as a pair, each naming what it must offer and what it must not offer from the other context, which is a check that scope awareness can actually fail. Two host tests remain red against genuine engine gaps, now tracked: a `value[…]` string hover reports the enclosing trigger instead of previewing the localisation it resolves to (MillenniumDawn/cwtools#317), and the `pop_faction_flag` value set is built per file, so a flag set in `common/button_effects` is not offered in `common/pop_faction_types` (MillenniumDawn/cwtools#318).

* The four graph editor/title commands (**Show graph**, **Save graph as image**, **Set graph depth**, **Save graph as json**) now carry a codicon instead of showing their full title text in the tab bar. (#144)
* `client/webview/graph.ts` now shows up in a coverage report. The host run excludes `client/webview/**` and the vitest `include` list never listed it, so the 642-line module counted in neither report, even after #135 added node tests that drive `setupTooltips` and the graph build. It is now a vitest-owned module, which grows that report's denominator and drops the node totals once, from 80.5% to 73.6% lines; graph.ts itself lands at 59.5% lines and 45.3% branches. The host run still excludes the webview, so the two reports stay disjoint. (#190)
* `publish-marketplace.yml`'s manual republish uploaded only the universal vsix. Both the publish and dry-run steps picked the release's vsix with `find | sort | head -n 1`, so the per-platform packages were downloaded and then silently dropped. Both steps now collect every downloaded vsix into one list and pass the whole set to a single `vsce publish --packagePath` (and `ls-publish` for the dry run), matching how `build.ts`'s `publish-prebuilt` already treats a release's vsixes as one set. (#131)
* `check` and `build` in `pr.yml` no longer re-download a full VS Code build on every run. `.vscode-test/` is cached via `actions/cache`, keyed on the OS plus the stable version resolved from the same update API `vscode-test` uses for the `vscode: "stable"` pin in `.vscode-test.mjs`, so a new cache entry is written only when stable moves. Restore-keys seed the newest prior cache if the resolve fails, and `vscode-test` tops it up. (#137)
* A pin bump that replaces an already-cached rules checkout is no longer silent. `syncPinnedRules` now logs the commit being replaced alongside the one replacing it, and shows an information message on that path, distinct from the (still silent) first-time download. The already-current path is untouched. (#168)
* Locations the server reports are now checked before anything opens them. Clicking a node in the graph or a file in the CWTools files tree passed the path straight to `showTextDocument`, so nothing but the engine's own discipline kept an arbitrary local path off the screen, and on Windows a UNC path would have started an SMB auth handshake with whatever host it named. Both handlers now require an absolute `file:` path, and open it directly only when it sits under a known root: a workspace folder, the extension's cache directory, the rules folder in use, or the configured base-game install. Anything else gets a modal naming the path and opens only if you confirm. Containment is lexical, so `..` can't climb out and a sibling that merely shares a root's name prefix doesn't match. (#134)

* Opening a large graph no longer builds every node's tooltip up front. `setupTooltips` created the header twice and the full details table for each node at setup time, so a graph with thousands of nodes made tens of thousands of detached DOM elements before the layout even ran, for tooltips most of which are never seen. The tooltip DOM is now built inside tippy's `content` closures and memoized there, matching the existing `popperRef` deferral: the header is built on first hover, and the details table only on a hover held past the expand timeout. The dummy tippy anchor moved into the same lazy path. While in there, the edge dedup in `go()` dropped its per-edge `JSON.stringify` key for a NUL-delimited template literal and now builds the deduped list in one pass, instead of materializing an intermediate array of every reference first. (#135)

* The settings schema now declares string `items` for the three array settings (`cwtools.errors.ignore`, `cwtools.errors.ignorefiles`, `cwtools.ignore_patterns`), so the Settings editor renders them as string lists instead of free-form entries. Descriptions that embed error codes, globs, or a reload caveat moved from `description` to `markdownDescription`, with the tokens in code spans and the reload instruction bolded. `cwtools.graph.zoomSensitivity` is now `window` scoped; it was the only `resource`-scoped setting despite being a window-level UI preference. (#143)

* `npm run build -- release` cuts a real release again. It pushed a bare `2.5.0` tag, because the CHANGELOG heading regex captures the version without the `v` and nothing put it back, so `release.yml` (which triggers on `v*`) never fired. The command then packaged and published from the dev machine instead: with no per-platform binaries staged locally, that is a single vsix carrying only the release machine's own server binary, published to the Marketplace with no matrix build and no smoke test. `release` now checks the CHANGELOG section exists, refuses a dirty tree or a tag that already exists locally or on origin, pushes `v<x.y.z>`, and stops. CI does the build, the smoke test, and the publish. The `v` is added where the tag is derived, so a `workflow_dispatch` run no longer cuts a bare-version GitHub release either. (#128)

* Extension highlighting no longer lags or flashes disco colors on large edits and after moving files. The server now serves `textDocument/semanticTokens/full/delta` with a per-file result cache and content-hash memoization so repeated polls skip the walk, and pushes `workspace/semanticTokens/refresh` after a bulk reindex, rules reload, or a watched-file batch so visible editors re-request without a tab switch. File renames are handled via `workspace/didRenameFiles` (with `didCreate`/`didDelete`) and the delta cache is moved to the new URI, so a moved file recolors immediately. `full` now advertises `delta:true` and the file-operations capability covers `**/*.{txt,gui,gfx,asset,yml,cwt}`. (#184)

* Localisation languages and hover preferences now apply without reloading the window. Changing the languages or whether hovers show every language rebuilds the localisation index and diagnostics; debug classification and resolved scopes affect the next hover. `cwtools.rules_folder` and the base-game install-path settings still need a reload, which each description now says. (#142)

* The engine auto-merge no longer accepts a bump that points the submodule backwards. `automerge-engine.yml` checked only that Test passed and that the PR touched nothing but the gitlink, so a Dependabot PR moving the engine to an *older* commit merged like any other — #177 was one (back to `v2.4.0`), stopped only by main's ruleset refusing an unapproved merge. Being tagged is no defense either: `release.yml` refuses an untagged engine, but an older tag clears that gate and would ship a regressed engine. The workflow now reads the gitlink on the base branch and on the PR head and merges only when the move is a fast-forward (GitHub's `compare` status is `ahead`, the API equivalent of `git merge-base --is-ancestor`), leaving anything else for review with a line in the run summary. The changed-path check asserts the file count rather than relying on a one-file PR rendering as a single line, and the header comment no longer claims main is unprotected. (#179)

* The long bulk commands now show a real progress bar, and Cancel actually stops the work. **Clear all caches and reindex**, **Re-index workspace**, **Regenerate game vanilla cache file**, **Reload config rules**, **Generate missing loc**, **Fix all auto-fixable problems in workspace** and the graph build each run inside a cancellable notification that carries the engine's own phase text ("Indexing workspace…", "Validating workspace…") and a percentage that moves during the long parallel passes, instead of an indeterminate spinner with no detail. Cancel now sends `window/workDoneProgress/cancel`, which the engine acts on *while* the scan is running — it stops within a file rather than at the next phase boundary, which on a large mod could be tens of seconds away — and the command returns and reports what happened ("Re-index cancelled.") rather than vanishing silently. Cancelling **Clear all caches** after the purge no longer leaves the base-game index dropped: the rebuild is handed to a background retry and the message says so. Requires MillenniumDawn/cwtools#223; against an older engine the notification keeps its previous indeterminate-spinner behaviour, detected from `executeCommandProvider.workDoneProgress`. (#145)

* One progress indicator per operation instead of three. A bulk command used to light up the extension's status-bar item, a second status-bar spinner from the language client's `$/progress` handling, and the command notification, all for the same scan. The engine now reports a command's progress against the token the client supplies rather than opening its own stream, and the status-bar item stands down while a command notification owns the screen. The status bar still shows the percentage for scans nobody asked for (startup, the periodic background pass). (#145)

* `package-prebuilt` no longer loses the staged server binaries if one platform's `vsce` run fails partway through. `packageAllVsixes` keyed its `finally` restore on whether `release/bin/server` still existed, but a mid-loop failure leaves the failing platform's binary there, so the restore was skipped and the holding directory (the only complete copy) was deleted. The restore is now gated on a success flag, and the partial directory is wiped before the full set is copied back. (#129)

* Edits made outside the editor are picked up for every file the server indexes, not just a hand-picked subset. The client watched `.txt` only under nine named directories, `.gui`/`.gfx`/`.sfx`/`.asset` only under `interface`/`gfx`/`fonts`/`music`/`sound`, and localisation only as `.yml`, while the server's workspace scan walks the whole tree and takes any `txt`, `gui`, `gfx`, `sfx`, `asset` or `map` file, plus `yml`, `yaml` and `csv` under a localisation folder. A git checkout or an external tool touching, say, `portraits/`, `dlc/` or a `.map` file left the index stale until a reload. The watchers are now keyed on those extensions instead of a directory list, with localisation still scoped to `localisation`/`localisation_synced`/`localization` because that is what the server's own loc check requires. Binary resources (`.dds`, `.mesh`) stay unwatched: the server reads a watched file as script, and the file index that filepath checks resolve against is not built in the editor at all. Because extension-keyed globs also catch files the server's own walk skips, and its watched-file path does not re-apply that skip list, the client now drops watched events for the names and directories the engine excludes (`Changelog.txt`, `README.txt`, `LICENSE.txt`, markdown, and anything under `.git`, `node_modules`, `dist`, `target` and the rest); the user's own ignore globs stay a server-side concern. Localisation `.yaml` files also open as Paradox Localisation now, and `.yaml`/`.csv` loc reaches the server on open. (#117)

* A missing `git` on `PATH` no longer surfaces as a cryptic `spawn git ENOENT` inside the rules-download warning. `runGit` translates the spawn ENOENT into a `GitNotFoundError`, and the initial-clone warning now says CWTools needs Git on your PATH (and to install Git and reload) instead of pointing at the network. (#169)
* `publish-marketplace.yml`'s dry run called `vsce ls-publish`, which isn't a real vsce subcommand, and hid the failure behind `|| true`, so every dry run reported success without validating anything. It now runs `vsce generate-manifest --packagePath` against each downloaded vsix, which actually opens the package and fails the step if one is missing or corrupt. (#193)

### 3.0.1

* 3.0.0 was tagged but never shipped. A release refuses to bundle an untagged engine, and the submodule pointed at a branch commit, so every platform job stopped at that gate and no vsix was built. The pin is now the tagged engine v2.5.0, which carries the same ROOT fix, merged as MillenniumDawn/cwtools#221.

* Installs are on extension 2.5.0, which bundled engine v2.4.0, so this release brings everything in between. **Fix all auto-fixable problems in workspace** has a server side now, so the command the client gates on capability actually runs, and CW100 gains a quick fix for a missing localisation key (MillenniumDawn/cwtools#128). Go-to-definition resolves an enum value. CW113's filepath check gets an opt-in case-sensitive mode, scope fallback no longer breaks on a mixed-case block key, and a subtype-qualified reference counts toward the unused check. On the hardening side, generated workspace edits stay inside the workspace, cache deletion is constrained to cwtools-owned entries, LSP file reads go through a URI access boundary, symlinks are rejected in every discovery walk, and a panic in a background task is logged and recovered instead of taking the server down.

* The `fixAllWorkspace` smoke test asserted the bundled engine does not advertise the command, and predicted in its own comment that a future engine would flip it. v2.5.0 does, so the test now pins the other branch: the server advertises `fixAllWorkspace`, and running the command produces neither the upgrade warning nor a raw protocol error. Activation exposes `serverCommands()` so the host suite can read the advertised commands without loading a second copy of the extension's modules.

### 3.0.0

* `ROOT` inside an event now resolves to the event's own scope instead of the file default. The engine seeded an event's `## push_scope` onto the current-scope stack but never updated ROOT, so `ROOT = { … }` blocks in HOI4's hybrid-scope `unit_leader_event`/`state_event` were falsely checked against country — `add_max_trait` and other unit-leader effects in `ROOT` fired a bogus CW105. The pinned engine now sets ROOT from the event's scope (lenient `any` for the hybrid types), matching the original F# behavior. (#152)

* Long-running commands now show a cancellable notification and pass cancellation to the language server without an error toast. Server status cleanup is tracked in MillenniumDawn/cwtools#204. (#145)

* The graph panel now survives a window reload. The webview persists the request parameters (entity type, depth), and a `WebviewPanelSerializer` re-opens the panel and re-requests the graph from the server; a graph imported from JSON prompts for the file again, since that data isn't persisted. (#146)

* The rules repos are now pinned. Each game in `client/extension/games.ts` carries the exact commit the extension fetches, and the fetch is a shallow `git fetch <repo> <commit>` plus a detached checkout instead of a clone or pull of whatever the default branch happened to hold. Pushing to one of the nine upstream rules repos no longer reaches every install on its next activation: pins move through a reviewed PR (`build/rulesPins.ts`, run weekly by `rules-pins.yml`) and publish in the reviewed `rules-pins.json` manifest. A cache already sitting on the selected pin skips the git fetch and rules reload. (#133)

* Reviewed rules updates now reach installed extensions without a full extension release. On activation, CWTools refreshes `rules-pins.json` in the background, then fetches only the full SHA selected for the detected game. An invalid, stale, or offline refresh keeps the cached manifest or bundled fallback, and a manifest cannot change a repo URL or name a branch. (#167)

* The **CWTools loaded files** tree is now navigable. It has a collapse-all button, and a file's context menu offers **Reveal in File Explorer** and **Copy Path** (both built-in commands that read the item's `resourceUri`). A new **Reveal active file in loaded-files tree** button in the view title finds the active editor's file and selects it, expanding its ancestors via a new `getParent` on the tree provider. (#147)

* Release and Marketplace publishing jobs now have bounded runtimes, so a hung
  job cannot hold the release queue indefinitely. (#136)

* ESLint now runs type-checked rules (`recommendedTypeChecked`) via `projectService`, so `no-floating-promises`, `no-misused-promises` and the `no-unsafe-*` family actually run — previously only the non-type-aware `recommended` set was active, which couldn't catch a swallowed promise. Flipping it on surfaced 140 issues across 26 files, all fixed: bare `window.show*Message`/graph calls are `void`-prefixed or awaited, LSP `sendRequest`/middleware payloads and `JSON.parse` results are explicitly typed instead of `any`, needless `async`/`await` pairs on sync helpers (`getState`, `getChildren`, test predicates) are gone, and the graph webview's tooltips build DOM nodes via `textContent` instead of `innerHTML`. Two lib getter/`String()` fallbacks that `tsc` types fine but the type-aware lint can't resolve keep a scoped disable comment. `npm run check` is clean. (#141)

* CI and the type surface now share a runtime. The setup-node composite action ran everything on Node 20, which is past end-of-life, while `@types/node` pinned the Node 26 API surface, so a Node 21+ API in the build scripts would typecheck clean and fail at runtime. CI now runs Node 26 and `engines.node` states the supported range, so the two can't drift again. (#140)

* **Fix all auto-fixable problems in workspace** is now gated on the running language server advertising the `fixAllWorkspace` command, matching how **Show graph** gates on `getGraphData`. The pinned engine doesn't advertise it, so the command previously failed with a raw protocol error toast; it's now hidden from the palette on unsupported servers, and running it directly shows a clear "needs a newer language server" warning instead. (#127)

* A tagged release now fails instead of silently shipping `--generate-notes` when `CHANGELOG.md` has no section matching the tag's version. `releaseNotes()` throws rather than returning an empty body, so a typo'd or missing CHANGELOG entry can no longer reach GitHub as generic auto-generated release notes. The `release` command checks it before pushing the tag, not after the build. The parsing moved into a pure `build/changelog.ts` module under vitest, and `build/` is now type-checked by `npm run check` (it previously ran under tsx with no typecheck, which let a `FileCoverage` cast bug in `coverage-summary.ts` slip through). `setReleaseVersion` also names the file it failed to parse instead of raising a bare `JSON.parse` error. (#138)

* Cleaned up the client. One `errorMessage` helper replaces the six copies of `err instanceof Error ? err.message : String(err)` (and the near-twin inside `logError`). The activation test asserts the `graphPanel` API is present instead of `assert.ok(true)`, the `currentEngine()` test helper that always returned `'rust'` is gone with its guards folded in, the file explorer's change emitter is disposed with the extension, `fnv1a` gains a unit test, the diagnostics-signature cache stops growing past 1000 entries, and game detection checks content hints with async fs instead of blocking activation on `existsSync`.
* The bundled themes now paint the language server's semantic tokens, which resolve the TextMate ambiguity a static grammar can't (a CK2 define key vs a scope keyword, `capital` as a keyword vs a field). Each theme opts in via `semanticHighlighting` and carries a small `semanticTokenColors` block for the structurally-classified tokens (`type`, `enumMember`, `variable`, `namespace`, `function`); the rest fall back to their scope colors. `editor.semanticHighlighting.enabled` defaults on for `paradox` files. Added [a themes guide](release/docs/themes.md) covering the semantic token legend and how to add a theme.

* `npm run test:watch` now starts both watchers on Windows. The script ran `tsc -w` and the test watcher joined with `&`, which npm executes sequentially through cmd.exe, so the never-exiting compiler watcher starved the test watcher; it worked on Linux only because `&` backgrounds there. The two now run under `concurrently`, and `-k` stops the test watcher when the compiler dies rather than rerunning stale output. (#132)
* A failed language-server start no longer causes an unhandled rejection whenever the active tab changes. The failed focus notification is logged instead. (#130)

### 2.5.0

* Upgraded to v2.4.0 of the cwtools rust engine. The `NOT`/`NOR`/`NAND` hint and the unnecessary-`AND`/`OR` hint now underline the operator itself instead of every line of the block it opens (#107), and squiggles in general no longer run one line past their content. The "Remove empty if" quick fix is withheld when an `else_if` or `else` follows the block, where applying it deleted valid script (and it was reachable from fix-all-on-save). `localisation_synced/` is walked for localisation, so HOI4 synced keys index and resolve instead of reading as missing. The rules-config error popup names the first error inline, and the per-file rule diagnostics are held until the handshake completes instead of being silently dropped (#98).
* Double-clicking a dotted id like `namespace.1` selects the whole id again, and go-to-definition resolves it without splitting at the dot. The #39 word-pattern fix was applied to the detected game's language id, and those ids were removed when the per-game languages merged into `paradox` in 2.4.0, so it had quietly become a no-op.
* The engine pin updates itself now. Dependabot checks the submodule daily instead of weekly, and a new workflow merges the bump once CI passes. CI builds that engine and runs the activation smoke suite against it, so a broken engine commit can't land, and releases still refuse to bundle an untagged engine.
* Fixed the unstable "disco" highlighting (#112). When the per-game grammars merged into one in 2.4.0, all four games' keyword lists became always-on, and 465 of the ~4,300 words sat in two or three color buckets at once, so the color a key got was an accident of pattern order. Worse, the CK2 list had dumped 824 define keys and generic data keys (`years`, `window`, `which`) into the scope-color bucket, which also outranked every other list. Each word now lives in exactly one bucket: the CK2 dump moved to the plain definition-token color, booleans beat scopes beat effects beat conditions beat data keys, `any_`/`count_` iterators color as condition scopes and `every_`/`random_` iterators as command scopes, an `if`/`else`/`else_if` chain is one color, and `yes`/`no` are no longer claimed as keys. A unit test now fails if any word ever appears in two buckets again. Colors move around once as a result; that is the point.
* An unterminated string colors only to the end of its line instead of flooding everything down to the next quote in the file, and a `{` at the start of a line, after a comparison operator, or after `rgb`/`hsv` now opens a block, so its `}` no longer closes the enclosing block and shifts every color after it by one nesting level. Lines longer than 10,000 characters (generated name lists, gfx dumps) stop tokenizing at that point, down from the editor's 20,000 default, since the merged keyword table makes each pass pricier.

### 2.4.0

* Upgraded to v2.3.0 of the cwtools rust engine, from the v1.25.0 (engine version 2.1.0) the extension had been pinned to. Validating a large mod is about three and a half times faster and uses about 40% less memory, from sharding the engine's string interner: interning took one process-wide lock, so parsing got slower as cores were added and the validator actually ran faster on six threads than on twenty-four. Four crashes are gone, two of which could take the server down mid-session. A single `'` inside a localisation command was enough to kill it, and a deeply nested file overflowed the worker stack with no depth limit to stop it, which a 48 KB file could trigger and which repeated on every restart because the file stayed on disk. The structural checks now recognise every casing of `if`, `limit`, `not`, `and`, `or` and `nor`, having silently skipped all but one spelling, and they no longer misread the arithmetic `and`/`or`/`not` of a dynamic value block as boolean logic. A corrupted cache file is no longer handed back as a valid parse tree. This release also brings the earlier v2.2.0 work, which the extension never shipped: suggested fixes with quick actions, multi-mod workspaces, and precise diagnostic ranges.
* Intel Macs get a server binary. The client resolves `osx-x64` there, but the release only ever built linux-x64, win-x64 and osx-arm64, so every Intel Mac install ended at "no language server binary found". The release matrix now also builds osx-x64 and linux-arm64, and the vsix smoke test is given the platform list so a silently shrunken matrix fails the release.
* Downloads are about a third the size. The release publishes one vsix per platform (`vsce --target`), each carrying a single server binary, plus a universal build with all five as the fallback for anything else. The smoke test now runs before the publish rather than after, so a broken package can't reach the Marketplace.
* **Show graph** works. Building a graph live needs the server's `getGraphData` command, which the Rust engine had never ported, so the command failed with "command 'getGraphData' not found" and the whole feature sat dark. The engine implements it as of v2.3.0: a breadth-first walk outward from the requested entity type, bounded by the depth you set, capped at 500 nodes with any truncation reported on the nodes themselves. **Show graph** and **Set graph depth** are still gated on what the running server advertises, so they stay hidden rather than erroring for anyone on an older engine, and **Recreate graph from json** never needed a server at all. One rough edge worth knowing: some entity types come back with nodes but no edges, because the engine's reference index only records string-valued references at the top level of a type rule, which leaves a focus tree's `prerequisite` links and numeric references like `capital = 1` invisible to it.
* Fixed three bugs in the client half of the graph, each of which only the first live `getGraphData` call could expose, and all three of which would have broken the feature the moment the engine bump above made it reachable. `getGraphData` reached the `vscode` module through `await import('vscode')`, which esbuild keeps as a native dynamic import, and that resolves through Node's ESM loader, which never sees the extension host's `require('vscode')` interception, so the very first call would have thrown `ERR_MODULE_NOT_FOUND` before a panel was ever created. Double-clicking a node to jump to its source fed the payload's 1-based line and column straight into a 0-based `vscode.Range`, so it landed a line and a column off. And the webview's content security policy named the `vscode-resource:` scheme, which VS Code replaced in 1.55 with a `cspSource` origin, so `site.css` was blocked and the graph container rendered with no height. The webview now also tolerates a node that arrives with no `references` array instead of failing the whole render on it.
* Two context keys the graph menus read were never seeded, which left the **Show graph** button in the editor title bar unable to appear at all. Its condition is `cwtoolsWebview == false` and only the graph panel ever wrote that key, so on a fresh window it was unset, which does not satisfy `== false`; the button only became reachable after a panel had been opened and closed once, and that button was the sole entry point outside the palette. `cwtoolsGraphFile` had the same shape, since it is written from the active-editor change event, which never fires for the editor that was already focused when the extension activated, so the file type stayed unknown and a graph opened from the palette right after startup sent an empty entity type. Both are seeded at activation now, the second by classifying the already-focused editor once the server can answer.
* PR CI builds the pinned engine and runs the host suite against it. Neither happened before: the workflow never checked out the submodule, so a submodule bump was first compiled by the release tag that published it, and the "unit" label it ran covers two files. There is a new `smoke` label (activation, commands, graph and file explorer, against the real server) that CI gates on, and the PR artifact is now an installable vsix with the server in it instead of a client-only shell.
* Fixed ten host tests that had been failing on "command already exists". They imported `graphPanel.ts` directly while the extension host runs the esbuild bundle, so they held a second copy of the module with its own panel, and creating a panel re-registered the panel's commands. `activate()` now returns an API the tests reach the extension's own modules through. The remaining host failures are the hover and completion suites, which need rule data the sample workspace can't resolve (it has no game name in its path and no game-specific content dir, so it detects as the generic `paradox` language, which has no rules repo).
* Added `cwtools.backgroundReindex.idleSeconds` (default 15), the inactivity window before a background re-index pass may start. The engine has read it since 1.23.0; the client never sent it.
* Added `cwtools.inlayHints.locTitles` (default on) and `cwtools.inlayHints.scopes` (default off). The engine has read both keys since engine 2.2.0, but the extension declared neither setting and sent neither, so the loc-title hints were on with no way to switch them off. Both are read once when the server starts, so a change needs a window reload, which the descriptions say. The scopes hint is scaffolding on the engine side and produces nothing yet, and its description says that too rather than leaving you to wonder why nothing shows up.
* `.cwt` rule files are watched for changes now. They have their own grammar, a structural lint and a place in the document selector, but no watcher glob covered them, so editing a rules file outside the editor (a branch switch, another tool) left the server on the old content until **Reload config rules** was run by hand. Two limits worth knowing: the watcher only reaches rules that live inside the workspace, and it refreshes that file's own diagnostics rather than rebuilding the ruleset, so **Reload config rules** is still what makes a rules change affect script validation.
* Declared what the extension needs to run: it does not support untrusted or virtual workspaces (it launches a bundled binary and shells out to git), and it runs on the workspace side in remote/WSL setups. Added the `homepage` and `bugs` manifest fields.
* Three commands (`clearAllCaches`, `reindexWorkspace`, `Export profiling log`), four settings and every settings-page group title had their text inlined in the manifest, so no translation could reach them. They go through the nls files now, the Chinese translation covers them, and the pseudo-locale file is regenerated complete. A new test fails on a `%key%` with no entry, a dead entry, or a stale key in a translation.
* Cleaned the sample mod's `.vscode/settings.json`, which pointed `cwtools.rules_folder` at a `D:\` path on another machine and set a `cwtools.rules_version` that isn't a setting. It made every host run pop a "rules_folder could not be found" warning.
* Cleared the npm advisory backlog, 24 down to zero. The `overrides` block had pinned `brace-expansion`, `fast-uri` and `undici` to exact versions that later became the vulnerable ones, so `npm audit fix` could never move them. Every override is a caret range now, and `diff` is forced to 8.x since mocha and sinon ask for `^7`, which has no patched release.
* Dependabot splits security fixes from routine version bumps into separate grouped PRs, so a security PR never queues behind a version bump. It also scans the composite action under `.github/actions/`, which was previously missed, and applies commit prefixes and a fixed weekly schedule.
* Added CodeQL analysis for the TypeScript client and for the workflows themselves, on push, on PRs and weekly. Nothing was scanning either before.
* PRs now run a dependency review that fails on a new high severity dependency, so a vulnerable package gets caught before merge instead of showing up as an alert afterwards.
* Added a SECURITY.md pointing reports at private vulnerability reporting rather than a public issue.

### 2.3.0

* No engine change. The original entry here claimed an upgrade to "v3.0.0 of the cwtools rust engine"; there is no such version, and the submodule pin was untouched, so 2.3.0 shipped the same v1.22.0 engine as 2.0.0 through 2.2.0. Corrected in 2.4.0, which does move the pin.

### 2.2.0

* The output panel no longer force-reveals itself when the server logs an error; the error still lands in the panel, but focus stays where you were. `getFileTypes` timeouts now log at info level instead of warn. File-list refreshes skip re-rendering when the incoming list is identical to what's already shown, and per-file diagnostics that are unchanged since the last publish are dropped client-side instead of re-applied (that cache is cleared on a server restart, so a fresh server's first batch always applies). (cwtools-vscode#95)
* The loadingBar progress notification reuses one status bar item, updated in place, instead of disposing and recreating it on every progress tick; the item is now also disposed on deactivate. (cwtools-vscode#96)
* The graph webview skips the node shadow pass above 300 nodes or below 0.4 zoom, where the blur was invisible but still cost a full render pass, and caches draw labels and hover-neighborhood lookups instead of recomputing them every frame or every hover. (cwtools-vscode#96)
* Housekeeping: deduped the FNV-1a hash used by the diagnostics and file-list signatures into a shared module, removed a tautological `shouldRefreshFileList` wrapper in favor of the inline comparison, and added a committed `clientPerf` benchmark for the client's hot-path functions, run with `npm run bench:node`. (cwtools-vscode#96)

### 2.1.0

* Activation no longer blocks on the first-run rules download. The extension used to wait for the rules download (git clone/pull, up to a minute on first run or a slow network) behind a progress notification before the language server would start. Now the server starts immediately against whatever rules are already on disk, the clone/pull runs in the background, and once it lands the server reloads the rules in place, no window reload needed. The editor is usable right away on first launch. (cwtools-vscode#93)
* The graph view scales to graphs with thousands of nodes. Edge styling and node/edge insertion were merged into a single pass (the old edge-highlight step was quadratic in nodes times edges), and tooltip DOM elements are now created lazily on first hover instead of one per node up front. Duplicate edges are now actually deduped (the old check compared object references and never matched), the cytoscape plugins register once instead of on every redraw, and image export was rewritten to use FileReader. (cwtools-vscode#93)
* Game detection no longer scans for the other eight games' executables once the workspace folder has already identified one, skipping the redundant globbing at activation with no behavior change.
* Housekeeping: removed the unused `GAME_DISPLAY`, `GAME_FOLDER` and `GAME_IDS` exports and a dead virtual-document provider that only ever returned an empty string.

### 2.0.0

* Consolidated the release manifest: `release/CHANGELOG.md` is no longer committed, since the build script regenerates it from the root `CHANGELOG.md`. The two had drifted out of sync.

### 1.21.0

* Upgraded to v1.23.0 of the cwtools rust engine.

### 1.20.0

* Removed the dead server-notification handlers (debugBar, createVirtualFile, promptReload, forceReload, promptVanillaPath) and the reload plumbing behind them. The engine only ever sends loadingBar and updateFileList. The `cwtools.reloadExtension` command is gone too; it was never in the palette, but keybindings or macros referencing it will now error. (cwtools-vscode#84)
* The graph commands are now namespaced: `showGraph`, `setGraphDepth`, `graphFromJson`, `saveGraphImage` and `saveGraphJson` became `cwtools.showGraph` and so on. Custom keybindings referencing the old bare IDs need updating. (cwtools-vscode#85)
* The `cwtools.localisation.hoverShowAllLanguages` setting now shows up in the settings UI; it was read but never declared.
* Release CI now builds the engine from the pinned submodule and fails unless it points at a tag, so a release can't silently bundle whatever engine main happens to be. The submodule is pinned to v1.22.0, the version 1.19.0 already shipped. (cwtools-vscode#83)
* Housekeeping: dropped the redundant onLanguage activation events, the FAKE-era build target aliases and two needless submodule checkouts in CI, and moved the ignore-settings mapping into a unit-tested module. (cwtools-vscode#87, cwtools-vscode#88)

### 1.19.0

* Fixed a feedback loop where a busy language server could make the extension spam "getFileTypes request timed out" and stay stuck: the 5s guard now actually cancels the in-flight request (instead of just giving up locally and leaving it queued on the server), and the editor-tracking retry backs off for a couple of seconds after a timeout instead of firing again immediately. (cwtools-vscode#90)
* The "did focus file" hint is no longer re-sent when the active editor is the file it was last sent for.
* Removed a file watcher on the extension's own rule cache that only generated redundant lint traffic against the server.
* Upgarded to v1.22.0 of the engine.

### 1.18.0

* A failed first-time download of the language rules now raises a warning notification, instead of silently leaving the extension with no rules and only a line in the output log. A failed offline refresh (rules already present) stays quiet.
* Documented the background reindex in the README: the idle-gated periodic rescan, the `Re-index workspace` command, and the `cwtools.backgroundReindex.intervalMinutes` setting.
* Refreshed the theming docs for the single merged `paradox` grammar (the per-game grammars were folded into it in 1.16.0).
* Removed the dead `cwtools.logging.diagnostic` setting, which did nothing.
* Housekeeping: bumped the bundled Rust engine submodule to v1.20.0, and made local and PR-CI vsix builds stamp the CHANGELOG version instead of the committed manifest version.

### 1.17.0

* Added a background reindex: the server re-scans the workspace on an interval (default 30 minutes, and only after you've gone idle), so files changed outside the editor and definitions moved between files no longer go stale until a window reload. `cwtools.backgroundReindex.intervalMinutes` tunes the interval; 0 disables it.
* Added a "Re-index workspace" command to run the same rescan on demand.
* `history/` script files are now watched, so external edits to them reach the server without a reload.

### 1.16.0

* Fixed identifier highlighting: a letter after a digit inside an id (`my_focus_2b`) was colored differently from the rest of the id. Dates, floats, negative numbers and real event ids (`civil_war.1`) are unaffected. (cwtools-vscode#73)
* Collapsed the nine vestigial per-game language IDs down to `paradox` and `cwt`, folding the per-game keyword grammars into the base `paradox` grammar. Highlighting is unchanged (no keywords or scopes lost) and the extension manifest is about 450 lines smaller.
* Removed dead command-palette entries and settings that no longer did anything, and wired the ignore and diagnostic-suppression settings (`ignore_patterns`, `errors.ignorefiles`, `errors.ignore`) through to the server so they take effect live. Added a manifest test that fails if a contributed command is not registered.
* Corrected the README: removed a stale code-action claim and added Victoria 2 and EU5 to the supported games.
* Upgrades to v1.19.0 of the Rust cwtools engine:
  * Completion no longer dumps saved variables where a specific value is expected (`add_tech_bonus` name, focus `x`/`y`, country flags); `mio:` scope keys are suggested inside effect blocks; effect and modifier completion is scope-aware; and `has_dlc` completion inserts a well-formed snippet that quotes DLC names. (cwtools-vscode#74, cwtools-vscode#75, cwtools-vscode#76, cwtools-vscode#77, cwtools-vscode#78, cwtools-vscode#79)
  * New editor features: document outline, folding, document highlight, and Find All References and Rename across closed files.
  * New localisation-stub (`genlocall`) and rules-reload (`reloadrulesconfig`) commands, plus diagnostic suppression by error code.

### 1.15.0

* The extension now activates only in Paradox mod workspaces instead of every window. Activation triggers on a Paradox language, a `descriptor.mod` / `.metadata/metadata.json`, or a game executable or content folder at the workspace root, and the cwtools file view only appears once activated. (Opening a lone game script with no folder open no longer auto-activates, which previously only surfaced the "open the mod folder" warning anyway.)
* Fixed Crusader Kings III and Victoria 3 mod folders being detected as CK2/Vic2: the 2-numbered folder hint matched first, so a "III" folder substring-matched the "II" game. 3-suffixed games are now checked first.
* The graph view reuses its webview when you change depth instead of tearing it down and re-parsing the 4.6MB bundle each time, so redraws are much faster. Game-exe detection, tooltips and node lookups were also trimmed along the way.
* The rules folder download (git clone/pull, up to a minute on first run) now runs under a progress notification instead of appearing to hang silently.
* Upgrades to 1.18.0 of the Rust cwtools engine.
  * Supports stellaris
  * Auto completion improvements
  * Memory improvements + performance improvements

### 1.14.0

* Fixed loc highlighting: text left over outside a quoted value (e.g. a stray character after the closing quote, or bare text instead of a quoted value) was colored as part of the string. It's now flagged in red as invalid, across all bundled themes. (cwtools-vscode#70)

### 1.13.0

* Fixed localisation `.yml` highlighting eating across lines on an unterminated string. (cwtools-vscode#59)
* Updated the cwtools engine with autocomplete, linting and go-to-definition fixes:
  * Localisation now flags unterminated quotes and invalid keys. (cwtools-vscode#59)
  * Better autocomplete context-awareness in alias blocks; scripted effects and dynamic modifiers are suggested; boolean aliases complete with `= yes/no`; duplicates removed. (cwtools-vscode#60, cwtools-vscode#64, cwtools-vscode#65, cwtools-vscode#66, cwtools-vscode#67)
  * Go-to-definition no longer shows duplicate results. (cwtools-vscode#62)
  * Missing required-field warnings point at the block key. (cwtools-vscode#63)

### 1.12.0

* Updated to v1.15.0 of the cwtools engine
  * The cwtools engine upgrade focus on math expressions support for Hearts of Iron IV + improve auto completion and context awareness

### 1.11.2

* Updated to v1.14.1 of the cwtools engine improving auto completion robustness and snappiness

### 1.11.1

* Improved minimalism of the classic theme.

### 1.11.0

* Improved localisation highlighting.
* Improved themes for various theme structures.
* Improved intellisense for localisation as a whole.
* Updated to v1.13.0 the cwtools engine.

### 1.10.0

* Added dedicated highlighting for Paradox localisation `.yml` files: a new `paradox-localisation` language, scoped to `localisation`/`localisation_synced`/`localization` folders so ordinary YAML is untouched. Strings now run to the last quote on the line (an embedded `"` or `#` no longer breaks coloring), `KEY:0` version suffixes and keys with no leading whitespace parse correctly, and `[commands]`, `$references$`, `§` colour codes and `£icons` are highlighted. It reuses the Paradox script grammar's scope names, so the bundled themes and editor defaults color it with no theme changes. (cwtools-vscode#56)
* Loc hover tooltips no longer append a trailing `#` comment, and no longer truncate a value that legitimately contains a `#`. (cwtools-vscode#50)
* Loc hover now falls back to the vanilla string for keys defined in the base game but not the mod, and refreshes live as you edit `.yml` files instead of needing a reload. (cwtools-vscode#51, cwtools-vscode#53)
* Go-to-definition follows a focus/event/decision after it is moved to another file, without a window reload. (cwtools-vscode#52)
* `visible`/`available` blocks in decisions now complete triggers instead of effects. (cwtools-vscode#57)
* Added the `## default_bool = yes|no` rule directive: a bool field explicitly set to its declared default gets an info-level hint (CW282) that the line can be omitted. (cwtools-vscode#26)

### 1.9.0

* Reworked the bundled themes. Every theme now paints the full scope set from both grammars (game scripts and `.cwt` rule files) plus a generic baseline, so coloring no longer falls flat in `.cwt` files or on the scopes most themes used to miss.
* Renamed the themes to `Paradox - <name>` (`Paradox - Nord`, `Paradox - Kate`, ...). If you had one of the old names set, reselect it in the Color Theme picker.
* Retuned `Paradox - Kate`/`Kate Light`, the `High Contrast` pair, and `Paradox - Syntax` (now a Dark+ flavored default), and converted `Paradox - Dimmed` and `Paradox - Quiet Light` to the full theme format while keeping their look.
* `.cwt` grammar: rule keys on the left of an assignment (`key = ...`) are now scoped, so they pick up theme colors instead of rendering as plain text.
* Added a node test that fails if any shipped theme leaves a grammar scope unstyled.

### 1.8.0

* Updated to v1.12.0 of the cwtools engine.

### 1.7.0

* Vendored the TextMate grammars from [cwtools/paradox-syntax](https://github.com/cwtools/paradox-syntax) directly into the extension, so highlighting ships with `cwtools-md-edition` and no longer needs the [tboby.paradox-syntax](https://marketplace.visualstudio.com/items?itemName=tboby.paradox-syntax) extension pack.
* Added a themes block: `Paradox-Syntax` (minimal), `Paradox-Kate` and `Paradox-Kate-Light` (modeled on the Kate syntax's `defStyleNum` token categorization), `Paradox-Nord` (Nord palette), `Paradox-HighContrast` and `Paradox-HighContrast-Light` (accessibility), plus the upstream `Paradox-Dimmed` and `Paradox-QuietLight` re-homed here.
* Added [`tools/sync-paradox-syntax.sh`](tools/sync-paradox-syntax.sh) to re-vendor the upstream grammars on demand without touching the themes.
* The per-game `stellaris`/`hoi4`/`eu4`/`ck2` grammars are intermediate. The end state is a single merged `paradox.tmLanguage.json` with game-specific keywords injected on top; this release keeps the per-game split for now to make the upstream diff trivial to track.

### 1.6.0

* Updated to v1.11.0 of the cwtools engine.

### 1.5.0

* Updated to v1.10.0 of the cwtools engine.

### 1.4.0

* Updated to v1.9.0 of the cwtools engine

### 1.3.4

* Updated to v1.8.4 of the cwtools engine.
* Fixed the hover for a resource trigger (`oil`, `steel`, …) showing the wrong tooltip: it read "Check ratio of this type of unit for commander" with scopes `unit_leader`/`combat` instead of "Check amount of resource state or country has" with scopes `country`/`state`. The engine was skipping `common/resources` when it indexed the mod, so resources were never recognized and the hover fell through to an unrelated rule. It now indexes `common/resources` and resolves the resource trigger correctly.

### 1.3.3

* Updated to v1.8.3 of the cwtools engine.
* Autocomplete now finishes effects and triggers as usable snippets. Block ones like `if` complete to `if = { limit = { } }` with the brackets and required fields filled in and tab stops to move between them; value ones like `add_political_power` complete to `add_political_power =` with the cursor ready for the value.

### 1.3.2

* Updated to v1.8.2 of the cwtools engine.
* Fixed a parser bug where a names/callsigns list mixing quoted and unquoted entries (e.g. `{ "Sunshine" Demon }`) reported a false "unclosed clause" error and dropped the rest of the file. Affected `common/names` and unit name files.
* Fixed resource triggers (`oil`, `steel`, …) being wrongly flagged "used in incorrect scope … expected combat or unit_leader" when used in a state scope.
* A numeric state-id block (`129 = { ... }`) now resolves to the state scope, so triggers and effects inside it (and the hover) show state. `random_list` weight buckets keep the surrounding scope.

### 1.3.1

* Updated to v1.8.1 of the cwtools engine.
* Adding a localisation key now clears the missing-localisation warning on the event (or other game file) that uses it as you type, instead of only after a window reload or rescan.
* Fixed the status bar getting stuck on "Indexing workspace…" when the scan hit an empty workspace or an error mid-scan.

### 1.3.0

* Updated to v1.8.0 of the cwtools engine.
* `.cwt` rule config files now open as their own language with dedicated syntax highlighting, and are linted against the loaded ruleset (undefined type, enum, or single_alias references) instead of being validated as game script. Opening the rules folder no longer floods every field with errors or hangs on indexing.
* Added the `cwtools.hover.scopeDisplay` setting. `resolved` adds a `Resolves to` line to hover tooltips showing the scope a link or FROM/ROOT/PREV keyword evaluates to; `context` (default) shows only the current scope.
* Go-to-definition and double-click treat a dotted event/decision id (`namespace.1`) as one word instead of splitting at the dot, so Ctrl+Click jumps to the event or decision.
* Hovering an event id shows its localised title.
* Localisation errors appear and clear as you type instead of only after reloading the window.
* Autocomplete recovers to real rule suggestions after a partial edit instead of getting stuck on generic word suggestions.
* Hover tooltips separate their sections (documentation, required scope, current scope) with a divider for readability.

### 1.2.1

* Updated to v1.7.1 of the cwtools engine

### 1.2.0

* Updated to v1.7.0 of the cwtools engine.
* Fixed `cwtools.rules_folder` being ignored on Windows: backslash paths, `~`, `%VAR%` environment variables, quoted values, and workspace-relative paths now resolve instead of silently cloning the upstream rules.
* Changed the extension to warn (log + popup) when `cwtools.rules_folder` is set but the folder can't be found, instead of silently falling back to the bundled rules.
* Fixed the `cwtools.rules_version` setting description to match its actual behavior.

### 1.1.0

* Order of battle references (`load_oob`, `oob`, `set_naval_oob`, `set_air_oob`) resolve on Windows again instead of being reported as missing from `history/units`.
* `NOT = { AND = { ... } }` is no longer flagged as an unnecessary AND. HOI4 `NOT` acts as a NOR, so the AND is a meaningful NAND.
* An `AND` inside a `count_triggers` block is no longer flagged as unnecessary. Each direct child is a separately counted condition, so the AND groups several into one counted unit.
* A localisation value that embeds an inline `[...]` command with a literal suffix (e.g. a `meta_effect` variable `"[?ROOT...GetTokenKey]_subtype"`) is no longer flagged as undefined localisation. It resolves at runtime.
* Built-in game variables like `faction_leader` work without the `var:` prefix instead of being flagged as unset.
* Event and news pictures set through scripted localisation (`picture = "[SomeFunction]"`) are no longer flagged as an unknown sprite.
* Localisation `$...$` references to dynamic modifiers, game objects, and script variables no longer show as undefined. Real typos still do.
* Filepath references with a redundant double slash (`gfx//interface/...`) resolve the way the engine treats them.
* Windows: trigger and effect documentation tooltips (the `###` lines), Ctrl+Click, and validation now work for files whose paths use backslashes.
* Hover tooltips show the current scope at the cursor.
* Hover and Ctrl+Click work on nested `$KEY$` references inside localisation .yml files. Hover shows the referenced entry's text, Ctrl+Click jumps to it.
* A broken rules config is flagged: a `.cwt` rule that references an undefined type, enum, or single_alias reports an error on the offending line.
* Autocomplete on a field key inserts `name =` with the cursor after the `=`, and works on a fresh line after a field again (it was only offering plain word suggestions, most noticeable in shared_focus and focus files).
* Objects whose type declares required localisation are flagged when that loc key is missing, so missing localisation is visible again.

### 1.0.22

* First stable (non-preview) release of CWTools MD Edition.

### 1.0.13-beta

* Fix release pipeline: the staging step referenced a removed `osx-x64` artifact after the macOS build switched to native ARM. Extracted staging into a platform-agnostic script so adding or removing platforms only requires editing the CI matrix.

### 1.0.9-beta

* Bundle the client with esbuild. The vsix now ships two bundles instead of the full `node_modules` tree, dropping it from 353 files to 90 (173 JS files to 2) and speeding up activation.
* Internal cleanup with no user-facing change: removed dead build artifacts and the unused Paket tooling, unified the five host-test configs into one, aligned the toolchain version pins, and moved the build/test helper scripts to TypeScript.

### 1.0.8-beta

* Millennium Dawn fork (CWTools MD Edition). The changes below are relative to upstream 0.10.31.
* Rust language server (cwtools-rs) is the only engine, shipped as a standalone per-platform binary.
* Linux, macOS, and Windows server binaries are packaged into a single vsix.
* Tag-driven release pipeline: push a `v*` tag to build, package, and publish the GitHub release.
* Rebranded to CWTools MD Edition under the `milleniumdawnmodteam` publisher.

### 0.10.31

* Stellaris: Allow "(", ")" as values, to allow parsing (but not proper support for) `@[()]`
* Fix a bug with document symbols

### 0.10.30

* HOI4: Add country_metadata, music, portraits, and sound folders to cache

### 0.10.29

* Add "integrated_dlc" directory

### 0.10.28

* Add basic support for EU5
* Fix DLC references
* Fix completion replacement range

### 0.10.27

* Remove unused config settings
* Added chinese localisation for extension setings and actions
* Various performance improvements

### 0.10.26

* Fix stellaris log parsing
* Fix empty zip crash (thanks @nikitalita!)
* Updated to net9.0 (security + performance)

### 0.10.25

* Add better warning when localisation contains characters that are possibly invalid

### 0.10.24

* Stellaris: Even more optimisation related to scripted_effects/triggers

### 0.10.23

* Stellaris: Reduce CPU usage related to scripted_effects/triggers

### 0.10.22

* Try to reduce CPU usage

### 0.10.21

* Fix extension highlighting

### 0.10.20

* Stellaris: Remove some obsolete validators

### 0.10.19

* Fix default values in settings (vanilla shouldn't validate by default anymore)
* Add new setting for enabling/disabling diagnostic logging from cwtools server
* HOI4: More performance improvements

### 0.10.18

* HOI4: Significant performance improvements
* Stellaris: inline_scripts now show related errors and don't go away when you open the inline_script file

### 0.10.17

* Stellaris: inline_scripts will now show errors caused by params

### 0.10.16

* Stellaris: Fix trigger docs parsing

### 0.10.15

* Stellaris: Add limited (and flawed!) support for inline_scripts

### 0.10.14

* Try and fix osx

### 0.10.13

* Double performance of cwtools

### 0.10.12

* Attempt to fix failure to start extension when a non-script file is open when the extension loads
* Slight performance improvements

### 0.10.11

* EU4: Support global_event_target in localisation commands

### 0.10.10

* Fix EU4 encoding confusion

### 0.10.9

* Support parsing "?=" as an operator

### 0.10.8

* Handle corrupt ZIP files

### 0.10.7

* Rules: Hotfix previous

### 0.10.6

* Rules: support type-based modifiers generated from rules in stellaris

### 0.10.5

* VIC3: support type-based modifiers generated from rules
* VIC3: Warn when a modifier is used but not defined in modifier_types

### 0.10.4

* Rules: Support `replace_scope` on top level type rules

### 0.10.3

* EU4: Add PREV_PREV
* Completion will now wrap items in quotes when necessary
* Fix spurious "tree view" errors

### 0.10.2

* Fix publishing error

### 0.10.1

* Victoria 3: Fix vanilla caching

### 0.10.0

* Victoria 3: Add initial support

### 0.9.16

* Stellaris: Remove obsolete localisation validation
* Stellaris: Add new languages (Korean/Japanese)

### 0.9.15

* Further completion performance improvements
  * If <2000 possible items then full VS Code fuzzy matching will apply
  * If >2000 possible items it will only suggest items where the text so far is contained in the item
* Fix completion for items you are only supposed to have one of
* Stellaris: Support parameter default value syntax on LHS
* Stellaris: Fix some missing underscores in modifiers

### 0.9.14

* Hotfix: Fix completion after tabs

### 0.9.13

* Support alias_key_field in links
* Stellaris: remove old forced scope changes for variable effects

### 0.9.12

* Hotfix: excessive debug logging

### 0.9.11

* Stellaris: Add support for script_value param syntax (value:something|PARAM|Value)
* Stellaris: Add validation for script_value param usage, checking if the param used will cause an error
* Significantly improved performance for completion (most notable in CK3)

### 0.9.10

* Stellaris: Fix auto-generated modifiers

### 0.9.9

* Stellaris: Suppress `[[FLAG]]` errors
* Fix occasional extension corruption

### 0.9.8

* Hotfix: Add prev/this/root/from to completion

### 0.9.7

* Hotfix: Allow "@" variables in value fields

### 0.9.6

* Add intelligent completion for scope/variable dot chains on the RHS
* Add different icons in the completion list for scopes/variables/values
* Add support for Stellaris variables and triggers
* Fix Stellaris script docs parsing

### 0.9.5

* Fix longstanding issues with newer linux distros
* Fix "tree-view" error
* 20-30% performance improvement

### 0.9.4

* Improved modifier support for stellaris
* Event targets in stellaris localisation

### 0.9.3

* Expose localisation links into config files (see stellaris)
* Expose scope groups into config files (see stellaris)

### 0.9.2

* Fix CK3 vanilla cache

### 0.9.1

* Fix CK3 path

### 0.9.0

* Add basic CK3 support
* Add simple CK3 GUI file support

### 0.8.41

* Rules: Add `## error_if_only_match` for custom error messages
* Rules: Add `should_be_used` for types, errors if type not used anywhere (experimental)

### 0.8.40

* Rules: Improve `alias_key_field`
* Rules: Add `variable_field_32` and `int_variable_field_32` for variables limited to 3 decimal places
* Rules: Fix subscopes

### 0.8.39

* Fix formatting on save
* Stellaris: Fix federation and tile scopes

### 0.8.38

* Add a setting for the placeholder localisation text
* Add stellaris federation scope
* Add "Export all types" command

### 0.8.37

* Hotfix redundant OR

### 0.8.36

* Support simple chinese
* Add redundant AND/OR validator to HOI4

### 0.8.35

* Fix random freezing when performing many actions in short succession

### 0.8.34

* Add `only_if_not` option for subtype, to add dependancies on other subtypes
* Fix DLC zips

### 0.8.33

* Add `datetime_field` for `YYYY.MM.DD.HH`
* Support DLC zips
* Support dynamic values, e.g. values that are set with `_@ROOT` and used as `_FRA`
* Support multiple complex enum defs

### 0.8.32

* Remove mixed block valdidator
* Rules: Add support for mandatory quotes in rules
* Rules: Add `localisation_inline` field, which is intended for things like `set_blah_name` where you don't want a quoted loc key.
* Fix HSV/RGB caps

### 0.8.31

* Increase missing quotes to warning
* HOI4: Better handle event_target variables
* HOI4: Increase validate loc character range to include more western
* HOI4: Handle `?` in variables
* HOI4: Support `?10` numbers in loc

### 0.8.30

* Better completion in files such as `on_actions`
* Fix floats formatting
* Supress localisation missing ref errors when dollars are used... as dollars
* HOI4: Handle starting `?` in loc commands
* CK2: Minor fix to titles

### 0.8.29

* Fix saving graph as image
* HOI4: Validate localisation
* Rules: Add `= ignore_field` which will ignore the subtree on the right
* Localisation: Validate that localisation starts and ends with quotes

### 0.8.28

* Significant performance improvement for large mods (30%)
* Support `^` on the LHS
* Rules: Allow `enum[]` in predefined variables
* Misc: Update Libgit2, removing the dependecy on libcurl3 on linux

### 0.8.27

* Add maxfilesize setting
* Improved performance for large files
* Rules: Add better support for flags

### 0.8.26 (skipped 0.8.25)

* Fixes for dynamic predefined values
* Naive support for HOI4 `?`, `^` and quoted scope changes

### 0.8.24

* Rules: Add support for dynamic predefined values (party_popularity@<ideology>)

### 0.8.23

* Imperator: Support jomini interface script structures
* Imperator: Add completion in localisation

### 0.8.22

* Add "Go to definition" for localisation keys
* Rules: Suppport the HOI4 array "^"
* Rules: "Go to definition" and hover works better for complicated types

### 0.8.21

* Rules: Add `## cardinality ~1..2` where the `~` means that values between 0 and 1 only show a warning
* Stellaris: Improved econ category modifiers

### 0.8.20

* Stellaris: Add "stellaris_name_format" rule field for random_names
* Rules: Add "alias_keys_field\[trigger\]"

### 0.8.19

* Hotfix scripted effect param validator performance
* Rules: Standardise path definitions between types and enums

### 0.8.18

* Add document formatting
* Rules: Recursively check subtypes

### 0.8.17

* Intelligently determine errors for scripted effects based on actual usage (Stellaris/EU4)
* Add "Set graph depth" to graphs, controlling how far back links are displayed
* Stellaris: Validate that all economic categories have an ai_budget

### 0.8.16

* Add "zoomSensitivity" setting for graph scroll speed
* Add support for configuring graphs from config files

### 0.8.15

* Significantly improved performance across the board
* Some tweaks to graph display

### 0.8.14

* New feature: Event graphs
Press "Show event graph" in an event file in order to visualise your events
* Add "Save graph to image"
* Add "Save graph to json", as well as "Recreate graph from json"
* Double click on nodes to go to event definition
* Hover over nodes to see defined event targets, flags, etc

### 0.8.13

* Hotfix

### 0.8.12

* Fix EU4 trade node scope
* Better default language settings

### 0.8.11

* Fix EU4 localisation commands

### 0.8.10

* Remove old Stellaris 2.1 validators
* Add warning for "If" without any effects inside it
* Rules: Add support for config defined scopes (which were previously hardcoded)

### 0.8.9

* Vic2: Better Vic2 vanilla detection
* Improved completion and hover for certain types of files (type_per_file = yes)

### 0.8.8

* Rules: support multiple skip_root_key

### 0.8.7

* Support workspaces (opening multiple folders in the same vscode window)
* Opening vanilla files at the same time as your mod no longer causes problems
* F12 (go to definition) on vanilla things in your mod is now safe and will show you the vanilla file!

### 0.8.6

* Bugfix reading log files from rules

### 0.8.5

* Smaller, faster cache files
* Add stellaris 2.3 scopes
* Add stellaris scripted effect params ($PARAM$)

### 0.8.4

* Pre_trigger code actions for stellaris
* Initial Vic2 support
* Add "unique" keyword for rule types that enforces just one definition
* Complex enums now properly recurse

### 0.8.3

* Fix localisation caching for Imperator
* Add configu rule support for "filepath[prefix]" and "ir_family_name_field"

### 0.8.1

* Fix EU4 scopes

### 0.8.0

* Surprise! Day 1 Imperator support!

### 0.7.14

* Update Stellaris to 2.2.6

### 0.7.13

* Fix links

### 0.7.12

* Add support for rule-based event target links in rule config

### 0.7.11

* Minor performance improvements

### 0.7.10

* Add support for event target links in rule config

### 0.7.9

* Hovering over fields will now show documentation where it exists
* Show subtype localisation in tooltips
* Further improvement to CK2 titles
* CK2 provinces

### 0.7.8

* CK2 landed titles

### 0.7.7

* Further improve CK2 localisation
* Add "starts_with" setting for config rule types
* Support multiple "path" settings for config rule types
* Support config rule types localisation being references to loc keys inside

### 0.7.6

* Improve CK2 localisation

### 0.7.5

* Fix CK2 loading
* Only load cached files if necessary
* Add CK2 DNA/Properties

### 0.7.4

* Better handling of changes to localisation files (update validation properly)

### 0.7.3

* Add support for CK2

### 0.7.2

* Validation context switches in localisation
* Reduce errors in debug output

### 0.7.0

* Complete EU4 support
* Prevent validation of vanilla files
* Make errors a little clearer

### 0.6.24

* Add completion for `@` script variables
* Add more detailed localisation in the hover tooltip for types (mainly eu4 for now)
* Sort completion list, moving items used in the same file to the top

### 0.6.23

* Significant performance improvements

### 0.6.22

* Enabled Code Outline

### 0.6.21

* Configure localisation commands from a config file called `localisation.cwt`
* Add modifier categories for HOI4

### 0.6.20

* Support other languages for EU4/HOI4

### 0.6.19

* Fix localisation of types
* Validate on swapping file

### 0.6.18

* Add "manual" setting for "rules_version", which combined with "rules_folder" to use a specific folder for config rules
* Provide completion for "event_target:"
* Add error for localisation that refers to itself
* Add stellaris building cap modifiers

### 0.6.16/17

* Add vanilla caching process for EU4 and HOI4

### 0.6.15

* Performance improvements
* Validation of localisation strings for EU4

### 0.6.14

* Add localisation for EU4/HOI4

### 0.6.13

* Update to 2.2.2

### 0.6.12

* Improved scope checking for effects/trigger/modifiers

### 0.6.11

* Automatically restart on rules update
* Improved validation of flags

### 0.6.10

* Add a warning when a file is opened instead of a mod folder
* Add features for rules: localisation can now be defined on types

### 0.6.9

* Add features for rules: `variable_field`

### 0.6.8

* Add some generated 2.2 modifiers

### 0.6.7

* Update to Stellaris 2.2

### 0.6.6

* Performance hotfix

### 0.6.5

* Add `folders.cwt` to manually override folders to be validated

### 0.6.4

* Fix crash when cachefile missing

### 0.6.3

* Add ability to cache vanilla files for Stellaris/EU4/HOI4 to use instead of embedded files
  Ctrl-Shift-p "cwtools: Generate cache"
* Remove requirement for git
* More fixes
* More hotfixes

### 0.6.2

* Basic HOI4 support

### 0.6.1

* Hotfixes!

### 0.6.0

* Enabled rule-based validation by default. Every file in your mod now gets detailed validation!
* Enabled completion by default. Every file in your mod should get intelligent auto-completion!
* Added auto-updating rules, controlled by the setting `cwtools.rules_version`.

### 0.5.38

* Updated rules
* Potential fix for linux/osx
* Reduced severity of localisation naming validators
* Add `severity` option for rules to manually set the severity of that rule (normally lower, from error to warning)

### 0.5.37

* Improved autocompletion (should now work everywhere)
* Increased resiliancy to errors, now won't restart on error
* Performance improvements

### 0.5.36

* Performance improvements and hotfix

### 0.5.35

* Add "Reload config rules" command to reload config rules

### 0.5.34

* Hotfix vanilla embedded files
* Performance improvements

### 0.5.33

* Fix locale issue with floats
* Fix scope hover info
* Slightly improve startup speed
* Fix crash on tooltip hover
* Add validation of localisation file encoding, header and name

### 0.5.32

* Performance improvements
* Hotfix some rules

### 0.5.31

* Improved config base validation
* Validation of value clauses
* Fix prescripted_countries embedded content
* Include syntax highlighting by default

### 0.5.30

* Update to 2.1.2
* Reduce flag usage to a warning
* Improve flag usage validator coverage
* Add megastructure/planet_class model validator

### 0.5.29

* Add flag usage validator

### 0.5.28

* Update config rules

### 0.5.27

* Fix completion

### 0.5.26

* Update config rules

### 0.5.25

* Add config rules for interface/gfx/sounds/music/fonts
* Add `path_strict` option for types which prevents subfolders being searched
* Add `severity = warning` option for types which reduces errors to warnings
* Add Alliance scope
* Add `percentage` value for config rules

### 0.5.24

* Reduce false-positives for localisation_synced references
* Add remaining missing scope commands
* Update config rules

### 0.5.23

* Add improved redundant AND and OR checks
* Update config rules

### 0.5.22

* Add "Find all references" (currently only works when right clicking on a reference)
* Update config rules with most of A-M

### 0.5.21

* Update config rules

### 0.5.20

* Add "Go to definition" (currently only works for mod defined types)
* Support "FROM" scopes
* Add `replace_scope` for config rules
* Add unnecessary AND validator

### 0.5.19

* Add basic support for HOI4

### 0.5.18

* Enable improved completion by default

### 0.5.17

* Add command "Generate missing loc for all files"

### 0.5.16

* Significantly improved general performance
* Validated anomaly localisation

### 0.5.15

* Fix completion in multi-mod projects
* Improve performance of config based validation
* Fix negative value parsing

### 0.5.14

* Add tile blocker localisation
* Add static_modifier desc localisation
* Add `push_scope` for config rules

### 0.5.13

* Add planet_killer localisation and other validation

### 0.5.12

* Add scope information tooltip
* Add support for `scope` rules
* Update config rules

### 0.5.11

* Update config rules
* Add support for localisation_synced rules

### 0.5.10

* Update config rules
* Add support for <type.subtype>

### 0.5.9

* Update config rules

### 0.5.8

* Add support for 'complex_enum'

### 0.5.7

* Bugfix <types>

### 0.5.6

* Add support for `localisation` value for cwt config

### 0.5.5

* Add support for more complicated keys in aliases for cwt config
* Add support for `type\_key\_filter` on subtypes for cwt config

### 0.5.4

* Add support for multiple rules with the same key for cwt config

### 0.5.3

* Add support for left hand side type values for cwt config (e.g. `<technology> = bool` for `tech_something = yes`)

### 0.5.2

* Add support for left hand side values for cwt config (e.g. `int = { }` for random_list `1 = { }`)

### 0.5.1

* Update config files
* Add support for .cwtools folder with config files in it

### 0.5.0

* Add support for new config file format, see <https://github.com/tboby/cwtools/wiki/.cwt-config-file-guidance>
* Check correct ordering of if/else/else_if

### 0.4.28

* Support opening a folder containing `.mod` files (that isn't the mod folder)
* Remove errors from list when a file is deleted

### 0.4.27

* Add nested if/else effect deprecation warning
* Add temporary if/else effect ambiguity warning

### 0.4.26

* Update to 2.1.0
* Add solar\_system\_initializer star class check
* Add anomaly migration warning
* Update validation to new anomaly format

### 0.4.25

* Add experimental completion

### 0.4.24

* Reduce memory usage
* Support syntax highlighting
* Fix vanilla syntax errors

### 0.4.23

* Add info for "REPLACE_ME" localisation
* Add localisation checks for starbase\_buildings, starbase\_modules, starbase\_types and special\_projects

### 0.4.22

* Sort generate localisation by order in file
* Override trigger\_docs and show error for pop/tile use of set\_variable and similar

### 0.4.21

* Update to stellaris 2.0.4

### 0.4.20

* Improved icon validators
* Ensure a final validation happens after changes stop

### 0.4.19

* Handle mod overwriting of vanilla files
* Add localisation tooltips
* Promote ship design validation from experimental
* Add mesh validation (experimental)
* Add graphical entity validation (experimental)
* Add ship\_size/component/section graphical entity validation (experimental)

### 0.4.18

* Add temporary error for 2.0.2 bug with after
* Add validation of ship designs, check that section slot type and component type match (behind experimental flag)
* Warn if tech has no effect (and has weight > 0)
* Fix .yml parsing bug

### 0.4.14

* Add validation of $refs$ in localisation
* Add validation of [commands] in localisation
* Add a command to create a .csv of all errors
    To use, press ctrl-shift-p, then "write all errors"

### 0.4.13

* Revert a change to localisation parsing

### 0.4.12

* Update to stellaris 2.0.2
* Check for ambiguous use of NOT operators

### 0.4.11

* Add localisation checking for opinion modifiers
* Fix localisation checking for war goals
* Check modifiers for opinion modifiers

### 0.4.10

* Significant performance improvements
* Display syntax errors immediately while validation is ongoing

### 0.4.9

* Add yes/no to autocomplete
* Add weights checking for
  * Event options
  * Agendas
  * Anomalies
  * Ascension perks
  * Bombardment stances
  * Buildings
  * Component templates
  * Country Customization
  * Country types
  * Deposits
  * Edicts
  * Ethics
  * Governments
  * Megastructures
  * Observatation station missions
  * Personalities
  * Planet modifiers
  * Policies
  * Pop faction types
  * Section templates
  * Species rights
  * Starbase buildings
  * Starbase modules
  * Starbase types
  * Technologies
  * Terraform
  * Tile blocks
  * Traditions
  * War goals

### 0.4.8

* Add localisation checking for war goals
* Add localisation checking for custom\_tooltip everywhere

### 0.4.7

* Correctly validate effects/triggers inside if, while and event_targets

### 0.4.6

* Fix bug with event\_target check when you reference an event that doesn't exist

### 0.4.5

* Improve event\_target checking to handle loops (still behind experimental setting)

### 0.4.4

* Add file "ignore" list to filter filenames that won't return validation errors
* Stop validation "random_names" as it's so non-standard

### 0.4.3

* Improve responsiveness of validation when make rapid changes (by only checking the latest version of a file not every changed version)
* Add scripted_effect event_target checking (still behind experimental setting)

### 0.4.2

* Add (experimental) event_target checking (hidden behind experimental flag)
* Add tooltip info for scripted_effects/triggers (taken from comments above definition)

### 0.4.1

* Add (experimental) modifier existence and scope checking for:
  * Buildings
  * Agendas
  * Ascension perks
  * Component templates
  * Edicts
  * Ethics
  * Governments
  * Policies
  * Ship sizes
  * Species rights
  * Starbase buildings
  * Starbase modules
  * Strategic resources
  * Technologies
  * Tradition categories
  * Traditions
  * Traits

### 0.4.0

* Add OSX support
* Update embedded interface to 2.0
* Significantly improve performance for large mods
* Add effect and trigger validation for:
  * Anomalies
  * Armies
  * Ascension perks
  * Bombardment stances
  * Buildable pops
  * Buildings
  * Bypasses
  * Casus belli
  * Diplomatic actions
  * Edicts
  * Ethics
  * Mandates
  * Megastructures
  * Observation stations
  * Personalities
  * Policies
  * Pop faction types
  * Ship sizes
  * Solar system initializers
  * Species rights
  * Starbase buildings
  * Starbase modules
  * Starbase types
  * Start screen messages
  * Subjects
  * System types
  * Technology
  * Terraform
  * Tradition categories
  * Traditions
  * Traits
  * War goals

### 0.3.3

* Fix bug with scope usage through prev
* Add simple scope commands to autocomplete
* Properly parse hsv/rgb with 4 values

### 0.3.2

* Check scope usage through "AND", "OR", etc
* Properly parse files with only values
* Properly parse "rgb"
* Give better feedback for parser errors when there is no matched brace

### 0.3.1

* Update localisation to 2.0

### 0.3.0

* Add simple autocomplete (triggers, effects, and modifiers)
* Add documentation for autocomplete (usage information and supported scopes)
* Add action to generate .yml for missing localisation keys
* Ignore missing keys that are just scripted loc (e.g. "[GetName]")
* Check correct scope of static modifiers use in has_modifier and remove_modifier

### 0.2.19

* Support scripted_variables
* Fix bug with checking variables in nested blocks
* Check correct scope of static modifiers use in add_modifier effects

### 0.2.18

* Add Stellaris 2.0 scopes
* Set 2.0 trigger_docs as default

### 0.2.17

* Check effectFile and textureFile against actual files, throw an error if the file isn't found

### 0.2.16

* Add "cwtools.errors.ignore" setting to allow ignoring of specific error codes
* Handle "hidden" prefix on scopes

### 0.2.15

* Update scope checking to support PREV and check inside scopes
* Check effects and triggers in more parts of events (options, desc triggers)

### 0.2.14

* Add experimental option to enable experimental features
* Added 2.0 effects/triggers behind experimental flag

### 0.2.13

* Check button_effects used in .gui files are defined
* Check spriteTypes used in .gui files are defined

### 0.2.12

* Fix ambient_object localisation checks
* Support recursive triggers and effects

### 0.2.10

* Add localisation_synced checking in events

### 0.2.9

* Add localisation for pop\_faction\_types
  * pft
  * pft plus "_desc"
* Add localisation for static_modifiers
  * modifier
  * modifier plus "_desc"
* Add localisation for spaceport modules
  * "sm_" plus module
* Add localisation for traits
  * trait
  * trait plus "_desc"
* Add localisation for governments
  * government key
  * ruler\_title, ruler\_title\_female, heir\_title, heir\_title\_female
  * civic
  * civic plus "_desc"
  * civic description
* Add localisation for personalities
  * "personality_" plus personality
  * peronality plus "_desc"
* Add localisation for ethics
  * ethics
  * ethics plus "_desc"
* Add localisation for planet_classes
  * planet class
  * planet class plus "_desc"
  * if colonizable
    * planet class plus "_tile"
    * planet class plus "\_tile\_desc"
    * "trait\_" plus planet class plus "\_preference"
    * preference plus "_desc"
    * planet class plus "_habitability"
* Add localisation for edicts
  * "edict\_" plus edict name
  * "edict\_" plus edict name plus "\_desc"
* Add localisation for policies
  * "policy\_" plus policy
  * "policy\_" plus policy plus "\_desc"
  * policy option name
  * policy option name plus "\_desc"
  * policy option flags
* Add more localisation for technology
  * feature_flags
  * feature\_flags + "\_desc"
* Add localisation for section_templates
  * key
* Add localisation for species\_name
  * "\_desc"; "\_plural"; "\_insult\_01"; "\_insult\_plural\_01"; "\_compliment\_01";"\_compliment\_plural\_01";"\_spawn";"\_spawn\_plural";
                                "\_sound\_01";"\_sound\_02";"\_sound\_03";"\_sound\_04";"\_sound\_05";"\_organ";"\_mouth"
* Add localisation for strategic_resources
  * resource
  * resource + "\_desc"

### 0.2.8

* Temporarily remove research leader checks

### 0.2.7

* Add localisation for armies and army_attachments
  * army name
  * army name plus "_plural"
  * army name plus "_desc"
  * attachtment is the same three but starting "army_"
* Add localisation for aura in component_templates
* Add localisation for diplo phrases
* Check technology "research_leader"'s "has\_trait" matches the technology category
* Add localisation for ship_sizes
* Add localisation for pop\_faction\_types
* Add localisation for technology gateway
* Add localisation for species_rights
  * right name
  * right name plus "_tooltip"
  * right name plus "\_tooltip\_delayed"
* Add localisation for map setup_secnarios
* Add localisation for megastructurew
  * megastructure name
  * megastrcture name plus "_DESC"
  * megastructure name plus "\_MEGASTRUCTURE\_DETAILS"
  * megastructure name plsu "\_CONSTRUCTION\_INFO\_DELAYED"

### 0.2.6

* Add more validation for technologies
  * All "research_leader" must have an area, which should match the technology

### 0.2.5

* Add localisation checks for buildings
  * building name
  * build name plus "_desc"
  * all "fail_text" under buildings
* Add localisation checks for component_templates
  * key
* Add localisation checks for traditions
  * use tradition_categories to determine traditions
  * tradition name for all
  * tradition_desc for start + traditions
  * tradition_delayed for traditions
  * tradition_effect for start and finish

### 0.2.4

* Add localisation checks for technology
  * technology name
  * technology name plus "_desc"
  * all "title" and "desc" keys under "prereqfor_desc"
* Add localisation checks for component_sets
  * component\_set's "key", but only is "required\_component\_set" is false
