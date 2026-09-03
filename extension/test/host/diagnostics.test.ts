import * as assert from "assert";
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

	setup(async function () {
		await activate();
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

	test("publishes no diagnostics for a clean file", function () {
		assert.deepStrictEqual(
			vscode.languages.getDiagnostics(cleanDocument.uri),
			[],
		);
	});
});
