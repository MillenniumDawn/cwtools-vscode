import * as assert from "assert";
import * as path from "path";
import * as sinon from "sinon";
import * as vscode from "vscode";
import {
	activate,
	openDocumentAndShow,
	SAMPLE_ROOT,
	waitForLanguageServer,
	waitUntil,
} from "../support/utils";

// The gating host tests only prove the workspace commands don't dead-end; a
// handler that became a no-op still passes them. This suite pins the boundary
// itself: a real server edit coming back through the client's applyEdit path.

// Not referenced by any other suite, so dirtying its buffer in memory can't
// shift another suite's expectations. It is never saved, so the fixture on
// disk stays byte-identical.
const targetFile = path.join(
	SAMPLE_ROOT,
	"common",
	"edicts",
	"irm_planetary_edicts.txt",
);

suite("workspace commands cross the LSP boundary", function () {
	this.timeout(120000);

	let sandbox: sinon.SinonSandbox | undefined;
	let document: vscode.TextDocument | undefined;

	setup(async function () {
		await activate();
	});

	teardown(async function () {
		sandbox?.restore();
		if (document) {
			await vscode.window.showTextDocument(document);
			// Discard the dirty buffer without saving, so nothing on disk moves
			// and later suites see the committed fixture text.
			await vscode.commands.executeCommand("workbench.action.files.revert");
		}
	});

	test("formatWorkspace routes the server's edit through workspace.applyEdit", async function () {
		const api = await activate();
		assert.ok(api, "activation API should be exposed");
		const advertised = api.serverCommands();
		assert.ok(
			advertised.includes("formatWorkspace"),
			`pinned server should advertise formatWorkspace, got: ${advertised.join(", ")}`,
		);

		document = await openDocumentAndShow(vscode.Uri.file(targetFile));
		assert.ok(
			await waitForLanguageServer(document.uri, 30000),
			"language server did not become ready",
		);

		// The command formats whatever text the server holds, so capture the
		// pristine formatter result before dirtying the buffer.
		const probe = () =>
			vscode.commands.executeCommand<vscode.TextEdit[]>(
				"vscode.executeFormatDocumentProvider",
				document!.uri,
				{ tabSize: 4, insertSpaces: true },
			);
		const before = await probe();

		// Dirty the buffer in memory only: a trailing-whitespace tail the
		// formatter removes. The edit is never saved.
		const realApplyEdit = vscode.workspace.applyEdit.bind(vscode.workspace);
		const dirty = new vscode.WorkspaceEdit();
		dirty.insert(
			document.uri,
			document.positionAt(document.getText().length),
			"   ",
		);
		assert.ok(await realApplyEdit(dirty), "dirtying the buffer should succeed");

		// Wait until the formatter reflects the dirty buffer before starting the
		// command, otherwise the edit could be planned against pristine text and
		// the assertion would race.
		const dirtyReachedServer = await waitUntil(async () => {
			const now = await probe();
			return (
				now !== undefined && JSON.stringify(now) !== JSON.stringify(before)
			);
		}, 30000);
		assert.ok(
			dirtyReachedServer,
			"server never picked up the dirty buffer text",
		);

		sandbox = sinon.createSandbox();
		// Capture instead of applying: the server's edit never touches the
		// fixture, and the server still sees a successful apply.
		const applyEdit = sandbox
			.stub(vscode.workspace, "applyEdit")
			.resolves(true);
		const info = sandbox.stub(vscode.window, "showInformationMessage");
		try {
			await vscode.commands.executeCommand("cwtools.formatWorkspace");
		} finally {
			sandbox.restore();
		}

		assert.ok(
			applyEdit.calledOnce,
			`expected exactly one workspace/applyEdit request, got ${applyEdit.callCount}`,
		);
		const edit = applyEdit.firstCall.args[0];
		const targets = edit.entries().map(([uri]) => uri.toString());
		assert.ok(
			targets.includes(document.uri.toString()),
			`the workspace edit should cover ${document.uri.toString()}, got: ${targets.join(", ")}`,
		);
		const messages = info.getCalls().map((call) => String(call.args[0]));
		assert.ok(
			messages.some((message) => /^CWTools: Formatted \d+ file/.test(message)),
			`expected a Formatted toast, got: ${messages.join(" | ")}`,
		);
	});
});
