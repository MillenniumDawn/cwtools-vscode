# Standalone HOI4 LSP and read-only audit

This recipe uses the `cwtools-server` stdio executable directly. The LSP is not
an HTTP service: the client launches the process and exchanges framed JSON-RPC
messages on stdin/stdout. Keep stdout reserved for LSP frames; capture stderr
for startup/configuration errors.

## Build and initialize

Build from the repository root (or use the platform binary bundled with an
extension release):

```sh
cargo build --release --manifest-path engine/Cargo.toml -p cwtools_lsp --bin cwtools-server
```

Use an explicit workspace root and a HOI4 rules checkout. Rules are not bundled;
clone [cwtools-hoi4-config](https://github.com/cwtools/cwtools-hoi4-config) and
use its `Config` directory. A minimal JSON-RPC initialize request is:

```json
{
  "jsonrpc": "2.0", "id": 1, "method": "initialize",
  "params": {
    "processId": 1234,
    "rootUri": "file:///absolute/path/to/mod",
    "capabilities": {},
    "initializationOptions": {
      "language": "hoi4",
      "rulesCache": "/absolute/path/to/cwtools-hoi4-config/Config",
      "localisationLanguages": ["English"]
    }
  }
}
```

Send each JSON message as `Content-Length: <UTF-8 byte count>\r\n\r\n` then
its JSON body. Wait for the response with `id: 1`, send `initialized`, then open
a document with `textDocument/didOpen` (URI, version and complete text). Wait for
`textDocument/publishDiagnostics` with that URI; a response to `initialize`
alone does not mean validation has finished. On edits, send `didChange` with a
strictly increasing document version and wait for a subsequent diagnostics
notification whose `version` matches. An empty diagnostics array is evidence of
a completed clean validation only for that exact URI/version. Preserve all
severities, including `Information` and `Hint`.

For graceful shutdown send the `shutdown` request (without a `params` member),
wait for its response, then send `exit` and wait for process termination. After
editing rule files in place, run the `reloadrulesconfig` command
(`workspace/executeCommand`) or restart the server; restarting is the simplest
reproducible setup. `workspace/didChangeConfiguration` reloads rules only when
`rulesCache` names a different directory.

## Reproduce the standalone contract

The Rust integration tests start the actual server binary and the CLI against
the same checked-in rules and two-file workspace fixtures. Both compare the
complete normalized diagnostic record lists with
[`expected.json`](../../engine/tests/standalone_fixture/expected.json):
relative slash-normalized file, 1-based line, code, severity, and message. The
LSP test separately checks the complete diagnostic ranges, version-1 publishes
for both files, the version-2 publish after correction, completion, and graceful
shutdown. Its non-default control has a separate CW263 so its inclusion in the
same workspace scope is visible while CW282 is absent.

```sh
cargo test --manifest-path engine/Cargo.toml -p cwtools_lsp --test standalone -- --nocapture
cargo test --manifest-path engine/Cargo.toml -p cwtools_cli --test standalone -- --nocapture
```

The fixture is intentionally small and self-contained. The setup test uses a
missing directory beneath a temporary directory, initializes and sends
`initialized`, then explicitly runs `reloadrulesconfig` to observe the warning
and successful request response. The malformed `.cwt` test waits for its
`CW601` diagnostic after `initialized`. For a real mod, point `rulesCache` to
the pinned/corresponding rules checkout and use `language: hoi4`. Do not treat
an empty game-file diagnostic list as success when setup errors are present.

## Compare with the CLI

Build the batch binary, then materialize the exact two checked-in workspace
files used by the integration tests. Run these commands from the repository root:

```sh
cargo build --release --manifest-path engine/Cargo.toml -p cwtools_cli --bin cwtools
mkdir -p /tmp/cwtools-standalone/mod/common/foo
cp engine/tests/standalone_fixture/invalid.txt /tmp/cwtools-standalone/mod/common/foo/example.txt
cp engine/tests/standalone_fixture/control.txt /tmp/cwtools-standalone/mod/common/foo/control.txt
engine/target/release/cwtools validate --game hoi4 \
  --directory /tmp/cwtools-standalone/mod \
  --rules engine/tests/standalone_fixture/Config \
  --loc-language english --no-vanilla-cache --fail-on none \
  --report-type json
```

This validates both files in the same workspace scope, against the same explicit
rules, HOI4 selection, English localization, and disabled vanilla cache as the
smoke tests. Compare the resulting records to
`engine/tests/standalone_fixture/expected.json`; the automated CLI and LSP tests
perform that complete normalized-list comparison. On Windows use
`engine/target/release/cwtools.exe`. The CLI JSON includes file, line, severity,
code, message and hash, but not an LSP-style range. LSP ranges are zero-based
UTF-16; the normalized expected records use slash-normalized relative paths and
1-based lines. For other scopes, the CLI's `--file` filters reported
validations but still indexes the full directory for cross-file references; the
LSP keeps incremental workspace state and may publish separate notifications
per open file. Some checks need a base-game index and therefore cannot be
compared unless both runs use matching base-game data/cache.

## Read-only simplification audit

Use `CW280`/`CW282` Information diagnostics as *review candidates*, not fixes to
apply blindly. Keep Information visible in the client. For each candidate:

1. Record its file, URI, range, code, message and current document version.
2. Inspect the full enclosing block and the applicable rule's documented
default; inspect callers/references and the surrounding game scope.
3. Confirm that removing only the redundant explicit default preserves the
   enclosing scope, default behavior, evaluation order and gameplay effect.
4. Record a behavior-preserving simplification separately from a likely bug.
   Do not invoke fix-all/workspace fixes or write edits during the audit.

The engine's existing machine fix is not authorization to apply a workspace
cleanup. Audit behavior and candidate edits remain a human read-only workflow.
Do not infer a new cleanup rule from one example; collect concrete examples and
propose any new check as a separate follow-up.

## Host coverage is not server coverage

A host or diagnostic aggregator must report **unchecked**, not clean, when it
has no CWTools server mapping for a game file, excludes the file, hits its own
size cap, times out, fails to launch the server, cannot load rules, or never
receives completion evidence for the current version. These are distinct host
coverage/infrastructure failures and must not be represented as an empty
CWTools diagnostic set.

Record the host's file-to-language mapping, include/exclude patterns, maximum
file size and timeout alongside its server launch/configuration result. For a
large Millennium Dawn file rejected by a host limit, check the host's own
mapping/exclusion/size-limit logs first; then run the standalone LSP against a
small extracted relevant fixture or open the original file directly with a
local client that can handle it. Compare the file length/encoding and server
stderr, wait for diagnostics for its URI/version, and note timing. Do not
conclude that the engine rejected a file unless the server actually received
and reported an error for it.
