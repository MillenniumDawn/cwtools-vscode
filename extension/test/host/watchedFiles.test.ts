import * as assert from "assert";
import * as fs from "node:fs/promises";
import * as path from "node:path";
import * as vscode from "vscode";
import { activate, waitForServerReady, waitUntil } from "../support/utils";

suite("served-root file watchers", function () {
	this.timeout(90_000);
	test("tracks external create, change and delete in served roots", async () => {
		const api = await activate();
		assert.ok(api);
		assert.ok(await waitForServerReady(api));
		assert.strictEqual(vscode.workspace.getConfiguration("cwtools").get("backgroundReindex.intervalMinutes"), 0);
		const folders = vscode.workspace.workspaceFolders ?? [];
		assert.strictEqual(folders.length, 3);
		const observedEvents = new Set<string>();
		const hostWatchers = folders.map((folder, index) => {
			// Observe the exact probe path; the server's production extension globs
			// are independently covered by the diagnostic assertions below.
			const relativePath = index === 2 ? "probe.cwt" : "events/probe.txt";
			const watcher = vscode.workspace.createFileSystemWatcher(
				new vscode.RelativePattern(folder.uri, relativePath),
			);
			watcher.onDidCreate((uri) => { observedEvents.add(`create:${uri.fsPath}`); });
			watcher.onDidChange((uri) => { observedEvents.add(`change:${uri.fsPath}`); });
			watcher.onDidDelete((uri) => { observedEvents.add(`delete:${uri.fsPath}`); });
			return watcher;
		});
		// Let VS Code finish registering the observers before writing probes.
		await new Promise((resolve) => setTimeout(resolve, 250));
		try {
			for (const [index, folder] of folders.entries()) {
				const expectScriptDiagnostics = index !== 2;
				const file = path.join(folder.uri.fsPath, index === 2 ? "probe.cwt" : "events/probe.txt");
				const uri = vscode.Uri.file(file);
				try {
					await fs.writeFile(file, 'name = "unterminated');
					assert.ok(await waitUntil(() => observedEvents.has(`create:${file}`), 15_000), `VS Code dropped create event: ${file}`);
					if (expectScriptDiagnostics) {
						assert.ok(await waitUntil(() => vscode.languages.getDiagnostics(uri).length > 0, 15_000), `server diagnostics missed create: ${file}`);
					}
					await fs.writeFile(file, 'name = "valid"\n');
					assert.ok(await waitUntil(() => observedEvents.has(`change:${file}`), 15_000), `VS Code dropped change event: ${file}`);
					if (expectScriptDiagnostics) {
						assert.ok(await waitUntil(() => vscode.languages.getDiagnostics(uri).length === 0, 15_000), `server diagnostics missed change: ${file}`);
						await fs.writeFile(file, 'name = "another unterminated string');
						assert.ok(await waitUntil(() => vscode.languages.getDiagnostics(uri).length > 0, 15_000));
					}
					await fs.unlink(file);
					assert.ok(await waitUntil(() => observedEvents.has(`delete:${file}`), 15_000), `VS Code dropped delete event: ${file}`);
					if (expectScriptDiagnostics) {
						assert.ok(await waitUntil(() => vscode.languages.getDiagnostics(uri).length === 0, 15_000), `server diagnostics missed delete: ${file}`);
					}
				} finally {
					await fs.rm(file, { force: true });
				}
			}
		} finally {
			for (const watcher of hostWatchers) watcher.dispose();
		}
	});
});
