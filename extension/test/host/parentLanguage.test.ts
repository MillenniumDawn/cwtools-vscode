import * as assert from "assert";
import * as path from "node:path";
import * as vscode from "vscode";
import { activate, waitForServerReady, waitUntil } from "../support/utils";

suite("resolved parent language roots", function () {
	this.timeout(90_000);

	test("promotes scripts from a launcher-discovered dependency with parentMods empty", async function () {
		assert.deepStrictEqual(
			vscode.workspace.getConfiguration("cwtools").get<string[]>("parentMods"),
			[],
			"fixture must exercise descriptor dependency discovery",
		);
		const api = await activate();
		assert.ok(api, "activation API should be exposed");
		const parentFile = vscode.Uri.file(
			path.resolve(
				"extension/test/workspaces/parent-language/home/.local/share/Paradox Interactive/Hearts of Iron IV/mod/parent-content/common/fixture.txt",
			),
		);
		const document = await vscode.workspace.openTextDocument(parentFile);
		await vscode.languages.setTextDocumentLanguage(document, "plaintext");
		assert.ok(
			await waitForServerReady(api),
			`server did not finish its scan: ${api.serverStatusText()}`,
		);
		await vscode.commands.executeCommand("cwtools.restartServer");
		assert.ok(
			await waitForServerReady(api),
			`restarted server did not finish its scan: ${api.serverStatusText()}`,
		);
		assert.ok(
			await waitUntil(() => document.languageId === "paradox"),
			"server-resolved dependency root should promote the already-open parent file",
		);

		const unrelatedFile = vscode.Uri.file(
			path.resolve("extension/test/workspaces/parent-language/home/external/common/notes.txt"),
		);
		const unrelated = await vscode.workspace.openTextDocument(unrelatedFile);
		assert.strictEqual(unrelated.languageId, "plaintext");
	});
});
