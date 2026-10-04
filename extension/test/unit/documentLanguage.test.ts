import * as assert from "assert";
import { afterEach, beforeEach, suite, test, vi } from "vitest";
import type { ExtensionContext } from "vscode";
import type { LanguageClient } from "vscode-languageclient/node";

const {
	activeEditor,
	disposable,
	executeCommand,
	logError,
	logInfo,
	onDidChangeActiveTextEditor,
} = vi.hoisted(() => ({
	activeEditor: {
		document: {
			languageId: "paradox",
			uri: {
				toString: () => "file:///workspace/events/focus.txt",
			},
		},
	},
	disposable: { dispose: () => {} },
	executeCommand: vi.fn().mockResolvedValue(undefined),
	logError: vi.fn(),
	logInfo: vi.fn(),
	onDidChangeActiveTextEditor: vi.fn((_listener: unknown) => disposable),
}));

vi.mock("vscode", () => ({
	CancellationTokenSource: class {
		private readonly cancellationListeners: Array<() => void> = [];
		token = {
			isCancellationRequested: false,
			onCancellationRequested: (listener: () => void) => {
				this.cancellationListeners.push(listener);
				return { dispose: () => {} };
			},
		};

		cancel(): void {
			this.token.isCancellationRequested = true;
			for (const listener of this.cancellationListeners) listener();
		}

		dispose(): void {}
	},
	commands: {
		executeCommand,
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
	logInfo,
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

	test("clears the latest type when getFileTypes returns no type", async () => {
		const sendNotification = vi.fn().mockResolvedValue(undefined);
		const sendRequest = vi
			.fn()
			.mockResolvedValueOnce(["idea"])
			.mockResolvedValueOnce([]);
		const tracker = await registerDocumentLanguage(
			{ subscriptions: [] } as unknown as ExtensionContext,
			{ sendNotification, sendRequest } as unknown as LanguageClient,
			"paradox",
		);

		await tracker.classifyActiveEditor();
		assert.strictEqual(tracker.getLatestType(), "idea");
		await tracker.classifyActiveEditor();

		assert.strictEqual(tracker.getLatestType(), "");
		assert.deepStrictEqual(executeCommand.mock.calls, [
			["setContext", "cwtoolsGraphFile", true],
			["setContext", "cwtoolsGraphFile", false],
		]);
	});

	test("clears the latest type when getFileTypes fails", async () => {
		const failure = new Error("server down");
		const sendNotification = vi.fn().mockResolvedValue(undefined);
		const sendRequest = vi
			.fn()
			.mockResolvedValueOnce(["idea"])
			.mockRejectedValueOnce(failure);
		const tracker = await registerDocumentLanguage(
			{ subscriptions: [] } as unknown as ExtensionContext,
			{ sendNotification, sendRequest } as unknown as LanguageClient,
			"paradox",
		);

		await tracker.classifyActiveEditor();
		assert.strictEqual(tracker.getLatestType(), "idea");
		await tracker.classifyActiveEditor();

		assert.strictEqual(tracker.getLatestType(), "");
		assert.deepStrictEqual(executeCommand.mock.calls, [
			["setContext", "cwtoolsGraphFile", true],
			["setContext", "cwtoolsGraphFile", false],
		]);
		assert.deepStrictEqual(logError.mock.calls, [
			["didChangeActiveTextEditor getFileTypes failed", failure],
		]);
	});

	test("clears the latest type during the active editor debounce", async () => {
		vi.useFakeTimers();
		try {
			const sendNotification = vi.fn().mockResolvedValue(undefined);
			const sendRequest = vi.fn().mockResolvedValue(["idea"]);
			const tracker = await registerDocumentLanguage(
				{ subscriptions: [] } as unknown as ExtensionContext,
				{ sendNotification, sendRequest } as unknown as LanguageClient,
				"paradox",
			);
			await tracker.classifyActiveEditor();
			assert.strictEqual(tracker.getLatestType(), "idea");

			const listener = onDidChangeActiveTextEditor.mock.calls[0]?.[0] as
				| ((editor: typeof activeEditor) => void)
				| undefined;
			assert.ok(listener);
			listener(activeEditor);

			assert.strictEqual(tracker.getLatestType(), "");
			await vi.advanceTimersByTimeAsync(200);
		} finally {
			vi.useRealTimers();
		}
	});

	test("clears the cached type before coalescing an in-flight request", async () => {
		const requestResolvers: Array<(data: string[]) => void> = [];
		const sendNotification = vi.fn().mockResolvedValue(undefined);
		const sendRequest = vi
			.fn()
			.mockResolvedValueOnce(["idea"])
			.mockImplementation(
				() =>
					new Promise<string[]>((resolve) => {
						requestResolvers.push(resolve);
					}),
			);
		const tracker = await registerDocumentLanguage(
			{ subscriptions: [] } as unknown as ExtensionContext,
			{ sendNotification, sendRequest } as unknown as LanguageClient,
			"paradox",
		);

		await tracker.classifyActiveEditor();
		assert.strictEqual(tracker.getLatestType(), "idea");
		const first = tracker.classifyActiveEditor();
		await vi.waitFor(() => assert.strictEqual(requestResolvers.length, 1));
		assert.strictEqual(tracker.getLatestType(), "");

		const second = tracker.classifyActiveEditor();
		assert.strictEqual(tracker.getLatestType(), "");
		requestResolvers.shift()!([]);
		await vi.waitFor(() => assert.strictEqual(requestResolvers.length, 1));
		requestResolvers.shift()!([]);
		await Promise.all([first, second]);
	});

	test("cancels and backs off a timed-out getFileTypes request", async () => {
		vi.useFakeTimers();
		try {
			const sendNotification = vi.fn().mockResolvedValue(undefined);
			const sendRequest = vi.fn(
				(
					_type: unknown,
					_params: unknown,
					token: {
						onCancellationRequested: (listener: () => void) => unknown;
					},
				) =>
					new Promise<string[]>((_resolve, reject) => {
						token.onCancellationRequested(() => reject(new Error("cancelled")));
					}),
			);
			const tracker = await registerDocumentLanguage(
				{ subscriptions: [] } as unknown as ExtensionContext,
				{ sendNotification, sendRequest } as unknown as LanguageClient,
				"paradox",
			);

			const classification = tracker.classifyActiveEditor();
			await vi.advanceTimersByTimeAsync(5000);
			await vi.advanceTimersByTimeAsync(2000);
			await classification;

			assert.strictEqual(tracker.getLatestType(), "");
			assert.deepStrictEqual(executeCommand.mock.calls, [
				["setContext", "cwtoolsGraphFile", false],
			]);
			assert.deepStrictEqual(logInfo.mock.calls, [
				["didChangeActiveTextEditor getFileTypes timed out after 5000ms"],
			]);
			assert.deepStrictEqual(logError.mock.calls, []);
		} finally {
			vi.useRealTimers();
		}
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

	suite("stale editor classification", () => {
		interface Editor {
			document: { languageId: string; uri: { toString(): string } };
		}
		const editorFor = (name: string): Editor => ({
			document: {
				languageId: "paradox",
				uri: { toString: () => `file:///workspace/events/${name}.txt` },
			},
		});
		const editorA = editorFor("a");
		const editorB = editorFor("b");
		const flush = () => vi.advanceTimersByTimeAsync(0);
		const graphFileValues = (): unknown[] => {
			const calls: unknown[][] = executeCommand.mock.calls;
			return calls
				.filter(([id, key]) => id === "setContext" && key === "cwtoolsGraphFile")
				.map((call) => call[2]);
		};
		const lastGraphFile = (): unknown => {
			const values = graphFileValues();
			return values[values.length - 1];
		};

		async function register() {
			const subscriptions: Array<{ dispose(): void }> = [];
			const requests: Array<{
				uri: string;
				resolve: (types: string[]) => void;
			}> = [];
			const sendNotification = vi.fn().mockResolvedValue(undefined);
			const sendRequest = vi.fn(
				(_type: unknown, params: { arguments: string[] }) =>
					new Promise<string[]>((resolve) => {
						requests.push({ uri: params.arguments[0], resolve });
					}),
			);
			const tracker = await registerDocumentLanguage(
				{ subscriptions } as unknown as ExtensionContext,
				{ sendNotification, sendRequest } as unknown as LanguageClient,
				"paradox",
			);
			const listener = onDidChangeActiveTextEditor.mock.calls[0]?.[0] as
				| ((editor: Editor | undefined) => void)
				| undefined;
			assert.ok(listener);
			return { tracker, subscriptions, requests, sendNotification, listener };
		}

		beforeEach(() => {
			vi.useFakeTimers();
		});
		afterEach(() => {
			vi.useRealTimers();
		});

		test("applies the reply for the only focused editor", async () => {
			const { tracker, requests, listener } = await register();

			listener(editorA);
			await vi.advanceTimersByTimeAsync(200);
			requests[0].resolve(["focus"]);
			await flush();

			assert.strictEqual(tracker.getLatestType(), "focus");
			assert.strictEqual(lastGraphFile(), true);
		});

		test("drops a late reply after the active editor goes away", async () => {
			const { tracker, requests, listener } = await register();

			listener(editorA);
			await vi.advanceTimersByTimeAsync(200);
			assert.strictEqual(requests.length, 1);
			listener(undefined);
			await vi.advanceTimersByTimeAsync(200);
			requests[0].resolve(["focus"]);
			await flush();

			assert.strictEqual(tracker.getLatestType(), "");
			assert.strictEqual(lastGraphFile(), false);
		});

		test("drops a late reply for the previous editor and applies the new one", async () => {
			const { tracker, requests, listener } = await register();

			listener(editorA);
			await vi.advanceTimersByTimeAsync(200);
			listener(editorB);
			await vi.advanceTimersByTimeAsync(200);
			requests[0].resolve(["focus"]);
			await flush();

			assert.strictEqual(tracker.getLatestType(), "");
			assert.strictEqual(lastGraphFile(), false);
			assert.deepStrictEqual(
				requests.map((request) => request.uri),
				["file:///workspace/events/a.txt", "file:///workspace/events/b.txt"],
			);

			requests[1].resolve(["idea"]);
			await flush();

			assert.strictEqual(tracker.getLatestType(), "idea");
			assert.strictEqual(lastGraphFile(), true);
		});

		test("never classifies a queued editor once nothing is focused", async () => {
			const { tracker, requests, listener } = await register();

			listener(editorA);
			await vi.advanceTimersByTimeAsync(200);
			listener(editorB);
			await vi.advanceTimersByTimeAsync(200);
			listener(undefined);
			await vi.advanceTimersByTimeAsync(200);
			requests[0].resolve(["focus"]);
			await flush();

			assert.strictEqual(requests.length, 1);
			assert.strictEqual(tracker.getLatestType(), "");
			assert.strictEqual(lastGraphFile(), false);
		});

		test("writes nothing after teardown", async () => {
			const { tracker, subscriptions, requests, sendNotification, listener } =
				await register();

			listener(editorA);
			await vi.advanceTimersByTimeAsync(200);
			listener(editorB);
			for (const subscription of subscriptions) subscription.dispose();
			requests[0].resolve(["focus"]);
			await vi.advanceTimersByTimeAsync(1000);

			assert.strictEqual(tracker.getLatestType(), "");
			assert.deepStrictEqual(sendNotification.mock.calls, [
				["didFocusFile", { uri: "file:///workspace/events/a.txt" }],
			]);
			assert.strictEqual(requests.length, 1);
			assert.ok(!graphFileValues().includes(true));
			assert.strictEqual(vi.getTimerCount(), 0);
		});
	});
});
