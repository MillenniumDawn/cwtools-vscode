import * as assert from "assert";
import * as fs from "node:fs/promises";
import * as path from "node:path";
import * as vscode from "vscode";
import { assertGateRefused, showNotStartedReport } from "../support/utils";

suite("Activation gate descriptor refusals", function () {
	this.timeout(60 * 1000);

	// First, so the command is what activates the extension, as it is from the palette.
	test("the setup report says why CWTools did not start", async function () {
		const text = await showNotStartedReport();

		assert.ok(
			text.includes("- Reason: no workspace folder has a descriptor.mod file at its root"),
			text,
		);
		for (const folder of vscode.workspace.workspaceFolders ?? []) {
			assert.ok(text.includes(`\`${folder.uri.fsPath}\``), text);
		}
		await assertGateRefused();
	});

	test("starts no server without a descriptor.mod file in any root", async function () {
		const folders = vscode.workspace.workspaceFolders ?? [];
		assert.deepStrictEqual(
			folders.map((folder) => folder.name),
			["no-descriptor", "directory-descriptor"],
			"expected the gate-descriptor fixture",
		);
		const [bare, directory] = folders.map((folder) =>
			path.join(folder.uri.fsPath, "descriptor.mod"),
		);
		await assert.rejects(fs.stat(bare), { code: "ENOENT" });
		assert.ok((await fs.stat(directory)).isDirectory());

		await assertGateRefused();
	});
});
