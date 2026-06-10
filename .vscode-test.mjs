// Single source for the host-based extension tests. Pick a config with
// `vscode-test --label <name>` (see the npm `test:*` scripts):
//
//   unit     fast suites that need the VS Code API but not the language server
//   host     full suite incl. host-dependent tests (server, slower, CI)
//
// Coverage applies globally when `--coverage` is passed (test:coverage runs the
// `unit` label).

import { defineConfig } from '@vscode/test-cli';

const sampleWorkspace = './release/bin/client/test/sample';
const sampleFile = './client/test/sample/events/irm.txt';
const unitFiles = [
	'./release/bin/client/test/suite/graphTypes.test.js',
	'./release/bin/client/test/suite/fileExplorer.test.js',
	'./release/bin/client/test/suite/extensionHelpers.test.js',
	'./release/bin/client/test/suite/executable.test.js',
];
const allFiles = './release/bin/client/test/suite/**/*.test.js';

const base = {
	vscode: 'stable',
	extensionDevelopmentPath: 'release',
	workspaceFolder: sampleWorkspace,
};

export default defineConfig({
	tests: [
		{ ...base, label: 'unit', files: unitFiles, launchArgs: [sampleFile] },
		{ ...base, label: 'host', files: allFiles, launchArgs: [sampleFile] },
	],
	coverage: {
		// Intent: only the hand-written client source. vscode-test instruments
		// every loaded file though, and ignores these globs for dependencies, so
		// node_modules still land in the raw report (semver, vscode-jsonrpc, ...).
		// build/coverage-summary.ts drops them and recomputes the totals; codecov
		// excludes them via codecov.yml.
		include: ['**/client/extension/**', '**/client/common/**'],
		exclude: ['**/client/test/**', '**/client/webview/**', '**/node_modules/**'],
	},
});
