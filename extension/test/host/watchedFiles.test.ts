import * as assert from "assert";
import * as fs from "node:fs/promises";
import * as path from "node:path";
import * as vscode from "vscode";
import { activate, waitForServerReady, waitUntil, wait } from "../support/utils";

suite("served-root file watchers", function () {
	this.timeout(90_000);
	test("tracks external create, change and delete beneath excluded ancestors", async () => {
		const api = await activate();
		assert.ok(api);
		assert.ok(await waitForServerReady(api));
		assert.strictEqual(vscode.workspace.getConfiguration("cwtools").get("backgroundReindex.intervalMinutes"), 0);
		const folders = vscode.workspace.workspaceFolders ?? [];
		assert.strictEqual(folders.length, 3);
		for (const [index, folder] of folders.entries()) {
			const suffix = index === 2 ? "cwt" : "txt";
			const file = path.join(folder.uri.fsPath, index === 2 ? "probe.cwt" : "events/probe.txt");
			const excluded = path.join(folder.uri.fsPath, "node_modules", `excluded.${suffix}`);
			const uri = vscode.Uri.file(file);
			try {
				await fs.mkdir(path.dirname(excluded), { recursive: true });
				await fs.writeFile(excluded, 'name = "unterminated');
				await fs.writeFile(file, 'name = "unterminated');
				assert.ok(await waitUntil(() => vscode.languages.getDiagnostics(uri).length > 0, 15_000), `create was dropped: ${file}`);
				await fs.writeFile(file, 'name = "valid"\n');
				assert.ok(await waitUntil(() => vscode.languages.getDiagnostics(uri).length === 0, 15_000), `change was dropped: ${file}`);
				await fs.writeFile(file, 'name = "another unterminated string');
				assert.ok(await waitUntil(() => vscode.languages.getDiagnostics(uri).length > 0, 15_000));
				await fs.unlink(file);
				assert.ok(await waitUntil(() => vscode.languages.getDiagnostics(uri).length === 0, 15_000), `delete was dropped: ${file}`);
				await wait(1_000);
				assert.deepStrictEqual(vscode.languages.getDiagnostics(vscode.Uri.file(excluded)), [], "excluded descendants must remain ignored");
			} finally {
				await fs.rm(file, { force: true });
				await fs.rm(path.dirname(excluded), { recursive: true, force: true });
			}
		}
	});
});
