import * as assert from "assert";
import { beforeEach, suite, test, vi } from "vitest";
import { LSPErrorCodes } from "vscode-languageserver-protocol";
import type { ExtensionContext } from "vscode";
import type { EditorTracker } from "../../src/host/documentLanguage";
import type { LanguageClient } from "vscode-languageclient/node";

// registerCommands' handlers are the only seam between the palette entries and
// the server: runCancellableExecuteCommand turns each one into an
// ExecuteCommandRequest. These tests drive the captured handlers against a
// fake LanguageClient so a handler that silently became a no-op — dropped
// capability gate, lost request, swallowed failure — fails here instead of
// staying green while the workspace command does nothing.

const state = vi.hoisted(() => {
	class CancellationError extends Error {}
	const registeredCommands = new Map<string, (...args: never[]) => unknown>();
	const token = {
		isCancellationRequested: false,
		onCancellationRequested: () => ({ dispose: () => undefined }),
	};
	const withProgress = vi.fn(
		(
			_options: unknown,
			task: (
				progress: { report: (value: unknown) => void },
				cancelToken: unknown,
			) => Promise<unknown>,
		): Promise<unknown> => task({ report: () => undefined }, token),
	);
	return {
		CancellationError,
		registeredCommands,
		requestType: {},
		token,
		withProgress,
		showInformationMessage: vi.fn(),
		executeCommand: vi.fn(),
		showWarningMessage: vi.fn(),
		showErrorMessage: vi.fn(),
		showSaveDialog: vi.fn(),
		showTextDocument: vi.fn(),
		openTextDocument: vi.fn(),
		writeFile: vi.fn(),
		registerWebviewPanelSerializer: vi.fn(),
	};
});

vi.mock("vscode", async (importOriginal) => ({
	...(await importOriginal<object>()),
	CancellationError: state.CancellationError,
	ProgressLocation: { Notification: 15 },
	commands: {
		registerCommand: vi.fn(
			(id: string, handler: (...args: never[]) => unknown) => {
				state.registeredCommands.set(id, handler);
				return { dispose: () => undefined };
			},
		),
		executeCommand: state.executeCommand,
	},
	window: {
		createOutputChannel: () => ({ appendLine: () => undefined }),
		withProgress: state.withProgress,
		showInformationMessage: state.showInformationMessage,
		showWarningMessage: state.showWarningMessage,
		showErrorMessage: state.showErrorMessage,
		showSaveDialog: state.showSaveDialog,
		showTextDocument: state.showTextDocument,
		registerWebviewPanelSerializer: state.registerWebviewPanelSerializer,
	},
	workspace: {
		getConfiguration: () => ({ get: () => undefined }),
		fs: { writeFile: state.writeFile, readFile: vi.fn() },
		openTextDocument: state.openTextDocument,
	},
}));

vi.mock("vscode-languageclient/node", () => ({
	ExecuteCommandRequest: { type: state.requestType },
}));

import {
	clearCommandAvailability,
	registerCommands,
} from "../../src/host/commands";

interface FakeClient {
	initializeResult: {
		capabilities: {
			executeCommandProvider: {
				commands: string[];
				workDoneProgress?: boolean;
			};
		};
	};
	onProgress: LanguageClient["onProgress"];
	isRunning: ReturnType<typeof vi.fn>;
	sendRequest: ReturnType<typeof vi.fn>;
}

function fakeClient(commands: string[], workDoneProgress = false): FakeClient {
	return {
		initializeResult: {
			capabilities: {
				executeCommandProvider: {
					commands,
					...(workDoneProgress ? { workDoneProgress: true } : {}),
				},
			},
		},
		onProgress: vi.fn(() => ({ dispose: () => undefined })),
		isRunning: vi.fn(() => true),
		sendRequest: vi.fn(),
	};
}

function register(client: FakeClient): void {
	const context = {
		extensionPath: "/ext",
		subscriptions: [],
	} as unknown as ExtensionContext;
	const tracker = { getLatestType: () => "" } as unknown as EditorTracker;
	registerCommands(
		context,
		client as unknown as LanguageClient,
		tracker,
		undefined,
	);
}

function handler(id: string): () => Promise<void> {
	const found = state.registeredCommands.get(id);
	assert.ok(found, `${id} must have a registered handler`);
	return found as () => Promise<void>;
}

