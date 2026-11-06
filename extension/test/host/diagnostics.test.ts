import * as assert from "assert";
import * as fs from "fs/promises";
import * as path from "path";
import * as sinon from "sinon";
import * as vscode from "vscode";
import {
	activate,
	EXTENSION_ID,
	openDocumentAndShow,
	SAMPLE_ROOT,
	waitForLanguageServer,
	waitUntil,
} from "../support/utils";

const diagnosticsFile = path.join(SAMPLE_ROOT, "events", "irm_diagnostics.txt");
const cleanFile = path.join(SAMPLE_ROOT, "events", "irm_diagnostics_clean.txt");

const isCW263 = (diagnostic: vscode.Diagnostic): boolean =>
	diagnostic.code === "CW263" ||
	(typeof diagnostic.code === "object" && diagnostic.code.value === "CW263");

suite("LSP Diagnostics Tests", function () {
	this.timeout(120000);

	let diagnosticsDocument: vscode.TextDocument;
	let cleanDocument: vscode.TextDocument;
	let api: Awaited<ReturnType<typeof activate>>;

	setup(async function () {
		api = await activate();
		const extension = vscode.extensions.getExtension(EXTENSION_ID)!;
		assert.ok(extension?.isActive, "Extension should be active");

		cleanDocument = await openDocumentAndShow(vscode.Uri.file(cleanFile));
		diagnosticsDocument = await openDocumentAndShow(
			vscode.Uri.file(diagnosticsFile),
		);
		for (const document of [diagnosticsDocument, cleanDocument]) {
			assert.ok(
				await waitForLanguageServer(document.uri, 30000),
				"Language server did not become ready",
			);
		}

		const received = await waitUntil(
			() =>
				vscode.languages.getDiagnostics(diagnosticsDocument.uri).some(isCW263),
			30000,
		);
		assert.ok(received, "Expected CW263 to be published for the fixture");
	});

	teardown(async function () {
		await vscode.commands.executeCommand("workbench.action.closeAllEditors");
	});

	test("publishes a diagnostic with its code and range", function () {
		const diagnostic = vscode.languages
			.getDiagnostics(diagnosticsDocument.uri)
			.find(isCW263);
		assert.ok(diagnostic, "Expected a CW263 diagnostic");
		assert.ok(
			diagnosticsDocument
				.getText(diagnostic.range)
				.includes("unknown_diagnostic_field"),
			"CW263 should cover unknown_diagnostic_field",
		);
	});

	test("ignores a diagnostic through the server's client command", async function () {
		const cfg = vscode.workspace.getConfiguration("cwtools", diagnosticsDocument.uri);
		const previous = cfg.inspect<string[]>("errors.ignore")?.workspaceValue;
		try {
			const diagnostic = vscode.languages.getDiagnostics(diagnosticsDocument.uri).find(isCW263)!;
			const actions = await vscode.commands.executeCommand<vscode.CodeAction[]>(
				"vscode.executeCodeActionProvider", diagnosticsDocument.uri, diagnostic.range, vscode.CodeActionKind.QuickFix.value,
			);
			const action = actions?.find((candidate) => candidate.command?.command === "cwtools.ignoreDiagnosticCode");
			assert.ok(action?.command, "server should offer the client-owned ignore action");
			assert.strictEqual(action.edit, undefined, "the server must not supply a settings replacement");
			const args = action.command.arguments as unknown[] | undefined;
			assert.strictEqual(await vscode.commands.executeCommand(action.command.command, ...args ?? []), true);
			assert.ok(await waitUntil(() => !vscode.languages.getDiagnostics(diagnosticsDocument.uri).some(isCW263), 30_000));
		} finally {
			await cfg.update("errors.ignore", previous, vscode.ConfigurationTarget.WorkspaceFolder);
		}
		assert.ok(await waitUntil(() => vscode.languages.getDiagnostics(diagnosticsDocument.uri).some(isCW263), 30_000));
	});

	test("rejects invalid code-action arguments without changing the target", async function () {
		const original = diagnosticsDocument.getText();
		const edit = new vscode.WorkspaceEdit();
		edit.insert(diagnosticsDocument.uri, new vscode.Position(0, 0), "# Must not apply\n");
		const uri = diagnosticsDocument.uri.toString();
		const cases: [unknown, unknown, string][] = [
			[{}, [], "The code action edit is invalid."],
			[edit, undefined, "The code action edit is invalid."],
			...[null, {}, { uri: 42, version: null }, { uri }, { uri, version: "1" }, { uri, version: 1.5 }]
				.map((target): [unknown, unknown, string] => [edit, [target], "The code action target version is invalid."]),
		];
		const errors = sinon.stub(vscode.window, "showErrorMessage").resolves(undefined);
		try {
			for (const [candidate, versions, message] of cases) {
				errors.resetHistory();
				assert.strictEqual(await vscode.commands.executeCommand("cwtools.applyCodeAction", candidate, versions), false);
				assert.strictEqual(diagnosticsDocument.getText(), original);
				assert.strictEqual(errors.callCount, 1);
				assert.strictEqual(errors.firstCall.args[0], `CWTools: could not apply quick fix: ${message}`);
			}
		} finally {
			errors.restore();
		}
	});

	test("reports a refused workspace edit without changing an existing file", async function () {
		const original = diagnosticsDocument.getText();
		const disk = await fs.readFile(diagnosticsDocument.uri.fsPath, "utf8");
		const edit = new vscode.WorkspaceEdit();
		edit.createFile(diagnosticsDocument.uri, { overwrite: false, ignoreIfExists: false });
		const errors = sinon.stub(vscode.window, "showErrorMessage").resolves(undefined);
		try {
			assert.strictEqual(await vscode.commands.executeCommand("cwtools.applyCodeAction", edit, [
				{ uri: diagnosticsDocument.uri.toString(), version: diagnosticsDocument.version },
			]), false);
			assert.strictEqual(diagnosticsDocument.getText(), original);
			assert.strictEqual(await fs.readFile(diagnosticsDocument.uri.fsPath, "utf8"), disk);
			assert.strictEqual(errors.callCount, 1);
			assert.strictEqual(errors.firstCall.args[0], "CWTools: could not apply quick fix: The code action edit could not be applied.");
		} finally {
			errors.restore();
		}
	});

	test("writes a baseline, suppresses old errors, and restores them when cleared", async function () {
		const cfg = vscode.workspace.getConfiguration("cwtools", diagnosticsDocument.uri);
		const baselineSetting = "diagnostics.baselineFile";
		const target = ".cwtools-baseline";
		const originalText = diagnosticsDocument.getText();
		try {
			await vscode.commands.executeCommand("cwtools.writeDiagnosticsBaseline", target);
			assert.ok(
				await waitUntil(() => vscode.workspace
					.getConfiguration("cwtools", diagnosticsDocument.uri)
					.get<string>(baselineSetting) === target, 10_000),
				"the selected folder should enable the written baseline",
			);
			assert.ok(
				await waitUntil(
					() => !vscode.languages.getDiagnostics(diagnosticsDocument.uri).some(isCW263),
					30_000,
				),
				"the existing CW263 should be suppressed after writing the baseline",
			);
			assert.ok(
				await waitUntil(() => {
					return api?.serverStatusText()?.includes("baseline-suppressed") ?? false;
				}, 30_000),
				"status should display the baseline suppression count",
			);
			const added = "\tunknown_new_baseline_field = yes\n";
			const edit = new vscode.WorkspaceEdit();
			edit.insert(diagnosticsDocument.uri, diagnosticsDocument.positionAt(originalText.lastIndexOf("}")), added);
			assert.ok(await vscode.workspace.applyEdit(edit), "should add a new diagnostic fixture line");
			assert.ok(
				await waitUntil(
					() => vscode.languages.getDiagnostics(diagnosticsDocument.uri).some((diagnostic) =>
						isCW263(diagnostic) && diagnosticsDocument.getText(diagnostic.range).includes("unknown_new_baseline_field")),
					30_000,
				),
				"a new CW263 should remain visible while the previous one is suppressed",
			);
			await cfg.update(baselineSetting, "", vscode.ConfigurationTarget.WorkspaceFolder);
			assert.ok(
				await waitUntil(
					() => vscode.languages.getDiagnostics(diagnosticsDocument.uri).some((diagnostic) =>
						isCW263(diagnostic) && diagnosticsDocument.getText(diagnostic.range).includes("unknown_diagnostic_field")),
					30_000,
				),
				"clearing the setting should restore the original diagnostic",
			);
		} finally {
			const restore = new vscode.WorkspaceEdit();
			restore.replace(diagnosticsDocument.uri, new vscode.Range(0, 0, diagnosticsDocument.lineCount, 0), originalText);
			await vscode.workspace.applyEdit(restore);
			await cfg.update(baselineSetting, undefined, vscode.ConfigurationTarget.WorkspaceFolder);
			await fs.rm(path.join(SAMPLE_ROOT, target), { force: true });
		}
	});

	test("rejects an absolute baseline target without changing the setting", async function () {
		const before = vscode.workspace.getConfiguration("cwtools", diagnosticsDocument.uri)
			.get<string>("diagnostics.baselineFile", "");
		const target = path.resolve(SAMPLE_ROOT, "outside-baseline");
		await vscode.commands.executeCommand("cwtools.writeDiagnosticsBaseline", target);
		assert.strictEqual(vscode.workspace.getConfiguration("cwtools", diagnosticsDocument.uri)
			.get<string>("diagnostics.baselineFile", ""), before);
		await assert.rejects(fs.stat(target), { code: "ENOENT" });
	});

	test("publishes no diagnostics for a clean file", function () {
		assert.deepStrictEqual(
			vscode.languages.getDiagnostics(cleanDocument.uri),
			[],
		);
	});
});
