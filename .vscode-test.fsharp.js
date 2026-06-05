// Run the extension tests against the F# engine.
// Pre-seeds the test runner's user-data-dir so cwtools.engine=fsharp
// is set before the extension activates.
//
// This avoids touching the developer's real user settings.

const path = require('path');
const fs = require('fs');

const userDataDirs = [
	path.resolve(__dirname, '.vscode-test/user-data/User'),
	path.resolve(__dirname, '.vscode-test/fsharp-profile/User'),
];
for (const userDir of userDataDirs) {
	fs.mkdirSync(userDir, { recursive: true });
	fs.writeFileSync(
		path.join(userDir, 'settings.json'),
		JSON.stringify({ 'cwtools.engine': 'fsharp' }, null, 2)
	);
}

/** @type {import('@vscode/test-cli').IBaseTestConfiguration} */
module.exports = {
	vscode: 'stable',
	extensionDevelopmentPath: 'release',
	files: './release/bin/client/test/suite/**/*.test.js',
	workspaceFolder: './release/bin/client/test/sample',
	launchArgs: [
		'--user-data-dir=' + path.resolve(__dirname, '.vscode-test/fsharp-profile'),
		'./client/test/sample/events/irm.txt'
	]
};

