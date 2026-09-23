import * as assert from "assert";
import * as path from "node:path";
import * as vscode from "vscode";
import { activate, waitForServerReady } from "../support/utils";

suite("Multi-root descriptor selection", function () {
	this.timeout(90 * 1000);

	test("gates and scans the descriptor root, not an unrelated first root", async function () {
		const folders = vscode.workspace.workspaceFolders ?? [];
		assert.strictEqual(folders.length, 2, "expected the multi-root fixture");
		assert.strictEqual(folders[0].name, "unrelated-hoi4");
		assert.strictEqual(folders[1].name, "stellaris-mod");

		const api = await activate();
		if (!api) {
			throw new Error("activation API should be exposed");
		}
		assert.ok(
			api.serverOutputChannel(),
			"descriptor root should enable CWTools",
		);
		assert.ok(
			await waitForServerReady(api),
			`server never finished its initial scan, last status: ${api.serverStatusText()}`,
		);

		const unrelated = vscode.Uri.file(
			path.join(folders[0].uri.fsPath, "events", "unrelated.txt"),
		);
		const selected = vscode.Uri.file(
			path.join(folders[1].uri.fsPath, "events", "selected.txt"),
		);
		assert.deepStrictEqual(
			vscode.languages.getDiagnostics(unrelated),
			[],
			"the unrelated first root must not be scanned",
		);
		assert.ok(
			vscode.languages.getDiagnostics(selected).length > 0,
			"the descriptor-bearing root should be scanned",
		);
	});
});
