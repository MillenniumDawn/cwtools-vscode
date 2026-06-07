# Engine parity harness

Drives both cwtools server binaries (the vanilla F# reference and the Rust
port) directly over stdio, with no VS Code host, and asks them identical
questions about the sample mod.

The rule is simple: vanilla cwtools (F#) is the spec. For each capability there
are two tests:

- `[fsharp] ...` proves the reference behavior works. If this fails, the rules
  or setup is broken, not the port.
- `[rust] ...` checks the Rust port reaches the same answer.

The Rust tests are **expected to fail until the port reaches parity**. A red
Rust test is the to-do list. Don't weaken a test to make Rust pass, fix the
Rust binary.

## Running

```bash
npm run test:parity
```

That clones the Stellaris config into `.cwtools-parity/` (once), compiles, and
runs the suite under plain mocha. To point at an existing rules checkout
instead of cloning:

```bash
CWTOOLS_PARITY_RULES=/path/to/cwtools-stellaris-config/config npm run test:parity
```

Without rules the whole suite skips.

## How the two engines are driven

They both speak LSP but load rules differently, so the handshake differs:

- Rust loads rules straight from `initializationOptions.rulesCache`.
- F# loads rules lazily, only after a `workspace/didChangeConfiguration` that
  carries the full `cwtools` settings block. It throws on any missing key, so
  the harness sends every key (see `engineSession.ts`).

The VS Code extension papers over this; here `engineSession.ts` reproduces just
enough of it.

## Adding a probe

Add an entry to `probes` in `parity.test.ts`. Each probe has a `kind` that
determines what to assert:

| Kind | What it checks | Key fields |
|---|---|---|
| `hover` | Hover content contains expected substrings | `file`, `line`, `character`, `expected` |
| `completion` | Completion list includes expected labels | `file`, `line`, `character`, `expected` |
| `definition` | Goto-definition resolves to expected file | `file`, `line`, `character`, `expectedTargetFile` |
| `references` | Find-references returns at least N hits | `file`, `line`, `character`, `expectedMinCount` |
| `diagnostics` | publishDiagnostics surfaces at least N issues | `file`, `expectedMinCount` |
| `formatting` | documentFormatting returns edits | `file`, `expectAny` |

Both engines get a test automatically.

A `references` or `formatting` probe can set `pending` to a short reason; both
its engine tests then register as pending (skipped) instead of running. Use this
only when neither server answers the request headless, so the gap is documented
without a misleading red or a hung request. Today `find-refs` and `formatting`
are pending: F# returns no references over stdio in this harness and neither
server answers `documentFormatting` (Rust replies "Method not found").

## Notes

- This is host-free and fast, separate from the electron-based suites
  (`npm run test:host`, `npm run test:engine:*`), which run the same checks
  through the real extension.
- The F# binary is used as-is. Its config reader crashing on a missing key is
  cruft worth removing, but rebuilding the F# server is currently blocked by a
  compile break in the `cwtools` submodule, so we send a complete config
  instead of patching the server.
