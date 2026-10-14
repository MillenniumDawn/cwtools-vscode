import * as assert from "assert";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import * as vscode from "vscode";
import sinon from "sinon";
import {
	activate,
	graphPanelModule,
	EXTENSION_ID,
	waitUntil,
} from "../support/utils";
import type * as GraphPanelNamespace from "../../src/host/graphPanel";

type GraphPanelModule = typeof GraphPanelNamespace;
type ReceiveMessage = Parameters<
	vscode.Webview["onDidReceiveMessage"]
>[0];

suite("Graph navigation trust", function () {
	this.timeout(60_000);
	let gp: GraphPanelModule | undefined;
	let trustedDir: string | undefined;
	let outsideDir: string | undefined;
	let panel: GraphPanelNamespace.GraphPanel | undefined;
	let sandbox: sinon.SinonSandbox;
	let receive: ReceiveMessage | undefined;

	setup(async () => {
		sandbox = sinon.createSandbox();
		await activate();
		gp = await graphPanelModule();
		const extension = vscode.extensions.getExtension(EXTENSION_ID);
		assert.ok(extension, "extension should be installed in the host");
		const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
		assert.ok(workspaceFolder, "sample workspace should be open");
		trustedDir = fs.mkdtempSync(
			path.join(workspaceFolder.uri.fsPath, "graph-trust-"),
		);
		outsideDir = fs.mkdtempSync(path.join(os.tmpdir(), "cwtools-graph-outside-"));
		gp.GraphPanel.currentPanel?.dispose();
		const createWebviewPanel = vscode.window.createWebviewPanel.bind(vscode.window);
		sandbox.stub(vscode.window, "createWebviewPanel").callsFake((...args) => {
			const created = createWebviewPanel(...args);
			const webview = created.webview;
			const onDidReceiveMessage = webview.onDidReceiveMessage.bind(webview);
			sandbox
				.stub(webview, "onDidReceiveMessage")
				.callsFake((listener, thisArgs, disposables) => {
					receive = listener;
					return onDidReceiveMessage(listener, thisArgs, disposables);
				});
			return created;
		});
		gp.GraphPanel.create(extension.extensionPath);
		panel = gp.GraphPanel.currentPanel;
		assert.ok(panel, "a real GraphPanel should be created");
	});

	teardown(async () => {
		if (panel && gp) {
			await waitUntil(() => panel!.getState() !== gp!.State.New, 2_000);
			panel.dispose();
			panel = undefined;
		}
		const activePath = vscode.window.activeTextEditor?.document.uri.fsPath;
		if (
			activePath &&
			[trustedDir, outsideDir].some((directory) => {
				if (!directory) return false;
				const relative = path.relative(directory, activePath);
				return (
					relative === "" ||
					(relative !== ".." &&
						!relative.startsWith(`..${path.sep}`) &&
						!path.isAbsolute(relative))
				);
			})
		) {
			await vscode.commands.executeCommand("workbench.action.closeActiveEditor");
		}
		sandbox?.restore();
		if (trustedDir) fs.rmSync(trustedDir, { recursive: true, force: true });
		if (outsideDir) fs.rmSync(outsideDir, { recursive: true, force: true });
	});

	function fixture(directory: string, name: string): string {
		const file = path.join(directory, name);
		fs.writeFileSync(file, "first line\nsecond line\nthird line\n", "utf8");
		return file;
	}

	function trustedRoot(): string {
		assert.ok(trustedDir, "trusted fixture should be initialized");
		return trustedDir;
	}

	function outsideRoot(): string {
		assert.ok(outsideDir, "outside fixture should be initialized");
		return outsideDir;
	}

	function handler(): ReceiveMessage {
		assert.ok(receive, "GraphPanel should register its real webview handler");
		return receive;
	}

	async function send(uri: string, line: number, column: number): Promise<void> {
		await handler()({ command: "goToFile", uri, line, column });
	}

	function captureOpenedFile(): {
		show: sinon.SinonStub;
		reveal: sinon.SinonSpy<
			[range: vscode.Range, revealType?: vscode.TextEditorRevealType],
			void
		>;
	} {
		const original = vscode.window.showTextDocument.bind(vscode.window);
		let reveal:
			| sinon.SinonSpy<
					[range: vscode.Range, revealType?: vscode.TextEditorRevealType],
					void
			  >
			| undefined;
		const show = sandbox
			.stub(vscode.window, "showTextDocument")
			.callsFake(async (...args) => {
				const editor = await original(...args);
				reveal = sandbox.spy(editor, "revealRange");
				return editor;
			});
		return {
			show,
			get reveal() {
				assert.ok(reveal, "the opened editor should expose revealRange");
				return reveal;
			},
		};
	}

	test("opens a trusted workspace file without prompting and reveals the 1-based location", async function () {
		const file = fixture(trustedRoot(), "trusted.txt");
		const warning = sandbox.stub(vscode.window, "showWarningMessage").resolves(undefined);
		const opened = captureOpenedFile();

		await send(file, 2, 3);

		assert.strictEqual(warning.called, false, "trusted files must open without confirmation");
		assert.ok(await waitUntil(() => vscode.window.activeTextEditor?.document.uri.fsPath === file));
		assert.strictEqual(vscode.window.activeTextEditor?.document.getText(), "first line\nsecond line\nthird line\n");
		assert.strictEqual(opened.show.calledOnce, true);
		const range = opened.reveal.firstCall.args[0];
		assert.deepStrictEqual(range.start, new vscode.Position(1, 2));
		assert.strictEqual(opened.reveal.firstCall.args[1], vscode.TextEditorRevealType.AtTop);
	});

	test("opens an outside file only after the user allows it and clamps zero coordinates", async function () {
		const file = fixture(outsideRoot(), "allowed.txt");
		const warningCalls: Array<{
			message: string;
			options: vscode.MessageOptions;
			choices: string[];
		}> = [];
		const originalWarningDescriptor = Object.getOwnPropertyDescriptor(
			vscode.window,
			"showWarningMessage",
		);
		const allowWarning = (
			message: string,
			options: vscode.MessageOptions,
			...choices: string[]
		): Promise<string | undefined> => {
			warningCalls.push({ message, options, choices });
			return Promise.resolve("Open");
		};
		const opened = captureOpenedFile();
		Object.defineProperty(vscode.window, "showWarningMessage", {
			configurable: true,
			writable: true,
			value: allowWarning,
		});

		try {
			await send(file, 0, 0);
		} finally {
			if (originalWarningDescriptor) {
				Object.defineProperty(
					vscode.window,
					"showWarningMessage",
					originalWarningDescriptor,
				);
			} else {
				Reflect.deleteProperty(vscode.window, "showWarningMessage");
			}
		}

		assert.strictEqual(warningCalls.length, 1);
		assert.ok(warningCalls[0].message.includes(file));
		assert.deepStrictEqual(warningCalls[0].options, { modal: true });
		assert.deepStrictEqual(warningCalls[0].choices, ["Open"]);
		assert.ok(await waitUntil(() => vscode.window.activeTextEditor?.document.uri.fsPath === file));
		assert.strictEqual(vscode.window.activeTextEditor?.document.getText(), "first line\nsecond line\nthird line\n");
		assert.deepStrictEqual(opened.reveal.firstCall.args[0].start, new vscode.Position(0, 0));
	});

	test("keeps an outside file closed when the user denies confirmation", async function () {
		const file = fixture(outsideRoot(), "denied.txt");
		const warning = sandbox.stub(vscode.window, "showWarningMessage").resolves(undefined);
		const show = sandbox.spy(vscode.window, "showTextDocument");

		await send(file, 1, 1);

		assert.strictEqual(warning.calledOnce, true);
		assert.strictEqual(show.called, false);
	});

	test("refuses relative paths before prompting or opening", async function () {
		const warning = sandbox.stub(vscode.window, "showWarningMessage").resolves(undefined);
		const show = sandbox.spy(vscode.window, "showTextDocument");

		await send("relative/graph-target.txt", 1, 1);

		assert.strictEqual(warning.called, false);
		assert.strictEqual(show.called, false);
	});
});
