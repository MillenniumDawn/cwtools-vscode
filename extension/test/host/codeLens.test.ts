import * as assert from "assert";
import * as path from "path";
import * as vscode from "vscode";
import {
	activate,
	EXTENSION_ID,
	openDocumentAndShow,
	SAMPLE_ROOT,
	waitForLSP,
} from "../support/utils";
import {
	checkForLSPErrors,
	setupLSPErrorMonitoring,
	teardownLSPErrorMonitoring,
} from "../support/lspErrorMonitor";

const testEventFile = path.join(SAMPLE_ROOT, "events", "irm_sector.txt");

async function codeLenses(uri: vscode.Uri): Promise<vscode.CodeLens[]> {
	return (
		(await vscode.commands.executeCommand<vscode.CodeLens[]>(
			"vscode.executeCodeLensProvider",
			uri,
			100,
		)) ?? []
	);
}

function lensForDefinition(
	document: vscode.TextDocument,
	lenses: vscode.CodeLens[],
	instanceName: string,
): vscode.CodeLens | undefined {
	return lenses.find((lens) => {
		const idLine = lens.range.start.line + 1;
		return (
			idLine < document.lineCount &&
			document.lineAt(idLine).text.includes(`id = ${instanceName}`)
		);
	});
}

suite("LSP CodeLens Tests", function () {
	this.timeout(60000);

	let document: vscode.TextDocument;

	setup(async function () {
		await activate();
		setupLSPErrorMonitoring();
		const extension = vscode.extensions.getExtension(EXTENSION_ID)!;
		assert.ok(extension?.isActive, "Extension should be active");
		document = await openDocumentAndShow(vscode.Uri.file(testEventFile));
		await waitForLSP(document.uri);
	});

	teardown(async function () {
		await vscode.commands.executeCommand("workbench.action.closeAllEditors");
		checkForLSPErrors(this.currentTest?.title || "unknown test");
	});

	suiteTeardown(() => teardownLSPErrorMonitoring());

	test("resolves type definitions to Show References commands", async function () {
		const lenses = await codeLenses(document.uri);
		const unused = lensForDefinition(document, lenses, "irm_sector.1051");
		assert.strictEqual(unused?.command?.title, "0 references");
		assert.strictEqual(unused?.command?.command, "cwtools.showReferences");
		assert.strictEqual(unused?.command?.arguments?.[0], document.uri.toString());
		assert.deepStrictEqual(unused?.command?.arguments?.[2], []);

		const commands = await vscode.commands.getCommands();
		assert.ok(
			commands.includes("cwtools.showReferences"),
			"Show References bridge command is not registered",
		);
	});
});
