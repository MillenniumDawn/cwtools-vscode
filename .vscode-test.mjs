// Single source for the host-based extension tests. Pick a config with
// `vscode-test --label <name>` (see the npm `test:*` scripts):
//
//   unit     fast suites that need the VS Code API but not the language server
//   host     full suite incl. host-dependent tests (server, slower, CI)
//   fsharp   full suite pinned to the F# engine
//   rust     full suite pinned to the Rust engine (the default)
//
// Coverage applies globally when `--coverage` is passed (test:coverage runs the
// `unit` label). The host-free F#-vs-Rust parity harness is separate
// (`npm run test:parity`).

import path from 'node:path';
import fs from 'node:fs';
import { fileURLToPath } from 'node:url';
import { defineConfig } from '@vscode/test-cli';

const here = path.dirname(fileURLToPath(import.meta.url));

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

// Seed an engine-pinned profile so `cwtools.engine` is set before the extension
// activates, without writing into the developer's real user settings or the
// shared default profile (which would leak the pin into `test:host`).
function engineProfile(engine) {
	const profile = path.resolve(here, `.vscode-test/${engine}-profile`);
	fs.mkdirSync(path.join(profile, 'User'), { recursive: true });
	fs.writeFileSync(
		path.join(profile, 'User', 'settings.json'),
		JSON.stringify({ 'cwtools.engine': engine }, null, 2),
	);
	return profile;
}

export default defineConfig({
	tests: [
		{ ...base, label: 'unit', files: unitFiles, launchArgs: [sampleFile] },
		{ ...base, label: 'host', files: allFiles, launchArgs: [sampleFile] },
		{
			...base,
			label: 'fsharp',
			files: allFiles,
			launchArgs: ['--user-data-dir=' + engineProfile('fsharp'), sampleFile],
		},
		{
			...base,
			label: 'rust',
			files: allFiles,
			launchArgs: ['--user-data-dir=' + engineProfile('rust'), sampleFile],
		},
	],
	coverage: {
		include: ['**/client/extension/**', '**/client/common/**'],
		exclude: ['**/client/test/**', '**/client/webview/**'],
	},
});
