import { afterEach, beforeEach, suite, test, vi } from "vitest";
import * as assert from "assert";
import type { WebviewPanel } from "vscode";

// The goToFile trust gate has no host coverage: a real webview only posts
// locations the server produced, so the refusal branches never run there, and
// a host test could not click through the modal prompt anyway. A fake panel
// playing webview messages makes every branch deterministic.

const {
	executeCommand,
	registerCommand,
	showTextDocument,
	showWarningMessage,
	revealRange,
	Range,
	textEditorRevealType,
	logWarn,
	roots,
} = vi.hoisted(() => ({
	executeCommand: vi.fn(),
	registerCommand: vi.fn(() => ({ dispose: () => {} })),
	showTextDocument: vi.fn(),
	showWarningMessage: vi.fn(),
	revealRange: vi.fn(),
	Range: vi.fn(function (
		startLine: number,
		startCharacter: number,
		endLine: number,
		endCharacter: number,
	) {
		return { startLine, startCharacter, endLine, endCharacter };
	}),
	textEditorRevealType: {
		Default: 0,
		InCenter: 1,
		InCenterIfOutsideViewport: 2,
		AtTop: 3,
	},
	logWarn: vi.fn(),
	roots: { folders: [] as { uri: { fsPath: string } }[] },
}));

vi.mock("vscode", async (importOriginal) => ({
	...(await importOriginal<object>()),
	commands: { executeCommand, registerCommand },
	Uri: {
		file: (p: string) => ({
			scheme: "file",
			fsPath: p,
			toString: () => `file://${p}`,
		}),
	},
	ViewColumn: { One: 1 },
	Range,
	TextEditorRevealType: textEditorRevealType,
	window: {
		activeTextEditor: undefined,
		showInformationMessage: vi.fn(),
		showSaveDialog: vi.fn(),
		showTextDocument,
		showWarningMessage,
	},
	workspace: {
		fs: { readFile: vi.fn() },
		getConfiguration: vi.fn(() => ({ get: () => 1 })),
		get workspaceFolders() {
			return roots.folders;
		},
	},
}));

vi.mock("../../src/host/logger", () => ({
	logError: vi.fn(),
	logInfo: vi.fn(),
	logWarn,
	errorMessage: (err: unknown) => (err instanceof Error ? err.message : ""),
}));

import { GraphPanel } from "../../src/host/graphPanel";

suite("graph panel goToFile", () => {
	const workspaceFile = "/roots/mod/events/a.txt";
	const outsideFile = "/etc/passwd";
	let fake: ReturnType<typeof fakePanel>;

	// Enough of the WebviewPanel surface for the GraphPanel constructor, with
	// a way to play webview messages straight into the real handler. The
	// handler is async, so awaiting the send waits for the whole branch.
	function fakePanel() {
		let messageListener: ((message: unknown) => unknown) | undefined;
		const panel = {
			webview: {
				html: "",
				cspSource: "https://test.webview",
				asWebviewUri: () => ({
					toString: () => "https://test.webview/graph.js",
				}),
				postMessage: vi.fn(),
				onDidReceiveMessage: (listener: (message: unknown) => unknown) => {
					messageListener = listener;
					return { dispose: () => {} };
				},
			},
			onDidDispose: () => ({ dispose: () => {} }),
			onDidChangeViewState: () => ({ dispose: () => {} }),
			dispose: vi.fn(),
		};
		return {
			panel: panel as unknown as WebviewPanel,
			send: (message: unknown) => messageListener?.(message),
		};
	}

	const send = (uri: string, line: number, column: number) =>
		fake.send({ command: "goToFile", uri, line, column });

	const openedUri = () =>
		showTextDocument.mock.calls[0][0] as { fsPath: string; scheme: string };

	beforeEach(() => {
		vi.clearAllMocks();
		roots.folders = [{ uri: { fsPath: "/roots/mod" } }];
		showTextDocument.mockResolvedValue({ revealRange });
		showWarningMessage.mockResolvedValue(undefined);
		fake = fakePanel();
		GraphPanel.restore("/ext", fake.panel);
	});

	afterEach(() => {
		GraphPanel.currentPanel?.dispose();
	});

	test("refuses a relative path without opening anything", async () => {
		await send("mod/events/a.txt", 1, 1);

		// The handler's own gate, not confirmOpen's: the raw path, no scheme.
		assert.deepStrictEqual(logWarn.mock.calls, [
			["Refusing to open mod/events/a.txt: not an absolute file path."],
		]);
		assert.strictEqual(showWarningMessage.mock.calls.length, 0);
		assert.strictEqual(showTextDocument.mock.calls.length, 0);
	});

	test("does not open an outside-root path when the prompt is declined", async () => {
		await send(outsideFile, 1, 1);

		assert.strictEqual(showWarningMessage.mock.calls.length, 1);
		assert.ok(
			String(showWarningMessage.mock.calls[0][0]).includes(outsideFile),
			"the prompt should name the path",
		);
		assert.strictEqual(
			(showWarningMessage.mock.calls[0][1] as { modal?: boolean }).modal,
			true,
		);
		assert.strictEqual(showTextDocument.mock.calls.length, 0);
	});

	test("opens an outside-root path once the user confirms", async () => {
		showWarningMessage.mockResolvedValue("Open");

		await send(outsideFile, 1, 1);

		assert.strictEqual(showWarningMessage.mock.calls.length, 1);
		assert.strictEqual(showTextDocument.mock.calls.length, 1);
		assert.strictEqual(openedUri().fsPath, outsideFile);
		assert.strictEqual(openedUri().scheme, "file");
	});

	test("opens a workspace file at the exact uri without prompting", async () => {
		await send(workspaceFile, 5, 3);

		assert.strictEqual(showWarningMessage.mock.calls.length, 0);
		assert.strictEqual(showTextDocument.mock.calls.length, 1);
		assert.strictEqual(openedUri().fsPath, workspaceFile);
		assert.strictEqual(openedUri().scheme, "file");
	});

	test("converts 1-based positions to a 0-based reveal range at top", async () => {
		await send(workspaceFile, 5, 3);

		assert.deepStrictEqual(Range.mock.calls, [[4, 2, 4, 2]]);
		assert.strictEqual(revealRange.mock.calls.length, 1);
		assert.strictEqual(
			revealRange.mock.calls[0][0],
			Range.mock.results[0]?.value,
		);
		assert.strictEqual(
			revealRange.mock.calls[0][1],
			textEditorRevealType.AtTop,
		);
	});

	test("clamps a defensive zero position instead of going negative", async () => {
		await send(workspaceFile, 0, 0);

		assert.deepStrictEqual(Range.mock.calls, [[0, 0, 0, 0]]);
		assert.strictEqual(showTextDocument.mock.calls.length, 1);
		assert.strictEqual(revealRange.mock.calls.length, 1);
		assert.strictEqual(
			revealRange.mock.calls[0][0],
			Range.mock.results[0]?.value,
		);
	});
});
