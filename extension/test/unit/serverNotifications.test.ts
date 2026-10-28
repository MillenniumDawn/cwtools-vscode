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
	const client = {
		onDidChangeState: vi.fn(() => ({ dispose: () => {} })),
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
	return { client, handlers, disposables };
}

function setup() {
	const { client, handlers, disposables } = fakeClient();
	const context = { subscriptions: [] as Array<{ dispose(): void }> };
	registerServerNotifications(
		context as unknown as ExtensionContext,
		client as unknown as LanguageClient,
	);
	return { context, handlers, disposables };
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
});