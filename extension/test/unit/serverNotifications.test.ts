import { beforeEach, suite, test, vi } from "vitest";
import * as assert from "assert";
import type { ExtensionContext } from "vscode";
import type { LanguageClient } from "vscode-languageclient/node";
import type * as VscodeStub from "./_stubs/vscode";

const state = vi.hoisted(() => ({
	statusBarItem: {
		text: "",
		command: undefined as string | undefined,
		show: vi.fn(),
		hide: vi.fn(),
		dispose: vi.fn(),
	},
	registerCommand: vi.fn(() => ({ dispose: () => {} })),
	executeCommand: vi.fn(),
}));

vi.mock("vscode", async (importOriginal) => {
	const original = await importOriginal<typeof VscodeStub>();
	return {
		...original,
		StatusBarAlignment: { Left: 1, Right: 2 },
		window: {
			...original.window,
			createStatusBarItem: (_alignment: unknown) => {
				state.statusBarItem.text = "";
				state.statusBarItem.show.mockClear();
				state.statusBarItem.hide.mockClear();
				state.statusBarItem.dispose.mockClear();
				return state.statusBarItem;
			},
			showQuickPick: vi.fn(),
		},
		commands: {
			registerCommand: state.registerCommand,
			executeCommand: state.executeCommand,
		},
	};
});

vi.mock("../../src/host/commands", () => ({
	publishCommandAvailability: vi.fn(),
	clearCommandAvailability: vi.fn(),
}));

vi.mock("../../src/host/commandProgress", () => ({
	commandProgressActive: vi.fn(() => false),
}));

vi.mock("../../src/host/fileExplorer", () => ({
	FileExplorer: class {
		dispose(): void {}
	},
}));

vi.mock("../../src/host/logger", async () =>
	(await import("./support/loggerMock")).mockLogger(),
);

// The real package does `require("vscode")` on load; only State is used at runtime.
vi.mock("vscode-languageclient/node", () => ({
	State: { Starting: 1, Running: 2, Stopped: 3, StartFailed: 4 },
}));

import { registerServerNotifications } from "../../src/host/serverNotifications";
type NotificationHandler = (params: never) => void;

interface FakeDisposable {
	disposed: boolean;
	dispose(): void;
}

function fakeClient() {
	const handlers = new Map<string, NotificationHandler>();
	const disposables = new Map<string, FakeDisposable>();
	const stateHandlers: Array<(e: { newState: number }) => void> = [];
	const client = {
		onDidChangeState: vi.fn((handler: (e: { newState: number }) => void) => {
			stateHandlers.push(handler);
			return { dispose: () => {} };
		}),
		onNotification: vi.fn((name: string, handler: NotificationHandler) => {
			// Match the vscode-languageclient shape: every subscription is a Disposable.
			const disposable: FakeDisposable = {
				disposed: false,
				dispose(): void {
					disposable.disposed = true;
				},
			};
			handlers.set(name, handler);
			disposables.set(name, disposable);
			return disposable;
		}),
	};
	return { client, handlers, disposables, stateHandlers };
}

function setup() {
	const { client, handlers, disposables, stateHandlers } = fakeClient();
	const context = { subscriptions: [] as Array<{ dispose(): void }> };
	const notifications = registerServerNotifications(
		context as unknown as ExtensionContext,
		client as unknown as LanguageClient,
	);
	return { context, handlers, disposables, stateHandlers, notifications };
}

