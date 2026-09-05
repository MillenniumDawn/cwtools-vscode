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
	return {
		status,
		stateHandler,
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
		state.setStateHandler(() => undefined);
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
			onNotification: () => ({ dispose: () => undefined }),
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
