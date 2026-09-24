import * as assert from "assert";
import { beforeEach, suite, test, vi } from "vitest";
import type { ExtensionContext } from "vscode";
import type { LanguageClient } from "vscode-languageclient/node";

const state = vi.hoisted(() => {
	const status = {
		command: undefined as string | undefined,
		text: "",
		show: vi.fn(),
		dispose: vi.fn(),
	};
	let stateHandler:
		| ((event: { oldState: number; newState: number }) => void)
		| undefined;
	const notificationHandlers = new Map<string, (params: unknown) => void>();
	return {
		status,
		stateHandler,
		notificationHandlers,
		showInformationMessage: vi.fn(),
		setNotificationHandler: (
			method: string,
			handler: (params: unknown) => void,
		) => notificationHandlers.set(method, handler),
		getNotificationHandler: (method: string) =>
			notificationHandlers.get(method),
		setStateHandler: (
			handler: (event: { oldState: number; newState: number }) => void,
		) => {
			stateHandler = handler;
		},
		getStateHandler: () => stateHandler,
		executeCommand: vi.fn(),
		registerCommand: vi.fn(() => ({ dispose: () => undefined })),
	};
});

vi.mock("vscode", () => ({
	StatusBarAlignment: { Left: 1 },
	commands: {
		executeCommand: state.executeCommand,
		registerCommand: state.registerCommand,
	},
	l10n: {
		t: (message: string) => message,
	},
	window: {
		createOutputChannel: () => ({ appendLine: () => undefined }),
		createStatusBarItem: () => state.status,
		showInformationMessage: state.showInformationMessage,
	},
}));

vi.mock("vscode-languageclient/node", () => ({
	ExecuteCommandRequest: { type: {} },
	State: { Stopped: 1, Running: 2, Starting: 3, StartFailed: 4 },
}));

import { State } from "vscode-languageclient/node";
import { registerServerNotifications } from "../../src/host/serverNotifications";

suite("server notifications", () => {
	beforeEach(() => {
		state.executeCommand.mockClear();
		state.registerCommand.mockClear();
		state.status.text = "";
		state.showInformationMessage.mockReset();
		state.notificationHandlers.clear();
		state.setStateHandler(() => undefined);
	});

	test("offers Show Output when the workspace diagnostics budget is reached", async () => {
		state.showInformationMessage.mockResolvedValue("Show Output");
		const context = { subscriptions: [] } as unknown as ExtensionContext;
		const client = {
			onDidChangeState: () => ({ dispose: () => undefined }),
			onNotification: (method: string, handler: (params: unknown) => void) => {
				state.setNotificationHandler(method, handler);
				return { dispose: () => undefined };
			},
		} as unknown as LanguageClient;

		registerServerNotifications(context, client);
		const handler = state.getNotificationHandler(
			"workspaceDiagnosticsBudgetReached",
		);
		assert.ok(handler, "budget notification should be registered");
		handler({ budget: 2000, heldBack: 4 });
		await vi.waitFor(() =>
			assert.deepStrictEqual(state.executeCommand.mock.calls, [
				["cwtools.showOutput"],
			]),
		);

		assert.deepStrictEqual(state.showInformationMessage.mock.calls, [
			[
				"CWTools: workspace diagnostics are limited to {0} closed files per scan; {1} more were held back.",
				"Show Output",
			],
		]);
	});

	test("shows the diagnostics budget notice once per client session", () => {
		state.showInformationMessage.mockResolvedValue(undefined);
		const context = { subscriptions: [] } as unknown as ExtensionContext;
		const client = {
			onDidChangeState: () => ({ dispose: () => undefined }),
			onNotification: (method: string, handler: (params: unknown) => void) => {
				state.setNotificationHandler(method, handler);
				return { dispose: () => undefined };
			},
		} as unknown as LanguageClient;

		registerServerNotifications(context, client);
		const handler = state.getNotificationHandler(
			"workspaceDiagnosticsBudgetReached",
		);
		assert.ok(handler, "budget notification should be registered");
		handler({ budget: 2000, heldBack: 4 });
		handler({ budget: 2000, heldBack: 5 });

		assert.strictEqual(state.showInformationMessage.mock.calls.length, 1);
	});

	test("clears command availability when the client stops", () => {
		const context = { subscriptions: [] } as unknown as ExtensionContext;
		const client = {
			onDidChangeState: (
				handler: (event: { oldState: number; newState: number }) => void,
			) => {
				state.setStateHandler(handler);
				return { dispose: () => undefined };
			},
			onNotification: (method: string, handler: (params: unknown) => void) => {
				state.setNotificationHandler(method, handler);
				return { dispose: () => undefined };
			},
		} as unknown as LanguageClient;

		const notifications = registerServerNotifications(context, client);
		const handler = state.getStateHandler();
		assert.ok(handler, "state change handler should be registered");
		handler({ oldState: State.Running, newState: State.Stopped });

		assert.deepStrictEqual(state.executeCommand.mock.calls, [
			["setContext", "cwtoolsGraphAvailable", false],
			["setContext", "cwtoolsFixAllAvailable", false],
			["setContext", "cwtoolsFormatWorkspaceAvailable", false],
		]);
		assert.strictEqual(notifications.statusText(), "CWTools: stopped");
	});
});
