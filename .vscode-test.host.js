// `npm run test:host`: the full suite including the host-dependent tests
// (hover, completion, engineParity, activation, graph panel). These launch the
// language server, so they are slower and need a display; CI runs them here on
// the default engine. To pin an engine, use the .vscode-test.fsharp.js /
// .vscode-test.rust.js configs instead.

/** @type {import('@vscode/test-cli').IBaseTestConfiguration} */
module.exports = {
  vscode: 'stable',
  extensionDevelopmentPath: 'release',
  files: './release/bin/client/test/suite/**/*.test.js',
  workspaceFolder: './release/bin/client/test/sample',
  launchArgs: [
    './client/test/sample/events/irm.txt'
  ]
}
