// Run the extension tests against the Rust engine (the default).
// Pre-seeds the test runner's user-data-dir so cwtools.engine=rust
// is set before the extension activates.

const path = require('path');
const fs = require('fs');

// Seed only this engine's own profile (launchArgs pins it below). Writing into
// the shared default profile would leak the engine pin into `npm run test:host`.
const userDir = path.resolve(__dirname, '.vscode-test/rust-profile/User');
fs.mkdirSync(userDir, { recursive: true });
fs.writeFileSync(
	path.join(userDir, 'settings.json'),
	JSON.stringify({ 'cwtools.engine': 'rust' }, null, 2)
);

/** @type {import('@vscode/test-cli').IBaseTestConfiguration} */
module.exports = {
	vscode: 'stable',
	extensionDevelopmentPath: 'release',
	files: './release/bin/client/test/suite/**/*.test.js',
	workspaceFolder: './release/bin/client/test/sample',
	launchArgs: [
		'--user-data-dir=' + path.resolve(__dirname, '.vscode-test/rust-profile'),
		'./client/test/sample/events/irm.txt'
	]
};

