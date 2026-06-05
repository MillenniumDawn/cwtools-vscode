// `npm run test:coverage`: runs the server-free unit suites (same as `npm test`)
// with V8 coverage. Scope is the extension-host code (client/extension,
// client/common); the webview is a separate rollup bundle and can't be
// instrumented here. Reporters are passed on the CLI in the npm script, not via
// coverage.reporter (the runner ignores that). Widen `files` to the host suites
// once those run with rules in CI.

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
