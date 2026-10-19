import * as assert from "assert";
import { beforeEach, suite, test, vi } from "vitest";
import type { ExtensionContext } from "vscode";
import type { LanguageClient } from "vscode-languageclient/node";
import type * as VscodeStub from "./_stubs/vscode";

// Replies are settled by hand: a healthy server never answers late, so the host labels cannot reach these branches.

const state = vi.hoisted(() => ({
	executeCommand: vi.fn(),
	logError: vi.fn(),
	switchEditor: undefined as ((editor: unknown) => void) | undefined,
}));

vi.mock("vscode", async (importOriginal) => {
	const original = await importOriginal<typeof VscodeStub>();
	return {
		...original,
		CancellationTokenSource: class {
			token = { isCancellationRequested: false };
			cancel(): void {
				this.token.isCancellationRequested = true;
			}
			dispose(): void {}
		},
		commands: { executeCommand: state.executeCommand },
		window: {
			...original.window,
			onDidChangeActiveTextEditor: (listener: (editor: unknown) => void) => {
				state.switchEditor = listener;
				return { dispose: () => {} };
			},
		},
		workspace: {
			...original.workspace,
			textDocuments: [],
			onDidOpenTextDocument: () => ({ dispose: () => {} }),
		},
	};
});

vi.mock("vscode-languageclient/node", () => ({
	ExecuteCommandRequest: { type: {} },
}));

vi.mock("../../src/host/logger", async () =>
	(await import("./support/loggerMock")).mockLogger({
		logError: state.logError,
	}),
);

import { registerDocumentLanguage } from "../../src/host/documentLanguage";

// These mirror the private constants in documentLanguage.ts.
const DEBOUNCE_MS = 200;
const TIMEOUT_MS = 5000;
const BACKOFF_MS = 2000;

interface Request {
	uri: string;
	token: { isCancellationRequested: boolean };
	resolve(types: string[]): void;
	reject(error: Error): void;
}

function fakeEditor(name: string) {
	return {
		document: {
			languageId: "paradox",
			uri: { toString: () => `file:///workspace/events/${name}.txt` },
		},
	};
}

async function startTracker() {
	const requests: Request[] = [];
	const client = {
		sendNotification: vi.fn().mockResolvedValue(undefined),
		sendRequest: vi.fn(
			(
				_type: unknown,
				params: { arguments: string[] },
				token: Request["token"],
			) =>
				new Promise<string[]>((resolve, reject) => {
					requests.push({ uri: params.arguments[0], token, resolve, reject });
				}),
		),
	};
	const context = { subscriptions: [] as Array<{ dispose(): void }> };
	const tracker = await registerDocumentLanguage(
		context as unknown as ExtensionContext,
		client as unknown as LanguageClient,
		"paradox",
		() => [],
	);
	return { tracker, requests, context };
}

const focus = async (editor: unknown) => {
	state.switchEditor!(editor);
	await vi.advanceTimersByTimeAsync(DEBOUNCE_MS);
};
const settle = () => vi.advanceTimersByTimeAsync(0);
const graphFileWrites = () =>
	state.executeCommand.mock.calls.map(([, , value]) => value as boolean);

