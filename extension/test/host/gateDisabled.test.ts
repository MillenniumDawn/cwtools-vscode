import * as assert from "assert";
import * as fs from "node:fs/promises";
import * as path from "node:path";
import * as vscode from "vscode";
import { assertGateRefused, showNotStartedReport } from "../support/utils";

suite("Activation gate cwtools.enable refusal", function () {
	this.timeout(60 * 1000);

	test("starts no server when cwtools.enable is false in a descriptor workspace", async function () {
		const root = vscode.workspace.workspaceFolders?.[0];
		assert.ok(root, "expected the gate-disabled fixture");
		assert.ok(
			(await fs.stat(path.join(root.uri.fsPath, "descriptor.mod"))).isFile(),
			"the fixture must hold a real descriptor so only the setting can refuse",
		);
		assert.strictEqual(
			vscode.workspace.getConfiguration("cwtools").get("enable"),
			false,
			"the workspace setting should reach the host",
		);

		await assertGateRefused();
	});

	test("the setup report says why CWTools did not start", async function () {
		const text = await showNotStartedReport();

		assert.ok(text.includes("- Reason: the cwtools.enable setting is false"), text);
		await assertGateRefused();
	});
});
