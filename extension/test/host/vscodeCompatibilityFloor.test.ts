import * as assert from "assert";
import * as vscode from "vscode";
import {
	activate,
	EXTENSION_ID,
	waitForServerReady,
} from "../support/utils";

suite("VS Code compatibility floor", function () {
	test("activates and completes language-server initialization on 1.91.0", async function () {
		this.timeout(90_000);
		assert.strictEqual(vscode.version, "1.91.0");

		const extension = vscode.extensions.getExtension(EXTENSION_ID);
		assert.ok(extension, "CWTools extension should be installed in the host");
		const manifest = extension.packageJSON as unknown as {
			engines: { vscode: string };
		};
		assert.strictEqual(manifest.engines.vscode, "^1.91.0");

		const api = await activate();
		assert.ok(api, "extension activation should expose its API");
		assert.ok(extension.isActive, "extension should be active");
		assert.ok(
			await waitForServerReady(api),
			`language server did not initialize, last status: ${api.serverStatusText()}`,
		);
		assert.ok(
			api.serverCommands().length > 0,
			"initialized server should publish command capabilities",
		);
	});
});
