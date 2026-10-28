import * as assert from "assert";
import * as fs from "node:fs/promises";
import * as path from "node:path";
import * as vscode from "vscode";
import { activate, waitForServerReady, waitUntil } from "../support/utils";

// Matches no script, localisation or rules glob, so the server never validates
// it; the broad resource watcher only reports it to the file index.
const READY_PROBE = "watch-ready.probe";

suite("served-root file watchers", function () {
	this.timeout(90_000);
	test("forwards one server event for a script save", async () => {
		const api = await activate();
		assert.ok(api);
		assert.ok(await waitForServerReady(api));
		const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
		assert.ok(root);
		const file = path.join(root, "events", "open.txt");
		const original = await fs.readFile(file, "utf8");
		const readinessObserver = api.observeServerWatchedFileEvents(vscode.Uri.file(file).toString());
		let saveObserver: ReturnType<typeof api.observeServerWatchedFileEvents> | undefined;
		try {
			let lastProbe = 0;
			assert.ok(
				await waitUntil(async () => {
				if (readinessObserver.count() > 0) return true;
					if (Date.now() - lastProbe >= 500) {
						lastProbe = Date.now();
						await fs.writeFile(file, `${original}\n# watcher warmup ${lastProbe}\n`);
					}
					return false;
				}, 20_000),
				"the server's dynamic watcher never became active",
			);
			readinessObserver.dispose();
			// Let the watched event finish before measuring one isolated save.
			await new Promise((resolve) => setTimeout(resolve, 500));
			const serverObserver = api.observeServerWatchedFileEvents(vscode.Uri.file(file).toString());
			saveObserver = serverObserver;
			await fs.writeFile(file, `${original}\n# watched-file regression save\n`);
			assert.ok(
				await waitUntil(() => serverObserver.count() > 0),
				"the server did not receive the script save",
			);
			// Allow the duplicate host watcher callback to arrive before comparing.
			await new Promise((resolve) => setTimeout(resolve, 500));
			assert.strictEqual(serverObserver.count(), 1);
		} finally {
			readinessObserver.dispose();
			saveObserver?.dispose();
			await fs.writeFile(file, original);
		}
	});

	test("indexes resources after a populated folder is moved into a served root", async () => {
		const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
		assert.ok(root);
		const openFile = path.join(root, "events", "open.txt");
		const openUri = vscode.Uri.file(openFile);
		const original = await fs.readFile(openFile, "utf8");
		const settings = vscode.workspace.getConfiguration("cwtools");
		const previousVanilla = settings.get<string>("cache.stellaris");
		const vanillaRoot = path.resolve(root, "..", "..", "vanilla");
		const source = path.resolve(root, "..", "..", "..", ".folder-move-source");
		const destination = path.join(root, "events", "assets");
		const texture = path.join(destination, "moved.dds");
		const script = path.join(destination, "moved.txt");
		const directoryUri = vscode.Uri.file(destination).toString();
		const textureUri = vscode.Uri.file(texture).toString();
		const scriptUri = vscode.Uri.file(script).toString();
		const hostEvents: string[] = [];
		const watcher = vscode.workspace.createFileSystemWatcher(
			new vscode.RelativePattern(vscode.Uri.file(root), "**"),
		);
		watcher.onDidCreate((uri) => { hostEvents.push(`create:${uri.toString()}`); });
		watcher.onDidChange((uri) => { hostEvents.push(`change:${uri.toString()}`); });
		watcher.onDidDelete((uri) => { hostEvents.push(`delete:${uri.toString()}`); });
		let serverObserver: { count(): number; dispose(): void } | undefined;
		let apiForCleanup: Awaited<ReturnType<typeof activate>> = undefined;
		try {
			const api = await activate();
			assert.ok(api);
			apiForCleanup = api;
			assert.ok(await waitForServerReady(api));
			assert.strictEqual(settings.get<number>("backgroundReindex.intervalMinutes"), 0);
			let lastProbe = 0;
			const readyUri = vscode.Uri.file(path.join(root, READY_PROBE)).toString();
			assert.ok(
				await waitUntil(async () => {
					if (hostEvents.some((event) => event.endsWith(`:${readyUri}`))) return true;
					if (Date.now() - lastProbe >= 500) {
						lastProbe = Date.now();
						await fs.writeFile(path.join(root, READY_PROBE), String(lastProbe));
					}
					return false;
				}, 20_000),
				"VS Code watcher did not become ready",
			);
			await fs.rm(path.join(root, READY_PROBE), { force: true });

			const reference = [
				'spriteType = { texturefile = "events/assets/moved.tga" }',
				'spriteType = { texturefile = "gfx/interface/watcher-base.tga" }',
			].join("\n") + "\n";
			await settings.update("cache.stellaris", vanillaRoot, vscode.ConfigurationTarget.Global);
			assert.strictEqual(vscode.workspace.getConfiguration("cwtools").get<string>("cache.stellaris"), vanillaRoot);
			await fs.writeFile(openFile, reference);
			const previousPid = api.serverProcessId();
			await vscode.commands.executeCommand("cwtools.restartServer");
			assert.ok(
				await waitUntil(() => api.serverProcessId() !== previousPid, 10_000),
				"server did not restart with synthetic vanilla configuration",
			);
			assert.ok(await waitForServerReady(api), "restarted server did not finish its initial scan");
			const diagnostics = () => vscode.languages.getDiagnostics(openUri);
			const missingFiles = () => diagnostics().filter((diagnostic) =>
				(typeof diagnostic.code === "object" ? diagnostic.code.value : diagnostic.code) === "CW113",
			);
			const diagnosticSummary = () => diagnostics().map((diagnostic) => ({
				code: diagnostic.code,
				message: diagnostic.message,
				source: diagnostic.source,
			}));
			assert.ok(
				await waitUntil(() => missingFiles().length > 0, 15_000),
				`restarted server emitted no CW113; configuration and diagnostics: ${JSON.stringify({ workspace: root, vanillaRoot, diagnostics: diagnosticSummary() })}`,
			);
			const initialCw113 = missingFiles();
			const initialEvidence = {
				workspace: root,
				gameHint: path.basename(root),
				vanillaRoot,
				vanillaFixture: await fs.stat(path.join(vanillaRoot, "gfx", "interface", "watcher-base.dds")).then(() => true, () => false),
				diagnostics: diagnosticSummary(),
			};
			console.log(`initial folder move diagnostics ${JSON.stringify(initialEvidence)}`);
			assert.strictEqual(initialCw113.length, 1, `only the missing texture should have CW113; diagnostics: ${JSON.stringify(diagnosticSummary())}`);
			const readinessObserver = api.observeServerWatchedFileEvents(openUri.toString());
			try {
				await fs.writeFile(openFile, `${reference}# watcher ready after server restart\n`);
				assert.ok(await waitUntil(() => readinessObserver.count() > 0), "restarted server watcher did not become ready");
			} finally {
				readinessObserver.dispose();
				await fs.writeFile(openFile, reference);
			}
			serverObserver = api.observeServerWatchedFileEvents(directoryUri);

			await fs.mkdir(source, { recursive: true });
			await fs.writeFile(path.join(source, "moved.dds"), "synthetic texture\n");
			await fs.writeFile(path.join(source, "moved.txt"), reference);
			await fs.rename(source, destination);
			const cw113Cleared = await waitUntil(
				() => missingFiles().length === 0,
				15_000,
			);
			await new Promise((resolve) => setTimeout(resolve, 500));
			const observed = (uri: string): string[] => hostEvents.filter((event) => event.endsWith(`:${uri}`));
			const forwardedDirectoryCount = serverObserver?.count() ?? 0;
			const eventEvidence = {
				version: vscode.version,
				platform: process.platform,
				hostEventTypes: {
					directory: observed(directoryUri),
					texture: observed(textureUri),
					script: observed(scriptUri),
				},
				forwardedDirectoryCount,
				cw113Cleared,
			};
			console.log(`folder move event evidence ${JSON.stringify(eventEvidence)}`);
			assert.ok(cw113Cleared, "CW113 should clear from the moved texture without a reindex");
			assert.ok(observed(directoryUri).some((event) => event.startsWith("create:")), `VS Code reported no moved directory create event: ${JSON.stringify(hostEvents)}`);
			assert.ok(forwardedDirectoryCount > 0, "server received no forwarded event for the moved directory");
		} finally {
			watcher.dispose();
			serverObserver?.dispose();
			await fs.writeFile(openFile, original);
			await fs.rm(source, { recursive: true, force: true });
			await fs.rm(destination, { recursive: true, force: true });
			await settings.update("cache.stellaris", previousVanilla, vscode.ConfigurationTarget.Global);
			if (apiForCleanup) {
				await vscode.commands.executeCommand("cwtools.restartServer");
				assert.ok(await waitForServerReady(apiForCleanup), "server did not restore its original vanilla configuration");
			}
		}
	});

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
				new vscode.RelativePattern(folder.uri, `{${relativePath},${READY_PROBE}}`),
			);
			watcher.onDidCreate((uri) => { observedEvents.add(`create:${uri.fsPath}`); });
			watcher.onDidChange((uri) => { observedEvents.add(`change:${uri.fsPath}`); });
			watcher.onDidDelete((uri) => { observedEvents.add(`delete:${uri.fsPath}`); });
			return watcher;
		});
		try {
			// VS Code starts a watch asynchronously, signals no readiness and drops
			// events from before it is live, so rewrite a probe until each observer
			// reports it.
			for (const folder of folders) {
				const ready = path.join(folder.uri.fsPath, READY_PROBE);
				let lastProbe = 0;
				try {
					assert.ok(
						await waitUntil(async () => {
							if (observedEvents.has(`create:${ready}`) || observedEvents.has(`change:${ready}`)) return true;
							if (Date.now() - lastProbe >= 500) {
								lastProbe = Date.now();
								await fs.writeFile(ready, String(lastProbe));
							}
							return false;
						}, 20_000),
						`the observer never went live: ${folder.uri.fsPath}`,
					);
				} finally {
					await fs.rm(ready, { force: true });
				}
			}
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
