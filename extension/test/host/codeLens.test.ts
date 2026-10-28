import * as assert from "assert";
import * as path from "path";
import * as sinon from "sinon";
import * as vscode from "vscode";
import {
	activate,
	EXTENSION_ID,
	openDocumentAndShow,
	SAMPLE_ROOT,
	waitForServerReady,
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
	let sandbox: sinon.SinonSandbox;
	let showReferences: sinon.SinonStub;

	setup(async function () {
		const api = await activate();
		assert.ok(api, "activation API should be exposed");
		sandbox = sinon.createSandbox();
		showReferences = sandbox.stub(vscode.commands, "executeCommand")
			.callThrough()
			.withArgs("editor.action.showReferences")
			.resolves(undefined);
		await setupLSPErrorMonitoring();
		const extension = vscode.extensions.getExtension(EXTENSION_ID)!;
		assert.ok(extension?.isActive, "Extension should be active");
		document = await openDocumentAndShow(vscode.Uri.file(testEventFile));
		assert.ok(await waitForServerReady(api), "server did not finish indexing");
	});

	teardown(async function () {
		sandbox.restore();
		await vscode.commands.executeCommand("workbench.action.closeAllEditors");
		checkForLSPErrors(this.currentTest?.title || "unknown test");
	});

	suiteTeardown(() => teardownLSPErrorMonitoring());

	const definitions = [
		[testEventFile, "irm_sector.1051", 0],
		[path.join(SAMPLE_ROOT, "events", "irm_codelens.txt"), "irm_codelens.target", 2],
	] as const;
	for (const [file, instance, count] of definitions) {
		test(`opens the references from the server's CodeLens for ${instance}`, async function () {
			document = await openDocumentAndShow(vscode.Uri.file(file));
			const lenses = await codeLenses(document.uri);
			const command = lensForDefinition(document, lenses, instance)?.command;
			assert.ok(command, `missing CodeLens for ${instance}`);
			assert.strictEqual(command.title, `${count} references`);
			assert.strictEqual(command.command, "cwtools.showReferences");
			const [uri, position, locations] = command.arguments as [
				string,
				{ line: number; character: number },
				Array<{ uri: string; range: { start: vscode.Position; end: vscode.Position } }>,
			];
			assert.strictEqual(uri, document.uri.toString());
			assert.strictEqual(locations.length, count);

			await vscode.commands.executeCommand(command.command, uri, position, locations);

			assert.strictEqual(showReferences.callCount, 1);
			assert.deepStrictEqual(showReferences.firstCall.args, [
				"editor.action.showReferences",
				vscode.Uri.parse(uri),
				new vscode.Position(position.line, position.character),
				locations.map((location) => new vscode.Location(
					vscode.Uri.parse(location.uri),
					new vscode.Range(
						location.range.start.line, location.range.start.character,
						location.range.end.line, location.range.end.character,
					),
				)),
			]);
		});
	}

	for (const coordinate of [0, Number.MAX_SAFE_INTEGER]) {
		test(`accepts reference coordinates at the ${coordinate} boundary`, async function () {
			const uri = document.uri.toString();
			const position = { line: coordinate, character: coordinate };
			await vscode.commands.executeCommand("cwtools.showReferences", uri, position, [
				{ uri, range: { start: position, end: position } },
			]);

			assert.strictEqual(showReferences.callCount, 1);
			const expected = new vscode.Position(coordinate, coordinate);
			assert.deepStrictEqual(showReferences.firstCall.args, [
				"editor.action.showReferences", vscode.Uri.parse(uri), expected,
				[new vscode.Location(vscode.Uri.parse(uri), new vscode.Range(expected, expected))],
			]);
		});
	}

	const position = { line: 0, character: 0 };
	const range = { start: position, end: position };
	const uri = vscode.Uri.file(testEventFile).toString();
	const invalidArguments: Array<[string, unknown, unknown, unknown]> = [
		["a non-string URI", null, position, []],
		["a non-array reference list", uri, position, null],
		["a missing origin", uri, null, []],
		["a negative line", uri, { line: -1, character: 0 }, []],
		["a negative character", uri, { line: 0, character: -1 }, []],
		["a fractional line", uri, { line: 0.5, character: 0 }, []],
		["a fractional character", uri, { line: 0, character: 0.5 }, []],
		["an unsafe line", uri, { line: Number.MAX_SAFE_INTEGER + 1, character: 0 }, []],
		["a non-finite character", uri, { line: 0, character: Infinity }, []],
		["a missing location", uri, position, [null]],
		["a non-string location URI", uri, position, [{ uri: 1, range }]],
		["a missing range", uri, position, [{ uri }]],
		["an invalid range start", uri, position, [{ uri, range: { start: { line: -1, character: 0 }, end: position } }]],
		["an invalid range end", uri, position, [{ uri, range: { start: position, end: { line: 0, character: -1 } } }]],
	];
	for (const [name, origin, start, locations] of invalidArguments) {
		test(`does not open references for ${name}`, async function () {
			await vscode.commands.executeCommand("cwtools.showReferences", origin, start, locations);

			assert.strictEqual(showReferences.callCount, 0);
		});
	}
});
