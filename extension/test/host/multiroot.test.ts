import * as assert from "assert";
import * as path from "node:path";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as vscode from "vscode";
import { activate, waitForServerReady, waitUntil } from "../support/utils";

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

		// `selected.txt` is opened through launchArgs, so its diagnostics come
		// from didOpen. The unopened file only gets diagnostics if the server
		// actually scanned the root, which is what this test exists to prove.
		const unrelated = vscode.Uri.file(
			path.join(folders[0].uri.fsPath, "events", "unrelated.txt"),
		);
		const unopened = vscode.Uri.file(
			path.join(folders[1].uri.fsPath, "events", "unopened.txt"),
		);
		assert.deepStrictEqual(
			vscode.languages.getDiagnostics(unrelated),
			[],
			"the unrelated first root must not be scanned",
		);
		assert.ok(
			await waitUntil(() => vscode.languages.getDiagnostics(unopened).length > 0),
			"the descriptor-bearing root should be scanned",
		);
	});
	test("keeps unrelated and external plaintext files while promoting selected mod script", async () => {
		await activate();
		const folders = vscode.workspace.workspaceFolders ?? [];
		const temp = await fs.mkdtemp(path.join(os.tmpdir(), "cwtools-language-"));
		const probeRoots = await Promise.all(folders.map((folder) =>
			fs.mkdtemp(path.join(folder.uri.fsPath, "cwtools-language-")),
		));
		const files = [
			path.join(probeRoots[1], "common", "probe.txt"),
			path.join(probeRoots[0], "common", "probe.txt"),
			path.join(temp, "common", "notes.txt"),
			path.join(temp, "unrelated.gfx"),
		];
		try {
			for (const file of files) {
				await fs.mkdir(path.dirname(file), { recursive: true });
				await fs.writeFile(file, "probe = yes\n");
				const doc = await vscode.workspace.openTextDocument(vscode.Uri.file(file));
				await vscode.languages.setTextDocumentLanguage(doc, "plaintext");
			}
			const language = (file: string) => vscode.workspace.textDocuments.find((doc) => doc.uri.fsPath === file)?.languageId;
			assert.ok(await waitUntil(() => language(files[0]) === "paradox"));
			// Give open/language-change callbacks a turn to settle for every doc.
			await new Promise((resolve) => setTimeout(resolve, 250));
			for (const file of files.slice(1)) assert.strictEqual(language(file), "plaintext", file);
		} finally {
			for (const root of probeRoots) await fs.rm(root, { recursive: true, force: true });
			await fs.rm(temp, { recursive: true, force: true });
		}
	});

});
