import { beforeEach, suite, test, vi } from "vitest";
import * as assert from "assert";
import type { ExtensionContext, Uri } from "vscode";

const {
	registerCommand,
	executeCommand,
	createTreeView,
	showTextDocument,
	showErrorMessage,
	logError,
	confirmOpen,
	registeredCommands,
} = vi.hoisted(() => {
	const registeredCommands = new Map<string, (...args: never[]) => unknown>();
	return {
		registerCommand: vi.fn(
			(id: string, handler: (...args: never[]) => unknown) => {
				registeredCommands.set(id, handler);
				return { dispose: () => {} };
			},
		),
		executeCommand: vi.fn(),
		createTreeView: vi.fn(() => ({
			reveal: vi.fn(),
			dispose: vi.fn(),
		})),
		showTextDocument: vi.fn(),
		showErrorMessage: vi.fn(),
		logError: vi.fn(),
		confirmOpen: vi.fn().mockResolvedValue(true),
		registeredCommands,
	};
});

vi.mock("vscode", async (importOriginal) => ({
	...(await importOriginal<object>()),
	commands: { executeCommand, registerCommand },
	window: {
		createTreeView,
		showTextDocument,
		showErrorMessage,
	},
}));

vi.mock("../../src/host/logger", async () =>
	(await import("./support/loggerMock")).mockLogger({ logError }),
);

vi.mock("../../src/host/trustedPaths", () => ({
	confirmOpen,
}));

import { FileExplorer } from "../../src/host/fileExplorer";

suite("FileExplorer — openFile", () => {
	const uri = { fsPath: "/workspace/events/a.txt" } as Uri;
	let openFile: (resource: Uri) => unknown;

	beforeEach(() => {
		confirmOpen.mockResolvedValue(true);
		registeredCommands.clear();
		const context = { subscriptions: [] } as unknown as ExtensionContext;
		new FileExplorer(context, [
			{
				scope: "events",
				uri: "file:///workspace/events/a.txt",
				logicalpath: "a.txt",
			},
		]);
		const handler = registeredCommands.get("cwtools-files.openFile");
		assert.ok(handler, "openFile command should be registered");
		openFile = handler as (resource: Uri) => unknown;
	});

	test("logs and shows an error when showTextDocument rejects", async () => {
		showTextDocument.mockRejectedValue(new Error("gone"));

		await openFile(uri);

		assert.strictEqual(logError.mock.calls.length, 1);
		assert.ok(
			String(logError.mock.calls[0][0]).includes(uri.fsPath),
			`logError should name the path, got: ${String(logError.mock.calls[0][0])}`,
		);
		assert.strictEqual(showErrorMessage.mock.calls.length, 1);
		assert.ok(
			String(showErrorMessage.mock.calls[0][0]).includes(uri.fsPath),
			`showErrorMessage should name the path, got: ${String(showErrorMessage.mock.calls[0][0])}`,
		);
	});

	test("does nothing when confirmOpen returns false", async () => {
		confirmOpen.mockResolvedValue(false);

		await openFile(uri);

		assert.strictEqual(logError.mock.calls.length, 0);
		assert.strictEqual(showErrorMessage.mock.calls.length, 0);
		assert.strictEqual(showTextDocument.mock.calls.length, 0);
	});
});
