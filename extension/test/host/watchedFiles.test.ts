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
		const observedEvents = new Set<string>();
		const hostWatchers = folders.map((folder, index) => {
			const suffix = index === 2 ? "cwt" : "txt";
			const watcher = vscode.workspace.createFileSystemWatcher(
				new vscode.RelativePattern(folder, `**/*.${suffix}`),
			);
			watcher.onDidCreate((uri) => { observedEvents.add(`create:${uri.fsPath}`); });
			watcher.onDidChange((uri) => { observedEvents.add(`change:${uri.fsPath}`); });
			watcher.onDidDelete((uri) => { observedEvents.add(`delete:${uri.fsPath}`); });
			return watcher;
		});
		try {
			for (const [index, folder] of folders.entries()) {
				const suffix = index === 2 ? "cwt" : "txt";
				const file = path.join(folder.uri.fsPath, index === 2 ? "probe.cwt" : "events/probe.txt");
				const excluded = path.join(folder.uri.fsPath, ".claude", `excluded.${suffix}`);
				const uri = vscode.Uri.file(file);
				const excludedUri = vscode.Uri.file(excluded);
				try {
					await fs.mkdir(path.dirname(excluded), { recursive: true });
					await fs.writeFile(excluded, 'name = "unterminated');
					assert.ok(await waitUntil(() => observedEvents.has(`create:${excluded}`), 15_000), `VS Code dropped excluded create event: ${excluded}`);
					await wait(1_500);
					assert.deepStrictEqual(vscode.languages.getDiagnostics(excludedUri), [], "invalid excluded create must not produce diagnostics");
					await fs.writeFile(excluded, 'name = "another unterminated string');
					assert.ok(await waitUntil(() => observedEvents.has(`change:${excluded}`), 15_000), `VS Code dropped excluded change event: ${excluded}`);
					await wait(1_500);
					assert.deepStrictEqual(vscode.languages.getDiagnostics(excludedUri), [], "invalid excluded change must not produce diagnostics");
					await fs.unlink(excluded);
					assert.ok(await waitUntil(() => observedEvents.has(`delete:${excluded}`), 15_000), `VS Code dropped excluded delete event: ${excluded}`);
					await fs.writeFile(file, 'name = "unterminated');
					assert.ok(await waitUntil(() => observedEvents.has(`create:${file}`), 15_000), `VS Code dropped create event: ${file}`);
					assert.ok(await waitUntil(() => vscode.languages.getDiagnostics(uri).length > 0, 15_000), `server diagnostics missed create: ${file}`);
					await fs.writeFile(file, 'name = "valid"\n');
					assert.ok(await waitUntil(() => observedEvents.has(`change:${file}`), 15_000), `VS Code dropped change event: ${file}`);
					assert.ok(await waitUntil(() => vscode.languages.getDiagnostics(uri).length === 0, 15_000), `server diagnostics missed change: ${file}`);
					await fs.writeFile(file, 'name = "another unterminated string');
					assert.ok(await waitUntil(() => vscode.languages.getDiagnostics(uri).length > 0, 15_000));
					await fs.unlink(file);
					assert.ok(await waitUntil(() => observedEvents.has(`delete:${file}`), 15_000), `VS Code dropped delete event: ${file}`);
					assert.ok(await waitUntil(() => vscode.languages.getDiagnostics(uri).length === 0, 15_000), `server diagnostics missed delete: ${file}`);
					await wait(1_000);
					assert.deepStrictEqual(vscode.languages.getDiagnostics(excludedUri), [], "observable excluded descendants must remain ignored");
				} finally {
					await fs.rm(file, { force: true });
					await fs.rm(path.dirname(excluded), { recursive: true, force: true });
				}
			}
		} finally {
			for (const watcher of hostWatchers) watcher.dispose();
		}
	});
});