suite("documentLanguage", () => {
	beforeEach(() => {
		vi.useFakeTimers();
	});

	test("a reply for the previous editor does not set the graph file context", async () => {
		const { tracker, requests } = await startTracker();
		const a = fakeEditor("a");
		const b = fakeEditor("b");

		await focus(a);
		await focus(b);
		assert.strictEqual(requests.length, 1);

		requests[0].resolve(["idea"]);
		await settle();

		assert.strictEqual(tracker.getLatestType(), "");
		assert.deepStrictEqual(graphFileWrites(), [false]);
		assert.strictEqual(requests.length, 2);
		assert.strictEqual(requests[1].uri, b.document.uri.toString());

		requests[1].resolve(["event"]);
		await settle();

		assert.strictEqual(tracker.getLatestType(), "event");
		assert.deepStrictEqual(graphFileWrites(), [false, true]);
		assert.strictEqual(state.logError.mock.calls.length, 0);
	});

	test("closing the last editor clears the type at once", async () => {
		const { tracker, requests } = await startTracker();

		await focus(fakeEditor("a"));
		requests[0].resolve(["idea"]);
		await settle();
		assert.strictEqual(tracker.getLatestType(), "idea");
		assert.deepStrictEqual(graphFileWrites(), [false, true]);

		state.switchEditor!(undefined);
		assert.strictEqual(tracker.getLatestType(), "");
		assert.deepStrictEqual(graphFileWrites(), [false, true, false]);

		await vi.advanceTimersByTimeAsync(DEBOUNCE_MS);
		assert.strictEqual(requests.length, 1);
		assert.deepStrictEqual(graphFileWrites(), [false, true, false]);
		assert.strictEqual(state.logError.mock.calls.length, 0);
	});

	test("a timed-out getFileTypes is cancelled and the next request backs off", async () => {
		const { requests } = await startTracker();
		const b = fakeEditor("b");

		await focus(fakeEditor("a"));
		await vi.advanceTimersByTimeAsync(TIMEOUT_MS - 1);
		assert.strictEqual(requests[0].token.isCancellationRequested, false);
		await vi.advanceTimersByTimeAsync(1);
		assert.strictEqual(requests[0].token.isCancellationRequested, true);

		requests[0].reject(new Error("request cancelled"));
		await settle();
		await focus(b);
		await vi.advanceTimersByTimeAsync(BACKOFF_MS - DEBOUNCE_MS - 1);
		assert.strictEqual(requests.length, 1);

		await vi.advanceTimersByTimeAsync(1);
		assert.strictEqual(requests.length, 2);
		assert.strictEqual(requests[1].uri, b.document.uri.toString());
		assert.strictEqual(state.logError.mock.calls.length, 0);
	});

	test("coalesces multiple editor switches during the timeout cooldown", async () => {
		const { requests } = await startTracker();

		state.switchEditor!(fakeEditor("initial"));
		await vi.advanceTimersByTimeAsync(DEBOUNCE_MS);
		assert.strictEqual(requests.length, 1);
		await vi.advanceTimersByTimeAsync(TIMEOUT_MS);
		assert.strictEqual(requests[0].token.isCancellationRequested, true);
		requests[0].reject(new Error("request cancelled"));
		await settle();

		state.switchEditor!(fakeEditor("superseded"));
		await vi.advanceTimersByTimeAsync(DEBOUNCE_MS);
		state.switchEditor!(fakeEditor("latest"));
		await vi.advanceTimersByTimeAsync(DEBOUNCE_MS);
		assert.strictEqual(requests.length, 1, "cooldown should hold later requests");

		await vi.advanceTimersByTimeAsync(BACKOFF_MS - 2 * DEBOUNCE_MS);
		assert.strictEqual(requests.length, 2);
		assert.strictEqual(
			requests[1].uri,
			"file:///workspace/events/latest.txt",
			"only the latest editor during cooldown should be classified",
		);
		requests[1].resolve(["event"]);
		await settle();
	});

	test("disposing before the first debounce prevents the classification request", async () => {
		const { requests, context } = await startTracker();

		state.switchEditor!(fakeEditor("before-debounce"));
		for (const subscription of context.subscriptions) subscription.dispose();
		await vi.advanceTimersByTimeAsync(DEBOUNCE_MS);

		assert.strictEqual(requests.length, 0);
	});

	test("disposing before a timed-out request rejects prevents a new backoff", async () => {
		const { requests, context } = await startTracker();

		await focus(fakeEditor("initial"));
		await vi.advanceTimersByTimeAsync(TIMEOUT_MS);
		assert.strictEqual(requests[0].token.isCancellationRequested, true);

		for (const subscription of context.subscriptions) subscription.dispose();
		requests[0].reject(new Error("request cancelled"));
		await settle();

		assert.strictEqual(vi.getTimerCount(), 0);
		await vi.advanceTimersByTimeAsync(BACKOFF_MS);
		assert.strictEqual(requests.length, 1);
		assert.strictEqual(state.logError.mock.calls.length, 0);
	});

	test("disposing during timeout backoff clears its timer and queued switch", async () => {
		const { requests, context } = await startTracker();

		await focus(fakeEditor("initial"));
		await vi.advanceTimersByTimeAsync(TIMEOUT_MS);
		requests[0].reject(new Error("request cancelled"));
		await settle();

		state.switchEditor!(fakeEditor("queued"));
		await vi.advanceTimersByTimeAsync(DEBOUNCE_MS);
		assert.strictEqual(vi.getTimerCount(), 1, "the backoff timer is pending");

		for (const subscription of context.subscriptions) subscription.dispose();
		await settle();
		assert.strictEqual(vi.getTimerCount(), 0, "disposal clears both timers");
		await vi.advanceTimersByTimeAsync(BACKOFF_MS);

		assert.strictEqual(requests.length, 1);
		assert.strictEqual(state.logError.mock.calls.length, 0);
	});

	test("nothing is written after the tracker is disposed", async () => {
		const { tracker, requests, context } = await startTracker();

		await focus(fakeEditor("a"));
		for (const subscription of context.subscriptions) subscription.dispose();

		requests[0].resolve(["idea"]);
		await settle();

		assert.strictEqual(tracker.getLatestType(), "");
		assert.deepStrictEqual(graphFileWrites(), [false]);
		assert.strictEqual(state.logError.mock.calls.length, 0);
	});

	test("a queued switch is not sent after the tracker is disposed", async () => {
		const { requests, context } = await startTracker();

		await focus(fakeEditor("a"));
		await focus(fakeEditor("b"));
		for (const subscription of context.subscriptions) subscription.dispose();

		requests[0].resolve(["idea"]);
		await settle();

		assert.strictEqual(requests.length, 1);
		assert.strictEqual(state.logError.mock.calls.length, 0);
	});
});
