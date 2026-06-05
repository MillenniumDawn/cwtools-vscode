// Default `npm test`: the fast unit suites only. These exercise pure
// extension logic (tree building, engine selection, graph types) and need the
// VS Code API but not the language server, so they run quickly and reliably.
//
// The host-dependent suites (hover, completion, engineParity, and the
// activation/graph-panel parts of extension.test) spin up the full server and
// are CI-only — run them with `npm run test:host`, or the engine-pinned
// `npm run test:engine:rust` / `test:engine:fsharp`. The host-free engine
// parity harness lives in `npm run test:parity`.

/** @type {import('@vscode/test-cli').IBaseTestConfiguration} */
module.exports = {
  vscode: 'stable',
  extensionDevelopmentPath: "release",
  files: [
    './release/bin/client/test/suite/graphTypes.test.js',
    './release/bin/client/test/suite/fileExplorer.test.js',
    './release/bin/client/test/suite/extensionHelpers.test.js',
  ],
  workspaceFolder: "./release/bin/client/test/sample",
  launchArgs: [
    // Bring the file under test into the workspace
    './client/test/sample/events/irm.txt'
  ]
}