suite("registered workspace commands", () => {
	beforeEach(() => {
		vi.clearAllMocks();
		state.registeredCommands.clear();
		state.token.isCancellationRequested = false;
	});

	test("clears command availability contexts when the server stops", () => {
		clearCommandAvailability();

		assert.deepStrictEqual(state.executeCommand.mock.calls, [
			["setContext", "cwtoolsGraphAvailable", false],
			["setContext", "cwtoolsFixAllAvailable", false],
			["setContext", "cwtoolsFormatWorkspaceAvailable", false],
		]);
	});

	suite("cwtools.showGraph", () => {
		test("reports that the server stopped instead of loading graph data", async () => {
			const client = fakeClient(["getGraphData"]);
			client.isRunning.mockReturnValue(false);
			register(client);

			await handler("cwtools.showGraph")();

			assert.deepStrictEqual(state.showWarningMessage.mock.calls, [
				[
					"CWTools: the language server is stopped. Run 'CWTools: Restart Server' to start it again.",
				],
			]);
			assert.deepStrictEqual(client.sendRequest.mock.calls, []);
		});
	});

	suite("cwtools.fixAllWorkspace", () => {
		test("reports that the server stopped instead of sending a request", async () => {
			const client = fakeClient(["fixAllWorkspace"]);
			client.isRunning.mockReturnValue(false);
			register(client);

			await handler("cwtools.fixAllWorkspace")();

			assert.deepStrictEqual(state.showWarningMessage.mock.calls, [
				[
					"CWTools: the language server is stopped. Run 'CWTools: Restart Server' to start it again.",
				],
			]);
			assert.deepStrictEqual(client.sendRequest.mock.calls, []);
			assert.deepStrictEqual(state.showErrorMessage.mock.calls, []);
		});

		test("warns and sends nothing when the server doesn't advertise the command", async () => {
			const client = fakeClient(["getGraphData"]);
			register(client);

			await handler("cwtools.fixAllWorkspace")();

			assert.deepStrictEqual(state.showWarningMessage.mock.calls, [
				[
					"CWTools: this language server doesn't support fixing the workspace. Update the language server to enable it.",
				],
			]);
			assert.deepStrictEqual(client.sendRequest.mock.calls, []);
			assert.deepStrictEqual(state.showErrorMessage.mock.calls, []);
		});

		test("sends the exact command without a workDoneToken and shows the server's reply", async () => {
			// The edit lands as one snapshot; Cancel stays the $/cancelRequest
			// path, so no token even against a server that supports progress.
			const client = fakeClient(["fixAllWorkspace"], true);
			sendRequestResolves(client, "Fixed 12 problem(s) in 3 file(s).");
			register(client);

			await handler("cwtools.fixAllWorkspace")();

			assert.deepStrictEqual(state.withProgress.mock.calls[0]?.[0], {
				location: 15,
				title: "CWTools: Fix all auto-fixable problems in workspace",
				cancellable: true,
			});
			assert.deepStrictEqual(client.sendRequest.mock.calls, [
				[
					state.requestType,
					{ command: "fixAllWorkspace", arguments: [] },
					state.token,
				],
			]);
			assert.deepStrictEqual(state.showInformationMessage.mock.calls, [
				["CWTools: Fixed 12 problem(s) in 3 file(s)."],
			]);
			assert.deepStrictEqual(state.showErrorMessage.mock.calls, []);
		});

		test("shows nothing when the server reports no fixes", async () => {
			const client = fakeClient(["fixAllWorkspace"]);
			sendRequestResolves(client, "");
			register(client);

			await handler("cwtools.fixAllWorkspace")();

			assert.deepStrictEqual(state.showInformationMessage.mock.calls, []);
		});

		test("shows a failure message when the server rejects", async () => {
			const client = fakeClient(["fixAllWorkspace"]);
			sendRequestRejects(client, new Error("server unavailable"));
			register(client);

			await handler("cwtools.fixAllWorkspace")();

			assert.deepStrictEqual(state.showErrorMessage.mock.calls, [
				["CWTools: fixAllWorkspace failed: server unavailable"],
			]);
			assert.deepStrictEqual(state.showInformationMessage.mock.calls, []);
		});

		test("stays silent when the request is cancelled", async () => {
			const client = fakeClient(["fixAllWorkspace"]);
			sendRequestRejects(client, { code: LSPErrorCodes.RequestCancelled });
			register(client);

			await handler("cwtools.fixAllWorkspace")();

			assert.deepStrictEqual(state.showErrorMessage.mock.calls, []);
			assert.deepStrictEqual(state.showInformationMessage.mock.calls, []);
		});
	});

	suite("cwtools.formatWorkspace", () => {
		test("reports that the server stopped instead of sending a request", async () => {
			const client = fakeClient(["formatWorkspace"]);
			client.isRunning.mockReturnValue(false);
			register(client);

			await handler("cwtools.formatWorkspace")();

			assert.deepStrictEqual(state.showWarningMessage.mock.calls, [
				[
					"CWTools: the language server is stopped. Run 'CWTools: Restart Server' to start it again.",
				],
			]);
			assert.deepStrictEqual(client.sendRequest.mock.calls, []);
			assert.deepStrictEqual(state.showErrorMessage.mock.calls, []);
		});

		test("warns and sends nothing when the server doesn't advertise the command", async () => {
			const client = fakeClient(["getGraphData"]);
			register(client);

			await handler("cwtools.formatWorkspace")();

			assert.deepStrictEqual(state.showWarningMessage.mock.calls, [
				[
					"CWTools: this language server doesn't support formatting the workspace. Update the language server to enable it.",
				],
			]);
			assert.deepStrictEqual(client.sendRequest.mock.calls, []);
		});

		test("carries a workDoneToken against a server that supports progress and shows the reply", async () => {
			const client = fakeClient(["formatWorkspace"], true);
			sendRequestResolves(client, "Formatted 2 file(s)");
			register(client);

			await handler("cwtools.formatWorkspace")();

			const [type, params, cancellation] = client.sendRequest.mock.calls[0] as [
				unknown,
				{ command: string; arguments: unknown[]; workDoneToken?: string },
				unknown,
			];
			assert.strictEqual(type, state.requestType);
			assert.strictEqual(params.command, "formatWorkspace");
			assert.deepStrictEqual(params.arguments, []);
			assert.match(
				params.workDoneToken ?? "",
				/^cwtools\/command\//,
				"the request must carry a workDoneToken",
			);
			assert.strictEqual(cancellation, undefined);
			assert.deepStrictEqual(state.showInformationMessage.mock.calls, [
				["CWTools: Formatted 2 file(s)"],
			]);
		});

		test("shows a failure message when the server rejects", async () => {
			const client = fakeClient(["formatWorkspace"]);
			sendRequestRejects(client, new Error("boom"));
			register(client);

			await handler("cwtools.formatWorkspace")();

			assert.deepStrictEqual(state.showErrorMessage.mock.calls, [
				["CWTools: formatWorkspace failed: boom"],
			]);
			assert.deepStrictEqual(state.showInformationMessage.mock.calls, []);
		});

		test("stays silent when the request is cancelled", async () => {
			const client = fakeClient(["formatWorkspace"]);
			sendRequestRejects(client, { code: LSPErrorCodes.RequestCancelled });
			register(client);

			await handler("cwtools.formatWorkspace")();

			assert.deepStrictEqual(state.showErrorMessage.mock.calls, []);
			assert.deepStrictEqual(state.showInformationMessage.mock.calls, []);
		});
	});

	suite("cwtools.exportProfilingLog", () => {
		test("sends the exact request and saves the fetched log", async () => {
			const client = fakeClient(["exportProfilingLog"]);
			const log = "phase=parse 12ms\nrss=31MB";
			sendRequestResolves(client, log);
			state.showSaveDialog.mockResolvedValue({ fsPath: "/tmp/prof.log" });
			register(client);

			await handler("cwtools.exportProfilingLog")();

			assert.deepStrictEqual(client.sendRequest.mock.calls, [
				[
					state.requestType,
					{ command: "exportProfilingLog", arguments: [] },
					state.token,
				],
			]);
			assert.deepStrictEqual(state.showSaveDialog.mock.calls, [
				[
					{
						filters: { Log: ["log", "txt"] },
						saveLabel: "Export CWTools profiling log",
					},
				],
			]);
			assert.deepStrictEqual(state.writeFile.mock.calls, [
				[{ fsPath: "/tmp/prof.log" }, Buffer.from(log, "utf8")],
			]);
			assert.deepStrictEqual(state.showInformationMessage.mock.calls, [
				["CWTools: profiling log written to /tmp/prof.log"],
			]);
		});

		test("warns instead of saving an empty log", async () => {
			const client = fakeClient(["exportProfilingLog"]);
			sendRequestResolves(client, "");
			register(client);

			await handler("cwtools.exportProfilingLog")();

			assert.deepStrictEqual(state.showWarningMessage.mock.calls, [
				[
					"CWTools: profiling log is empty. Turn on 'cwtools.profiling', reload the window, reproduce the slowdown, then export.",
				],
			]);
			assert.deepStrictEqual(state.showSaveDialog.mock.calls, []);
			assert.deepStrictEqual(state.writeFile.mock.calls, []);
		});

		test("saves nothing when the save dialog is cancelled", async () => {
			const client = fakeClient(["exportProfilingLog"]);
			sendRequestResolves(client, "profiling data");
			state.showSaveDialog.mockResolvedValue(undefined);
			register(client);

			await handler("cwtools.exportProfilingLog")();

			assert.deepStrictEqual(state.writeFile.mock.calls, []);
			assert.deepStrictEqual(state.showInformationMessage.mock.calls, []);
		});

		test("shows a failure message when the fetch rejects", async () => {
			const client = fakeClient(["exportProfilingLog"]);
			sendRequestRejects(client, new Error("boom"));
			register(client);

			await handler("cwtools.exportProfilingLog")();

			assert.deepStrictEqual(state.showErrorMessage.mock.calls, [
				["CWTools: could not fetch profiling log: boom"],
			]);
			assert.deepStrictEqual(state.writeFile.mock.calls, []);
		});

		test("stays silent when the fetch is cancelled", async () => {
			const client = fakeClient(["exportProfilingLog"]);
			sendRequestRejects(client, { code: LSPErrorCodes.RequestCancelled });
			register(client);

			await handler("cwtools.exportProfilingLog")();

			assert.deepStrictEqual(state.showErrorMessage.mock.calls, []);
			assert.deepStrictEqual(state.showInformationMessage.mock.calls, []);
			assert.deepStrictEqual(state.writeFile.mock.calls, []);
		});
	});
});

function sendRequestResolves(client: FakeClient, result: unknown): void {
	client.sendRequest.mockResolvedValue(result);
}

function sendRequestRejects(client: FakeClient, error: unknown): void {
	client.sendRequest.mockRejectedValue(error);
}
