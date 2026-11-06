import * as assert from "node:assert";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import * as vscode from "vscode";
import { activate, waitUntil } from "../support/utils";

suite("Ignore diagnostic code", function () {
	this.timeout(30000);
	let fixture: Awaited<ReturnType<typeof fs.mkdtempDisposable>>;
	let folder: vscode.Uri;
	let settings: vscode.Uri;
	let workspaceFile: vscode.Uri;
	let workspaceText: string;

	suiteSetup(async () => {
		assert.ok(await activate(), "extension should activate");
		assert.ok(vscode.workspace.workspaceFile, "run this suite in the multi-root fixture");
		workspaceFile = vscode.workspace.workspaceFile;
		workspaceText = await fs.readFile(workspaceFile.fsPath, "utf8");
	});

	suiteTeardown(async () => {
		await fs.writeFile(workspaceFile.fsPath, workspaceText);
	});

	setup(async () => {
		fixture = await fs.mkdtempDisposable(path.join(os.tmpdir(), "cwtools-ignore-"));
		folder = vscode.Uri.file(path.join(fixture.path, "workspace"));
		settings = vscode.Uri.joinPath(folder, ".vscode", "settings.json");
		await fs.mkdir(folder.fsPath);
	});

	teardown(async () => {
		const document = vscode.workspace.textDocuments.find((doc) => doc.uri.toString() === settings.toString());
		if (document && !document.isClosed) {
			await vscode.window.showTextDocument(document);
			await vscode.commands.executeCommand("workbench.action.revertAndCloseActiveEditor");
		}
		await changeFolder(false);
		await fixture.remove();
	});

	async function attachWorkspace(content?: string): Promise<void> {
		if (content !== undefined) {
			await fs.mkdir(path.dirname(settings.fsPath), { recursive: true });
			await fs.writeFile(settings.fsPath, content);
		}
		await changeFolder(true);
	}

	async function changeFolder(add: boolean, target: vscode.Uri = folder): Promise<void> {
		const folders = vscode.workspace.workspaceFolders ?? [];
		const index = add ? folders.length : folders.findIndex((entry) => entry.uri.toString() === target.toString());
		if (index < 0) return;
		const changed = new Promise<void>((resolve) => {
			const subscription = vscode.workspace.onDidChangeWorkspaceFolders((event) => {
				if ((add ? event.added : event.removed).some((entry) => entry.uri.toString() === target.toString())) {
					subscription.dispose();
					resolve();
				}
			});
		});
		assert.ok(vscode.workspace.updateWorkspaceFolders(index, add ? 0 : 1, ...(add ? [{ uri: target }] : [])));
		await changed;
	}

	async function ignore(code: unknown = "CW100", uri: unknown = folder.toString()): Promise<boolean | undefined> {
		const result = await vscode.commands.executeCommand<boolean>("cwtools.ignoreDiagnosticCode", uri, code);
		if (result && !vscode.workspace.textDocuments.some((doc) => doc.uri.toString() === settings.toString() && doc.isDirty)) {
			assert.ok(await waitUntil(() => {
				const codes = ignored();
				return Array.isArray(codes) && codes.some((entry: unknown) => typeof entry === "string" && entry.toUpperCase() === code);
			}), "saved settings should become effective");
		}
		return result;
	}

	function ignored(): unknown {
		return vscode.workspace.getConfiguration("cwtools", folder).get("errors.ignore");
	}

	test("creates missing folder settings in the diagnostic's workspace", async () => {
		await attachWorkspace();
		assert.strictEqual(await ignore(), true);
		const content = JSON.parse(await fs.readFile(settings.fsPath, "utf8")) as Record<string, unknown>;
		assert.deepStrictEqual(content["cwtools.errors.ignore"], ["CW100"]);
		assert.deepStrictEqual(ignored(), ["CW100"]);
	});

	test("keeps the server-selected root when a child folder is also open", async () => {
		await attachWorkspace();
		const child = vscode.Uri.joinPath(folder, "nested");
		await fs.mkdir(child.fsPath);
		await changeFolder(true, child);
		try {
			assert.strictEqual(await ignore(), true);
			assert.deepStrictEqual(ignored(), ["CW100"]);
			await assert.rejects(fs.stat(path.join(child.fsPath, ".vscode", "settings.json")), { code: "ENOENT" });
		} finally {
			await changeFolder(false, child);
		}
	});

	test("preserves JSONC comments, trailing commas, and unrelated settings", async () => {
		await attachWorkspace(`{
	// Keep this comment.
	"editor.formatOnSave": false,
	"cwtools.errors.ignore": ["CW200"],
}\n`);
		assert.strictEqual(await ignore(), true);
		const text = await fs.readFile(settings.fsPath, "utf8");
		assert.ok(text.includes("// Keep this comment."), text);
		assert.ok(text.includes('"editor.formatOnSave": false,'), text);
		assert.deepStrictEqual(ignored(), ["CW200", "CW100"]);
	});

	test("preserves comments within an existing ignore array", async () => {
		await attachWorkspace('{"cwtools.errors.ignore": [\n // Keep the reason.\n "CW200",\n]}\n');
		assert.strictEqual(await ignore(), true);
		assert.deepStrictEqual(ignored(), ["CW200", "CW100"]);
		assert.ok((await fs.readFile(settings.fsPath, "utf8")).includes("// Keep the reason."));
	});

	test("preserves comments when flattening and moving legacy ignore lists", async () => {
		await attachWorkspace(`{
	// Above the flat key.
	"cwtools.errors.ignore": [/* Inside flat */ "CW200"],
	"cwtools": { "errors": {
		// Above the nested key.
		"ignore": [/* Inside nested */ "CW300"]
	} }
}`);
		assert.strictEqual(await ignore(), true);
		assert.deepStrictEqual(ignored(), ["CW200", "CW300", "CW100"]);
		const text = await fs.readFile(settings.fsPath, "utf8");
		for (const comment of ["// Above the flat key.", "/* Inside flat */", "// Above the nested key.", "/* Inside nested */"]) {
			assert.ok(text.includes(comment), `lost ${comment}: ${text}`);
		}
	});

	test("updates the effective flat setting rather than a shadowed nested list", async () => {
		await attachWorkspace('{"cwtools.errors.ignore":["CW200"],"cwtools":{"errors":{"ignore":["CW300"],"keep":true}}}\n');
		assert.strictEqual(await ignore(), true);
		assert.deepStrictEqual(ignored(), ["CW200", "CW300", "CW100"]);
		const content = JSON.parse(await fs.readFile(settings.fsPath, "utf8")) as { cwtools: { errors: Record<string, unknown> } };
		assert.deepStrictEqual(content.cwtools.errors, { keep: true });
	});

	test("moves an existing flat code past a shadowing namespace", async () => {
		await attachWorkspace('{"cwtools.errors.ignore":["CW100"],"cwtools":{"hover":{"debug":true}}}\n');
		assert.strictEqual(await ignore(), true);
		assert.deepStrictEqual(ignored(), ["CW100"]);
		const content = JSON.parse(await fs.readFile(settings.fsPath, "utf8")) as { cwtools: { hover: { debug: boolean } } };
		assert.strictEqual(content.cwtools.hover.debug, true);
	});

	test("keeps effective legacy nested entries when adding a flat setting", async () => {
		await attachWorkspace('{"cwtools":{"errors":{"ignore":["CW200"]}}}\n');
		assert.strictEqual(await ignore(), true);
		assert.deepStrictEqual(ignored(), ["CW200", "CW100"]);
	});

	test("deduplicates codes case-insensitively without rewriting the file", async () => {
		const original = '{"cwtools.errors.ignore":["cw100"]}\n';
		await attachWorkspace(original);
		assert.strictEqual(await ignore(), true);
		assert.strictEqual(await fs.readFile(settings.fsPath, "utf8"), original);
	});

	test("adds a code to empty settings", async () => {
		await attachWorkspace("{}\n");
		assert.strictEqual(await ignore(), true);
		assert.deepStrictEqual(ignored(), ["CW100"]);
	});

	test("uses settings at command execution rather than action creation", async () => {
		await attachWorkspace('{"cwtools.errors.ignore":["CW200"]}\n');
		const action = [folder.toString(), "CW100"];
		await vscode.workspace.getConfiguration("cwtools", folder).update("errors.ignore", ["CW200", "CW300"], vscode.ConfigurationTarget.WorkspaceFolder);
		assert.strictEqual(await ignore(action[1], action[0]), true);
		assert.deepStrictEqual(ignored(), ["CW200", "CW300", "CW100"]);
	});

	test("preserves dirty settings and leaves them unsaved", async () => {
		const original = '{"editor.formatOnSave":false}\n';
		await attachWorkspace(original);
		const document = await vscode.workspace.openTextDocument(settings);
		await vscode.window.showTextDocument(document);
		const edit = new vscode.WorkspaceEdit();
		edit.insert(settings, new vscode.Position(0, 0), "// Unsaved user edit\n");
		assert.ok(await vscode.workspace.applyEdit(edit));
		assert.ok(document.isDirty);
		assert.strictEqual(await ignore(), true);
		assert.ok(document.getText().startsWith("// Unsaved user edit\n"));
		assert.match(document.getText(), /"editor.formatOnSave"\s*:\s*false/);
		assert.ok(document.getText().includes('"cwtools.errors.ignore"'));
		assert.ok(document.isDirty);
		assert.strictEqual(await fs.readFile(settings.fsPath, "utf8"), original);
		assert.ok(await document.save());
		assert.ok(await waitUntil(() => JSON.stringify(ignored()) === '["CW100"]'));
	});

	for (const original of ['{"broken":', "[]", "null", '{"cwtools.errors.ignore":"CW200"}', '{"cwtools":false}']) {
		test(`refuses invalid settings without overwriting: ${original}`, async () => {
			await attachWorkspace(original);
			assert.strictEqual(await ignore(), false);
			assert.strictEqual(await fs.readFile(settings.fsPath, "utf8"), original);
		});
	}

	test("refuses unreadable settings without overwriting", async () => {
		const original = '{"editor.formatOnSave":false}\n';
		await attachWorkspace(original);
		await fs.chmod(settings.fsPath, 0o000);
		try {
			assert.strictEqual(await ignore(), false);
		} finally {
			await fs.chmod(settings.fsPath, 0o600);
		}
		assert.strictEqual(await fs.readFile(settings.fsPath, "utf8"), original);
	});

	test("refuses a settings symlink outside the workspace", async () => {
		const outside = path.join(fixture.path, "outside.json");
		const original = '{"editor.formatOnSave":false}\n';
		await fs.writeFile(outside, original);
		await fs.mkdir(path.dirname(settings.fsPath));
		await fs.symlink(outside, settings.fsPath);
		await attachWorkspace();
		assert.strictEqual(await ignore(), false);
		assert.strictEqual(await fs.readFile(outside, "utf8"), original);
	});

	test("refuses a symlinked .vscode directory outside the workspace", async () => {
		const outside = path.join(fixture.path, "outside");
		await fs.mkdir(outside);
		await fs.symlink(outside, path.dirname(settings.fsPath));
		await attachWorkspace();
		assert.strictEqual(await ignore(), false);
		assert.deepStrictEqual(await fs.readdir(outside), []);
	});

	test("refuses a directory where settings.json should be", async () => {
		await fs.mkdir(settings.fsPath, { recursive: true });
		await attachWorkspace();
		assert.strictEqual(await ignore(), false);
		assert.ok((await fs.stat(settings.fsPath)).isDirectory());
	});

	test("refuses a regular file where .vscode should be", async () => {
		await fs.writeFile(path.dirname(settings.fsPath), "keep");
		await attachWorkspace();
		assert.strictEqual(await ignore(), false);
		assert.strictEqual(await fs.readFile(path.dirname(settings.fsPath), "utf8"), "keep");
	});

	test("refuses invalid arguments and files outside workspace folders", async () => {
		await attachWorkspace();
		assert.strictEqual(await ignore("arbitrary.setting"), false);
		assert.strictEqual(await ignore("CW100", 42), false);
		assert.strictEqual(await ignore("CW100", vscode.Uri.file(path.join(fixture.path, "outside.txt")).toString()), false);
		assert.deepStrictEqual(await fs.readdir(folder.fsPath), []);
	});

	test("serializes concurrent updates without losing either code", async () => {
		await attachWorkspace();
		assert.deepStrictEqual(await Promise.all([ignore("CW100"), ignore("CW200")]), [true, true]);
		assert.deepStrictEqual(ignored(), ["CW100", "CW200"]);
	});
});