suite("serverNotifications", () => {
	beforeEach(() => {
		state.statusBarItem.text = "";
	});

	test("dispatching a loadingBar notification updates the status text and disposing the context unsubscribes both listeners", () => {
		const { context, handlers, disposables } = setup();
		const loadingBar = disposables.get("loadingBar");
		const updateFileList = disposables.get("updateFileList");
		assert.ok(loadingBar, "loadingBar listener was not subscribed");
		assert.ok(updateFileList, "updateFileList listener was not subscribed");
		assert.strictEqual(loadingBar.disposed, false);
		assert.strictEqual(updateFileList.disposed, false);

		assert.strictEqual(state.statusBarItem.text, "CWTools: starting");
		handlers.get("loadingBar")!({ enable: false, value: "" } as never);
		assert.strictEqual(state.statusBarItem.text, "CWTools: ready");
		handlers.get("loadingBar")!({ enable: true, value: "Scanning" } as never);
		assert.strictEqual(state.statusBarItem.text, "Scanning");

		for (const subscription of context.subscriptions) subscription.dispose();

		assert.strictEqual(loadingBar.disposed, true);
		assert.strictEqual(updateFileList.disposed, true);
		assert.strictEqual(context.subscriptions.includes(loadingBar), true);
		assert.strictEqual(context.subscriptions.includes(updateFileList), true);
	});

	async function settled(wait: Promise<unknown>): Promise<boolean> {
		let completed = false;
		void wait.then(() => { completed = true; });
		await Promise.resolve();
		return completed;
	}

	test("a completed scan resolves the current wait as ready", async () => {
		const { handlers, notifications } = setup();
		const wait = notifications.initialScanDone();
		handlers.get("loadingBar")!({ enable: true, value: "Scanning" } as never);
		handlers.get("loadingBar")!({ enable: false, value: "" } as never);
		assert.strictEqual(await settled(wait), true);
		const outcome = await wait;
		assert.strictEqual(outcome.kind, "ready");
		assert.strictEqual(outcome.isCurrent(), true);
	});

	test("a loadingBar disable without a started scan does not finish the wait, markStopped does", async () => {
		const { handlers, notifications } = setup();
		const wait = notifications.initialScanDone();
		handlers.get("loadingBar")!({ enable: false, value: "" } as never);
		assert.strictEqual(await settled(wait), false, "wait must stay pending");
		notifications.markStopped();
		assert.strictEqual((await wait).kind, "stopped");
	});

	test("markStopped settles the initial scan wait as stopped", async () => {
		const { notifications } = setup();
		const wait = notifications.initialScanDone();
		notifications.markStopped();
		assert.strictEqual((await wait).kind, "stopped");
	});

	test("disposing the extension subscriptions settles a pending wait as stopped", async () => {
		const { context, notifications } = setup();
		const wait = notifications.initialScanDone();
		for (const subscription of context.subscriptions) subscription.dispose();
		assert.strictEqual((await wait).kind, "stopped");
	});

	test("the Stopped and StartFailed state changes settle the wait as stopped", async () => {
		const stopped = setup();
		const stoppedWait = stopped.notifications.initialScanDone();
		stopped.stateHandlers[0]({ newState: 3 }); // State.Stopped
		assert.strictEqual((await stoppedWait).kind, "stopped");

		const failed = setup();
		const failedWait = failed.notifications.initialScanDone();
		failed.stateHandlers[0]({ newState: 4 }); // State.StartFailed
		assert.strictEqual((await failedWait).kind, "stopped");
	});

	test("a pending wait from a replaced start settles as stopped and never adopts the replacement's scan", async () => {
		const { handlers, stateHandlers, notifications } = setup();
		const oldWait = notifications.initialScanDone();
		handlers.get("loadingBar")!({ enable: true, value: "Scanning" } as never);

		// Restart without a Stopped event: Starting must end the old wait (#1270).
		stateHandlers[0]({ newState: 1 }); // State.Starting
		assert.strictEqual((await oldWait).kind, "stopped");
		const newWait = notifications.initialScanDone();
		assert.notStrictEqual(
			newWait,
			oldWait,
			"the replacement must get a fresh wait",
		);
		assert.strictEqual(
			await settled(newWait),
			false,
			"the replacement's scan has not run yet",
		);
		handlers.get("loadingBar")!({ enable: true, value: "Scanning" } as never);
		handlers.get("loadingBar")!({ enable: false, value: "" } as never);
		assert.strictEqual((await newWait).kind, "ready");
	});

	test("a restarted server's scan completes its own wait, not the previous start's", async () => {
		const { handlers, stateHandlers, notifications } = setup();
		const firstWait = notifications.initialScanDone();
		handlers.get("loadingBar")!({ enable: true, value: "Scanning" } as never);
		handlers.get("loadingBar")!({ enable: false, value: "" } as never);
		assert.strictEqual((await firstWait).kind, "ready");

		stateHandlers[0]({ newState: 1 }); // State.Starting
		const secondWait = notifications.initialScanDone();
		assert.strictEqual(
			await settled(secondWait),
			false,
			"a restarted server must not inherit the resolved wait",
		);
		handlers.get("loadingBar")!({ enable: true, value: "Scanning" } as never);
		handlers.get("loadingBar")!({ enable: false, value: "" } as never);
		assert.strictEqual((await secondWait).kind, "ready");
	});

	test.each(["stop", "dispose"])("a ready outcome is invalidated on %s", async (end) => {
		const { context, handlers, notifications } = setup();
		handlers.get("loadingBar")!({ enable: true, value: "Scanning" } as never);
		handlers.get("loadingBar")!({ enable: false, value: "" } as never);
		const outcome = await notifications.initialScanDone();
		assert.strictEqual(outcome.isCurrent(), true);

		if (end === "stop") notifications.markStopped();
		else for (const subscription of context.subscriptions) subscription.dispose();

		assert.strictEqual(outcome.isCurrent(), false);
		assert.strictEqual((await notifications.initialScanDone()).isCurrent(), false);
	});

	test("a ready outcome stops being current once its start is replaced", async () => {
		const { handlers, stateHandlers, notifications } = setup();
		const firstWait = notifications.initialScanDone();
		handlers.get("loadingBar")!({ enable: true, value: "Scanning" } as never);
		handlers.get("loadingBar")!({ enable: false, value: "" } as never);
		const ready = await firstWait;
		assert.strictEqual(ready.isCurrent(), true);

		stateHandlers[0]({ newState: 1 }); // State.Starting
		assert.strictEqual(
			ready.isCurrent(),
			false,
			"the replacement must not inherit the ready outcome",
		);
	});
});