import * as assert from "node:assert/strict";
import * as fs from "node:fs/promises";
import * as path from "node:path";
import * as vscode from "vscode";
import { activate, EXTENSION_ID } from "../support/utils";

suite("Activation gates", function () {
	this.timeout(30_000);

	test("does not initialize the server when the workspace is not an enabled mod", async function () {
		const gate = process.env.CWTOOLS_TEST_ACTIVATION_GATE;
		assert.ok(
			gate === "missing" || gate === "directory" || gate === "disabled",
			`expected an isolated activation-gate host, got: ${gate}`,
		);
		const workspaceRoot = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
		assert.ok(workspaceRoot, "expected a single fixture workspace");
		const descriptor = path.join(workspaceRoot, "descriptor.mod");
		if (gate === "missing") {
			await assert.rejects(fs.stat(descriptor), { code: "ENOENT" });
		} else if (gate === "directory") {
			assert.ok((await fs.stat(descriptor)).isDirectory());
		} else {
			assert.ok((await fs.stat(descriptor)).isFile());
			assert.strictEqual(
				vscode.workspace.getConfiguration("cwtools").get("enable"),
				false,
			);
		}

		const extension = vscode.extensions.getExtension(EXTENSION_ID);
		assert.ok(extension, "CWTools extension should be installed in the host");

		const api = await activate();
		assert.ok(api, "activation should expose its documented host API");
		assert.deepStrictEqual(
			api.serverCommands(),
			[],
			"the language client should not receive server commands when activation is gated",
		);
		assert.strictEqual(
			api.serverProcessId(),
			undefined,
			"the activation gate should not spawn the server process",
		);
		assert.strictEqual(
			api.rulesCacheRoot(),
			undefined,
			"the activation gate should return before creating its rules cache root",
		);
		assert.strictEqual(
			api.serverOutputChannel(),
			undefined,
			"the activation gate should not create the CWTools output channel",
		);
		assert.strictEqual(
			api.serverStatusText(),
			undefined,
			"the activation gate should not create the server status item",
		);
		assert.strictEqual(extension.isActive, true);

		// Manifest-contributed commands remain listed even while disabled. The
		// runtime context used by the contributed views is internal to VS Code,
		// so process/client state above is the observable activation contract.
		assert.ok((vscode.workspace.workspaceFolders?.length ?? 0) > 0);
	});
});
