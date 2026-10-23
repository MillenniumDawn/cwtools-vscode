import * as assert from "assert";
import * as fs from "fs/promises";
import * as path from "path";
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

	test("writes a baseline, suppresses old errors, and restores them when cleared", async function () {
		const cfg = vscode.workspace.getConfiguration("cwtools");
		const baselineSetting = "diagnostics.baselineFile";
		const target = ".cwtools-baseline";
		const originalText = diagnosticsDocument.getText();
		try {
			await vscode.commands.executeCommand("cwtools.writeDiagnosticsBaseline", target);
			assert.strictEqual(cfg.get<string>(baselineSetting), target);
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
			const added = "\nunknown_new_baseline_field = yes\n";
			const edit = new vscode.WorkspaceEdit();
			edit.insert(diagnosticsDocument.uri, new vscode.Position(diagnosticsDocument.lineCount, 0), added);
			assert.ok(await vscode.workspace.applyEdit(edit), "should add a new diagnostic fixture line");
			assert.ok(
				await waitUntil(
					() => vscode.languages.getDiagnostics(diagnosticsDocument.uri).some(isCW263),
					30_000,
				),
				"a new CW263 should remain visible while the previous one is suppressed",
			);
			await cfg.update(baselineSetting, "", vscode.ConfigurationTarget.WorkspaceFolder);
			assert.ok(
				await waitUntil(
					() => vscode.languages.getDiagnostics(diagnosticsDocument.uri).some(isCW263),
					30_000,
				),
				"clearing the setting should restore the diagnostic",
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
		const cfg = vscode.workspace.getConfiguration("cwtools");
		const before = cfg.get<string>("diagnostics.baselineFile", "");
		await vscode.commands.executeCommand("cwtools.writeDiagnosticsBaseline", path.resolve(SAMPLE_ROOT, "outside-baseline"));
		assert.strictEqual(cfg.get<string>("diagnostics.baselineFile", ""), before);
	});

	test("publishes no diagnostics for a clean file", function () {
		assert.deepStrictEqual(
			vscode.languages.getDiagnostics(cleanDocument.uri),
			[],
		);
	});
});
