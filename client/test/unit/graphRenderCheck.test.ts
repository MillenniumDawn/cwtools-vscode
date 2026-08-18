import { afterEach, beforeEach, suite, test, vi } from "vitest";
import * as assert from "assert";
import type { WebviewPanel } from "vscode";

// The one-second no-reply answer #212 added for #210 has no host coverage: a
// real webview replies, so the host suite only ever walks the happy path. Fake
// timers against a fake panel make the other one deterministic.

const { executeCommand, registerCommand } = vi.hoisted(() => ({
	executeCommand: vi.fn(),
	registerCommand: vi.fn(() => ({ dispose: () => {} })),
}));

vi.mock("vscode", async (importOriginal) => ({
	...(await importOriginal<object>()),
	commands: { executeCommand, registerCommand },
	Uri: { file: (p: string) => ({ fsPath: p, toString: () => `file://${p}` }) },
	ViewColumn: { One: 1 },
	window: {
		activeTextEditor: undefined,
		showInformationMessage: vi.fn(),
		showSaveDialog: vi.fn(),
		showWarningMessage: vi.fn(),
	},
	workspace: {
		fs: { readFile: vi.fn() },
		getConfiguration: vi.fn(() => ({ get: () => 1 })),
	},
}));

vi.mock("../../extension/logger", () => ({
	logError: vi.fn(),
	logInfo: vi.fn(),
	logWarn: vi.fn(),
	errorMessage: (err: unknown) => (err instanceof Error ? err.message : ""),
}));

import { GraphPanel } from "../../extension/graphPanel";

suite("graph render check", () => {
	// Enough of the WebviewPanel surface for the GraphPanel constructor, with
	// postMessage recorded and a way to play the webview's reply back.
	function fakePanel() {
		let messageListener: ((message: unknown) => void) | undefined;
		const postMessage = vi.fn();
		const panel = {
			webview: {
				html: "",
				cspSource: "https://test.webview",
				asWebviewUri: () => ({
					toString: () => "https://test.webview/graph.js",
				}),
				postMessage,
				onDidReceiveMessage: (listener: (message: unknown) => void) => {
					messageListener = listener;
					return { dispose: () => {} };
				},
			},
			onDidDispose: () => ({ dispose: () => {} }),
			onDidChangeViewState: () => ({ dispose: () => {} }),
			dispose: vi.fn(),
		};
		// The real webview echoes the id of the check it is answering, so a reply
		// here has to name one too.
		const idOf = (call: number) =>
			(postMessage.mock.calls[call][0] as { id: number }).id;
		return {
			panel: panel as unknown as WebviewPanel,
			postMessage,
			reply: (rendered: boolean, call = postMessage.mock.calls.length - 1) =>
				messageListener?.({
					command: "cytoscapeRenderedResult",
					rendered,
					id: idOf(call),
				}),
		};
	}

	beforeEach(() => {
		vi.clearAllMocks();
		vi.useFakeTimers();
	});

	afterEach(() => {
		vi.useRealTimers();
		GraphPanel.currentPanel?.dispose();
	});

	test("resolves with the rendered flag the webview replies", async () => {
		const fake = fakePanel();
		const graph = GraphPanel.restore("/ext", fake.panel);

		const pending = graph.checkCytoscapeRendered();
		fake.reply(true);

		assert.strictEqual(await pending, true);
	});

	test("resolves false a second after a webview that never replies", async () => {
		const fake = fakePanel();
		const graph = GraphPanel.restore("/ext", fake.panel);

		const pending = graph.checkCytoscapeRendered();
		await vi.advanceTimersByTimeAsync(1_000);

		assert.strictEqual(await pending, false);
	});

	test("posts a fresh check per call so a poll loop can ask again", async () => {
		const fake = fakePanel();
		const graph = GraphPanel.restore("/ext", fake.panel);

		const first = graph.checkCytoscapeRendered();
		await vi.advanceTimersByTimeAsync(1_000);
		assert.strictEqual(await first, false);

		const second = graph.checkCytoscapeRendered();
		fake.reply(true);
		assert.strictEqual(await second, true);
		assert.strictEqual(fake.postMessage.mock.calls.length, 2);
	});

	test("ignores a reply the webview owed a check that already timed out", async () => {
		const fake = fakePanel();
		const graph = GraphPanel.restore("/ext", fake.panel);

		const first = graph.checkCytoscapeRendered();
		await vi.advanceTimersByTimeAsync(1_000);
		assert.strictEqual(await first, false);

		const second = graph.checkCytoscapeRendered();
		fake.reply(true, 0);
		await vi.advanceTimersByTimeAsync(1_000);

		assert.strictEqual(
			await second,
			false,
			"the late reply belongs to the first check, not this one",
		);
	});
});
