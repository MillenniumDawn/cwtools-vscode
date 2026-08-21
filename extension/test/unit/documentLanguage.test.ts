import * as assert from "assert";
import { beforeEach, suite, test, vi } from "vitest";
import type { ExtensionContext } from "vscode";
import type { LanguageClient } from "vscode-languageclient/node";

const { activeEditor, disposable, logError, onDidChangeActiveTextEditor } =
	vi.hoisted(() => ({
		activeEditor: {
			document: {
				languageId: "paradox",
				uri: {
					toString: () => "file:///workspace/events/focus.txt",
				},
			},
		},
		disposable: { dispose: () => {} },
		logError: vi.fn(),
		onDidChangeActiveTextEditor: vi.fn((_listener: unknown) => disposable),
	}));

vi.mock("vscode", () => ({
	CancellationTokenSource: class {
		token = { isCancellationRequested: false };

		cancel(): void {
			this.token.isCancellationRequested = true;
		}

		dispose(): void {}
	},
	commands: {
		executeCommand: vi.fn().mockResolvedValue(undefined),
	},
	languages: {
		setTextDocumentLanguage: vi.fn().mockResolvedValue(undefined),
	},
	window: {
		activeTextEditor: activeEditor,
		onDidChangeActiveTextEditor,
	},
	workspace: {
		textDocuments: [],
		onDidOpenTextDocument: vi.fn(() => disposable),
	},
}));

vi.mock("vscode-languageclient/node", () => ({
	ExecuteCommandRequest: { type: {} },
}));

vi.mock("../../src/host/logger", () => ({
	logError,
	logInfo: vi.fn(),
}));

import { registerDocumentLanguage } from "../../src/host/documentLanguage";

suite("documentLanguage", () => {
	beforeEach(() => {
		vi.clearAllMocks();
	});

	test("logs a rejected focus notification instead of propagating it", async () => {
		const failure = new Error("Client is not running");
		const sendNotification = vi.fn().mockRejectedValue(failure);
		const sendRequest = vi.fn();
		const tracker = await registerDocumentLanguage(
			{ subscriptions: [] } as unknown as ExtensionContext,
			{ sendNotification, sendRequest } as unknown as LanguageClient,
			"paradox",
		);

		await assert.doesNotReject(() => tracker.classifyActiveEditor());
		assert.deepStrictEqual(sendNotification.mock.calls, [
			["didFocusFile", { uri: "file:///workspace/events/focus.txt" }],
		]);
		assert.strictEqual(sendRequest.mock.calls.length, 0);
		assert.deepStrictEqual(logError.mock.calls, [
			["didChangeActiveTextEditor failed", failure],
		]);
	});

	test("contains a rejected notification from an editor change", async () => {
		vi.useFakeTimers();
		try {
			const failure = new Error("Client is not running");
			const sendNotification = vi.fn().mockRejectedValue(failure);
			await registerDocumentLanguage(
				{ subscriptions: [] } as unknown as ExtensionContext,
				{ sendNotification, sendRequest: vi.fn() } as unknown as LanguageClient,
				"paradox",
			);
			const listener = onDidChangeActiveTextEditor.mock.calls[0]?.[0] as
				| ((editor: typeof activeEditor) => void)
				| undefined;
			assert.ok(listener);

			listener(activeEditor);
			await vi.advanceTimersByTimeAsync(200);

			assert.deepStrictEqual(sendNotification.mock.calls, [
				["didFocusFile", { uri: "file:///workspace/events/focus.txt" }],
			]);
			assert.deepStrictEqual(logError.mock.calls, [
				["didChangeActiveTextEditor failed", failure],
			]);
		} finally {
			vi.useRealTimers();
		}
	});
});
