// `npm run test:coverage`: runs the server-free unit suites (the same ones as
// the default `npm test`) with V8 coverage turned on. @vscode/test-cli runs
// this through c8 and maps the V8 data back to the TypeScript via the source
// maps tsc emits (tsconfig.json: sourceMap: true).
//
// We instrument the server-free suites rather than the full host suite on
// purpose: they need no language server or cloned rules, so the coverage run is
// reliable locally and in CI (it runs exactly where `npm test` already passes).
// The host-dependent suites (hover, completion, activation) self-skip without
// rules anyway, so they add no coverage today; widen `files` to the host glob
// once those are exercised in CI.
//
// `coverage` lives at the global-options level (alongside `tests`), so the
// config uses the { tests: [...], coverage: {...} } shape. The runner only
// honours the reporter list passed on the CLI (--coverage-reporter), not
// coverage.reporter here, so the reporters live in the npm script.
//
// Scope is the extension-host code (client/extension, client/common). The
// webview (client/webview) runs in the webview process as a rollup bundle, not
// in the extension host, so it can't be instrumented this way.

/** @type {import('@vscode/test-cli').IConfigurationWithGlobalOptions} */
module.exports = {
	tests: [
		{
			vscode: 'stable',
			extensionDevelopmentPath: 'release',
			files: [
				'./release/bin/client/test/suite/graphTypes.test.js',
				'./release/bin/client/test/suite/fileExplorer.test.js',
				'./release/bin/client/test/suite/extensionHelpers.test.js'
			],
			workspaceFolder: './release/bin/client/test/sample',
			launchArgs: [
				'./client/test/sample/events/irm.txt'
			]
		}
	],
	coverage: {
		include: ['**/client/extension/**', '**/client/common/**'],
		exclude: ['**/client/test/**', '**/client/webview/**']
	}
};
