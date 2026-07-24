// Single source for the host-based extension tests. Pick a config with
// `vscode-test --label <name>` (see the npm `test:*` scripts):
//
//   unit     fast suites that need the VS Code API but not the language server
//   smoke    unit plus the activation suite, which starts the real server
//   host     full suite incl. the language-feature tests (slower, see below)
//
// The hover and completion suites assert on rule-driven data. The sample
// workspace has no game name in its path and no game-specific content dir, so
// it detects as the generic `paradox` language, which has no rules repo to
// clone; those suites then see only word-completion fallback and empty hovers
// and fail. CI gates on `smoke` (which still boots the server binary and checks
// activation) and `host` stays the local full run until the fixture is made
// identifiable as a game.
//
// Coverage applies globally when `--coverage` is passed (test:coverage runs the
// `unit` label).

import { defineConfig } from '@vscode/test-cli';

const sampleWorkspace = './release/bin/client/test/sample';
const sampleFile = './client/test/sample/events/irm.txt';
const unitFiles = [
	'./release/bin/client/test/suite/graphTypes.test.js',
	'./release/bin/client/test/suite/fileExplorer.test.js',
];
const smokeFiles = [...unitFiles, './release/bin/client/test/suite/extension.test.js'];
const allFiles = './release/bin/client/test/suite/**/*.test.js';

const base = {
	vscode: 'stable',
	extensionDevelopmentPath: 'release',
	workspaceFolder: sampleWorkspace,
};

export default defineConfig({
	tests: [
		{ ...base, label: 'unit', files: unitFiles, launchArgs: [sampleFile] },
		{ ...base, label: 'smoke', files: smokeFiles, launchArgs: [sampleFile] },
		{ ...base, label: 'host', files: allFiles, launchArgs: [sampleFile] },
	],
	coverage: {
		reporter: ['text-summary', 'html', 'lcov', 'json-summary'],
		// Intent: only the hand-written client source. vscode-test instruments
		// every loaded file though, and doesn't reliably honor these globs: it
		// leaks node_modules into the raw report (semver, vscode-jsonrpc, ...)
		// and still reports engine.ts / executable.ts even when excluded. Those
		// two are owned by the node unit tests (vitest, see vitest.config.ts);
		// build/coverage-summary.ts is what actually drops node_modules and the
		// vitest-owned files from the rendered report and recomputes the totals.
		// The excludes below are kept as declared intent.
		include: ['**/client/extension/**', '**/client/common/**'],
		exclude: [
			'**/client/extension/engine.ts',
			'**/client/extension/executable.ts',
			'**/client/extension/games.ts',
			'**/client/test/**',
			'**/client/webview/**',
			'**/node_modules/**',
		],
	},
});
