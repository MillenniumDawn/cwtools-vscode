import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";
import * as sinon from "sinon";
import * as vscode from "vscode";
import {
	activate,
	openDocumentAndShow,
	SAMPLE_ROOT,
	waitForLanguageServer,
	waitForServerReady,
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

function requiredServerCommands(): string[] {
	// Read the test-safe JSON contract as data; importing the extension module
	// here would load a second copy beside the running esbuild bundle.
	const contractPath = path.resolve(
		__dirname,
		"../../../../../../extension/src/common/serverCommands.json",
	);
	const contract: unknown = JSON.parse(fs.readFileSync(contractPath, "utf8"));
	assert.ok(
		typeof contract === "object" && contract !== null && !Array.isArray(contract),
		"server executeCommand contract must be a JSON object",
	);
	return Object.keys(contract);
}

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

	test("running server advertises every client-required executeCommand", async function () {
		const api = await activate();
		assert.ok(api, "activation API should be exposed");
		const required = requiredServerCommands();
		const advertised = api.serverCommands();
		const missing = required.filter((command) => !advertised.includes(command));
		assert.deepStrictEqual(
			missing,
			[],
			`server is missing required executeCommand names: ${missing.join(", ")}; advertised: ${advertised.join(", ")}`,
		);
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

const messagesOf = (stub: sinon.SinonStub): string[] =>
	stub.getCalls().map((call) => String(call.args[0]));

suite("lspClient user-facing results", function () {
	this.timeout(120000);

	let sandbox: sinon.SinonSandbox;

	suiteSetup(async function () {
		const api = await activate();
		assert.ok(api, "activation API should be exposed");
		assert.ok(
			await waitForServerReady(api),
			`server never finished its initial scan, last status: ${api.serverStatusText()}`,
		);
	});

	setup(function () {
		sandbox = sinon.createSandbox();
	});

	teardown(function () {
		sandbox.restore();
	});

	// A stuck scan or a cancelled request is the only way the real server
	// returns the odd shapes below, so they are fed in at the progress call that
	// carries its reply; the client code that reads the reply is the real one.
	function serverReplies(title: string, result: unknown): sinon.SinonStub {
		const withProgress = sandbox
			.stub(vscode.window, "withProgress")
			.callThrough();
		const reply = withProgress.withArgs(sinon.match.has("title", title));
		reply.resolves(result);
		return reply;
	}

	test("validateWorkspace shows the diagnostic summary", async function () {
		const info = sandbox
			.stub(vscode.window, "showInformationMessage")
			.resolves(undefined);
		await vscode.commands.executeCommand("validateWorkspace");
		const summary = info
			.getCalls()
			.find((call) => /^CWTools: validated \d+ of \d+ files;/.test(String(call.args[0])));
		assert.ok(
			summary,
			`expected a validation summary, got: ${messagesOf(info).join(" | ")}`,
		);
		const counts = /^CWTools: validated (\d+) of (\d+) files; \d+ with errors, \d+ errors, \d+ warnings, \d+ infos, \d+ hints\./.exec(
			String(summary.args[0]),
		);
		assert.ok(counts, `summary wording changed: ${String(summary.args[0])}`);
		const [validated, total] = counts.slice(1).map(Number);
		assert.ok(validated > 0 && validated <= total, "summary should count the scanned files");
		assert.strictEqual(summary.args[1], "Show Problems");
	});

	test("validateWorkspace puts each count in its place without a held-back caveat",
		async function () {
			const info = sandbox
				.stub(vscode.window, "showInformationMessage")
				.resolves(undefined);
			serverReplies("CWTools: Validate workspace", {
				totalFiles: 20,
				validatedFiles: 18,
				filesWithErrors: 3,
				totalErrors: 7,
				totalWarnings: 5,
				totalInfos: 4,
				totalHints: 1,
			});
			await vscode.commands.executeCommand("validateWorkspace");
			const summary = messagesOf(info).find((message) =>
				/^CWTools: validated 18 of 20 files;/.test(message),
			);
			assert.ok(
				summary,
				`got: ${messagesOf(info).join(" | ")}`,
			);
			assert.ok(
				messagesOf(info).includes(
						"CWTools: validated 18 of 20 files; 3 with errors, 7 errors, 5 warnings, 4 infos, 1 hints.",
				),
				`got: ${messagesOf(info).join(" | ")}`,
			);
			assert.ok(
				!messagesOf(info).some((message) => message.includes("leaves out")),
				`#1070 removed the closed-file budget; no held-back caveat may remain: ${messagesOf(info).join(" | ")}`,
			);
		});

	test("genlocall opens each non-empty stub as an untitled document with a BOM", async function () {
		const english = 'l_english:\n irm_one:0 "TODO"\n';
		const french = 'l_french:\n irm_one:0 "TODO"\n';
		serverReplies("CWTools: Generate missing loc for all files", [
			{ language: "english", content: english },
			{ language: "german", content: "" },
			{ language: "spanish" },
			{ language: "french", content: french },
		]);
		const opened = sandbox.spy(vscode.workspace, "openTextDocument");
		const shown = sandbox.spy(vscode.window, "showTextDocument");
		await vscode.commands.executeCommand("genlocall");
		const documents = shown
			.getCalls()
			.map((call) => call.args[0] as unknown as vscode.TextDocument);
		try {
			// VS Code folds the leading U+FEFF into the document's BOM flag, so the
			// document text no longer shows it; the request is where it is visible.
			assert.deepStrictEqual(
				opened.getCalls().map((call) => call.args[0]),
				[
					{ content: `\uFEFF${english}`, language: "paradox-localisation" },
					{ content: `\uFEFF${french}`, language: "paradox-localisation" },
				],
			);
			assert.deepStrictEqual(
				documents.map((document) => [document.isUntitled, document.getText()]),
				[
					[true, english],
					[true, french],
				],
			);
			assert.deepStrictEqual(
				shown.getCalls().map((call) => call.args[1]),
				[{ preview: false }, { preview: false }],
			);
		} finally {
			for (const document of documents) {
				await vscode.window.showTextDocument(document);
				await vscode.commands.executeCommand(
					"workbench.action.revertAndCloseActiveEditor",
				);
			}
		}
	});

	const nothingMissing: Array<[name: string, reply: unknown]> = [
		["an empty list", []],
		["stubs without content", [{ content: "" }, {}]],
	];
	for (const [name, reply] of nothingMissing) {
		test(`genlocall says so when nothing is missing: ${name}`, async function () {
			const info = sandbox
				.stub(vscode.window, "showInformationMessage")
				.resolves(undefined);
			const shown = sandbox.spy(vscode.window, "showTextDocument");
			serverReplies("CWTools: Generate missing loc for all files", reply);
			await vscode.commands.executeCommand("genlocall");
			assert.ok(
				messagesOf(info).includes("CWTools: no missing localisation found."),
				`got: ${messagesOf(info).join(" | ")}`,
			);
			assert.strictEqual(shown.callCount, 0, "no document should open");
		});
	}

	const validateReplies: Array<
		[name: string, reply: unknown, channel: "info" | "warning", text: string]
	> = [
		["a busy server", { busy: true }, "warning", "another scan is still running"],
		["a cancelled run", { cancelled: true }, "info", "validateWorkspace cancelled."],
		[
			"a reply without a summary",
			{ message: "workspace validation did not complete" },
			"warning",
			"did not return a summary",
		],
		[
			"a negative count",
			{
				totalFiles: 3,
				validatedFiles: 3,
				filesWithErrors: 0,
				totalErrors: -1,
				totalWarnings: 0,
				totalInfos: 0,
				totalHints: 0,
			},
			"warning",
			"did not return a summary",
		],
		["a non-object reply", "done", "warning", "did not return a summary"],
	];
	for (const [name, reply, channel, text] of validateReplies) {
		test(`validateWorkspace says so on ${name}`, async function () {
			const info = sandbox
				.stub(vscode.window, "showInformationMessage")
				.resolves(undefined);
			const warning = sandbox
				.stub(vscode.window, "showWarningMessage")
				.resolves(undefined);
			const served = serverReplies("CWTools: Validate workspace", reply);
			await vscode.commands.executeCommand("validateWorkspace");
			assert.ok(served.calledOnce, "the command should have asked the server once");
			const [shown, other] = channel === "info" ? [info, warning] : [warning, info];
			assert.ok(
				messagesOf(shown).some((message) => message.includes(text)),
				`expected ${channel} "${text}", got: ${messagesOf(shown).join(" | ")}`,
			);
			assert.ok(
				!messagesOf(other).some((message) => message.includes(text)),
				`"${text}" should not be shown as the other severity`,
			);
			assert.ok(
				!messagesOf(info).some((message) => message.startsWith("CWTools: validated")),
				"no summary should be shown",
			);
		});
	}

	test("a reload-only settings change prompts once and reloads the window when accepted", async function () {
		const setting = "inlayHints.scopes";
		const config = vscode.workspace.getConfiguration("cwtools");
		const previous = config.inspect(setting)?.globalValue;
		const prompt = sandbox
			.stub(vscode.window, "showInformationMessage")
			.resolves("Reload Window" as never);
		const prompts = () =>
			messagesOf(prompt).filter((message) => message.startsWith("CWTools settings changed."));
		const reload = sandbox
			.stub(vscode.commands, "executeCommand")
			.callThrough()
			.withArgs("workbench.action.reloadWindow")
			.resolves(undefined);
		try {
			await config.update(setting, !previous, vscode.ConfigurationTarget.Global);
			assert.ok(await waitUntil(() => reload.called), "accepting the prompt should reload the window");
			assert.strictEqual(prompts().length, 1);
			assert.strictEqual(reload.callCount, 1);
		} finally {
			prompt.resolves(undefined);
			await config.update(setting, previous, vscode.ConfigurationTarget.Global);
		}
		assert.ok(await waitUntil(() => prompts().length >= 2), "restoring the setting should prompt again");
		assert.strictEqual(reload.callCount, 1, "declining the prompt must not reload");
	});
});
