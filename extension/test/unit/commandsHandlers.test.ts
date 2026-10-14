import * as assert from "assert";
import { beforeEach, suite, test, vi } from "vitest";
import { LSPErrorCodes } from "vscode-languageserver-protocol";
import type { ExtensionContext } from "vscode";
import type {
	LogOutputChannel,
	WebviewPanel,
	WebviewPanelSerializer,
} from "vscode";
import type * as VscodeStub from "./_stubs/vscode";
import type { EditorTracker } from "../../src/host/documentLanguage";
import type { LanguageClient } from "vscode-languageclient/node";

// registerCommands' handlers are the only seam between the palette entries and
// the server. The host suites run the happy path against the real server; these
// drive the captured handlers against a fake LanguageClient for the branches a
// healthy server never takes: stopped, unadvertised, failed, cancelled, and
// responses arriving out of order.

const state = vi.hoisted(() => {
	const registeredCommands = new Map<string, (...args: never[]) => unknown>();
	const initialiseGraph = vi.fn();
	const logError = vi.fn();
	let graphSerializerRegistration:
		| { viewType: string; serializer: WebviewPanelSerializer }
		| undefined;
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
		registeredCommands,
		requestType: {},
		token,
		withProgress,
		showInformationMessage: vi.fn(),
		showOpenDialog: vi.fn(),
		executeCommand: vi.fn(),
		showWarningMessage: vi.fn(),
		readFile: vi.fn(),
		showErrorMessage: vi.fn(),
		showInputBox: vi.fn(),
		showSaveDialog: vi.fn(),
		writeFile: vi.fn(),
		initialiseGraph,
		logError,
		getGraphSerializerRegistration: () => graphSerializerRegistration,
		setGraphSerializer: (
			viewType: string,
			serializer: WebviewPanelSerializer | undefined,
		) => {
			graphSerializerRegistration = serializer ? { viewType, serializer } : undefined;
		},
	};
});

vi.mock("vscode", async (importOriginal) => {
	const original = await importOriginal<typeof VscodeStub>();
	return {
		...original,
		commands: {
			registerCommand: (id: string, handler: (...args: never[]) => unknown) => {
				state.registeredCommands.set(id, handler);
				return { dispose: () => undefined };
			},
			executeCommand: state.executeCommand,
		},
		window: {
			...original.window,
			withProgress: state.withProgress,
			showInformationMessage: state.showInformationMessage,
			showOpenDialog: state.showOpenDialog,
			showWarningMessage: state.showWarningMessage,
			showErrorMessage: state.showErrorMessage,
			showInputBox: state.showInputBox,
			showSaveDialog: state.showSaveDialog,
			registerWebviewPanelSerializer: (
				_viewType: string,
				serializer: WebviewPanelSerializer,
			) => {
				state.setGraphSerializer(_viewType, serializer);
				return { dispose: () => undefined };
			},
		},
		workspace: {
			...original.workspace,
			fs: { writeFile: state.writeFile, readFile: state.readFile },
		},
	};
});

vi.mock("vscode-languageclient/node", () => ({
	ExecuteCommandRequest: { type: state.requestType },
}));

import {
	clearCommandAvailability,
	registerCommands,
} from "../../src/host/commands";
import { GraphPanel } from "../../src/host/graphPanel";
import { initializeLogger } from "../../src/host/logger";

const graphPanelCreate = vi.spyOn(GraphPanel, "create").mockImplementation(() => {
	GraphPanel.currentPanel = {
		initialiseGraph: state.initialiseGraph,
	} as unknown as GraphPanel;
});

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

function register(client: FakeClient, latestType = ""): void {
	const context = {
		extensionPath: "/ext",
		subscriptions: [],
	} as unknown as ExtensionContext;
	const tracker = {
		getLatestType: () => latestType,
	} as unknown as EditorTracker;
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

interface Deferred<T> {
	promise: Promise<T>;
	resolve: (value: T) => void;
	reject: (reason?: unknown) => void;
}

function deferred<T>(): Deferred<T> {
	let resolve!: (value: T) => void;
	let reject!: (reason?: unknown) => void;
	const promise = new Promise<T>((resolvePromise, rejectPromise) => {
		resolve = resolvePromise;
		reject = rejectPromise;
	});
	return { promise, resolve, reject };
}

interface GraphRequest {
	entityType: unknown;
	depth: unknown;
	result: Deferred<unknown[]>;
}

function mockGraphRequests(): GraphRequest[] {
	const requests: GraphRequest[] = [];
	state.executeCommand.mockImplementation(
		(command: unknown, entityType: unknown, depth: unknown) => {
			if (command !== "getGraphData") {
				return undefined;
			}
			const result = deferred<unknown[]>();
			requests.push({ entityType, depth, result });
			return result.promise;
		},
	);
	return requests;
}

const STOPPED =
	"CWTools: the language server is stopped. Run 'CWTools: Restart Server' to start it again.";
const bytes = (text: string) => new TextEncoder().encode(text);

async function capturedSerializer(): Promise<WebviewPanelSerializer> {
	await vi.waitFor(() => assert.ok(state.getGraphSerializerRegistration()));
	const registration = state.getGraphSerializerRegistration();
	assert.ok(registration);
	assert.strictEqual(registration.viewType, GraphPanel.viewType);
	return registration.serializer;
}

async function withRestoreRecorder(
	body: (restoredPanel: GraphPanel, webviewPanel: WebviewPanel) => Promise<void>,
	initialiseGraph: ReturnType<typeof vi.fn> = state.initialiseGraph,
): Promise<void> {
	const restoredPanel = {
		initialiseGraph,
	} as unknown as GraphPanel;
	const webviewPanel = {} as WebviewPanel;
	const restore = vi.spyOn(GraphPanel, "restore").mockImplementation(() => {
		GraphPanel.currentPanel = restoredPanel;
		return restoredPanel;
	});
	state.initialiseGraph.mockClear();
	graphPanelCreate.mockClear();
	try {
		await body(restoredPanel, webviewPanel);
		assert.strictEqual(restore.mock.calls.length, 1);
		assert.strictEqual(restore.mock.calls[0][0], "/ext");
		assert.strictEqual(restore.mock.calls[0][1], webviewPanel);
	} finally {
		restore.mockRestore();
		GraphPanel.currentPanel = undefined;
	}
}

suite("registered workspace commands", () => {
	beforeEach(() => {
		state.executeCommand.mockReset();
		state.showInputBox.mockReset();
		state.showOpenDialog.mockReset();
		state.readFile.mockReset();
		state.showSaveDialog.mockReset();
		state.logError.mockReset();
		state.setGraphSerializer("", undefined);
		initializeLogger({ error: state.logError } as unknown as LogOutputChannel);
		GraphPanel.currentPanel = undefined;
		state.registeredCommands.clear();
	});

	test("clears command availability contexts when the server stops", () => {
		clearCommandAvailability();

		assert.deepStrictEqual(state.executeCommand.mock.calls, [
			["setContext", "cwtoolsGraphAvailable", false],
			["setContext", "cwtoolsFixAllAvailable", false],
			["setContext", "cwtoolsFormatWorkspaceAvailable", false],
			["setContext", "cwtoolsValidateWorkspaceAvailable", false],
		]);
	});

	suite("graph requests", () => {
		test("showGraph reports that the server stopped instead of loading graph data", async () => {
			const client = fakeClient(["getGraphData"]);
			client.isRunning.mockReturnValue(false);
			register(client);

			await handler("cwtools.showGraph")();

			assert.deepStrictEqual(state.showWarningMessage.mock.calls, [[STOPPED]]);
			assert.deepStrictEqual(state.executeCommand.mock.calls, []);
		});

		test("renders only the newest depth when responses resolve out of order", async () => {
			const requests = mockGraphRequests();
			state.showInputBox.mockResolvedValue("1");
			register(fakeClient(["getGraphData"]), "event");

			const oldRequest = handler("cwtools.showGraph")();
			await vi.waitFor(() => assert.strictEqual(requests.length, 1));
			const newRequest = handler("cwtools.setGraphDepth")();
			await vi.waitFor(() => assert.strictEqual(requests.length, 2));

			assert.deepStrictEqual(
				requests.map(({ entityType, depth }) => [entityType, depth]),
				[
					["event", 3],
					["event", 1],
				],
			);
			requests[1].result.resolve([{ id: "new" }]);
			await newRequest;
			requests[0].result.resolve([{ id: "old" }]);
			await oldRequest;

			assert.deepStrictEqual(state.initialiseGraph.mock.calls, [
				[[{ id: "new" }], 1, { source: "server", entityType: "event", depth: 1 }],
			]);
		});

		test("does not revive a stale graph after the newest request fails", async () => {
			const requests = mockGraphRequests();
			state.showInputBox.mockResolvedValue("1");
			register(fakeClient(["getGraphData"]), "event");

			const oldRequest = handler("cwtools.showGraph")();
			await vi.waitFor(() => assert.strictEqual(requests.length, 1));
			const newRequest = handler("cwtools.setGraphDepth")();
			await vi.waitFor(() => assert.strictEqual(requests.length, 2));

			requests[1].result.reject(new Error("newest failed"));
			await assert.rejects(newRequest, /newest failed/);
			requests[0].result.resolve([{ id: "old" }]);
			await oldRequest;

			assert.deepStrictEqual(state.initialiseGraph.mock.calls, []);
		});

		test("keeps a newer import when an older server graph completes", async () => {
			const requests = mockGraphRequests();
			register(fakeClient(["getGraphData"]), "event");
			const server = handler("cwtools.showGraph")();
			await vi.waitFor(() => assert.strictEqual(requests.length, 1));
			state.showOpenDialog.mockResolvedValue([{ fsPath: "/tmp/graph.json" }]);
			state.readFile.mockResolvedValue(bytes("new json"));

			await handler("cwtools.graphFromJson")();
			requests[0].result.resolve([{ id: "old" }]);
			await server;

			assert.deepStrictEqual(state.initialiseGraph.mock.calls, [
				["new json", 1, { source: "json", fileName: "graph.json" }],
			]);
		});

		test("renders only the last selected import when reads finish in reverse order", async () => {
			register(fakeClient([]));
			state.showOpenDialog
				.mockResolvedValueOnce([{ fsPath: "first.json" }])
				.mockResolvedValueOnce([{ fsPath: "second.json" }]);
			const firstBytes = deferred<Uint8Array>();
			const secondBytes = deferred<Uint8Array>();
			state.readFile
				.mockReturnValueOnce(firstBytes.promise)
				.mockReturnValueOnce(secondBytes.promise);

			const first = handler("cwtools.graphFromJson")();
			await vi.waitFor(() => assert.strictEqual(state.readFile.mock.calls.length, 1));
			const second = handler("cwtools.graphFromJson")();
			await vi.waitFor(() => assert.strictEqual(state.readFile.mock.calls.length, 2));
			secondBytes.resolve(bytes("second"));
			await second;
			firstBytes.resolve(bytes("first"));
			await first;

			assert.deepStrictEqual(state.initialiseGraph.mock.calls, [
				["second", 1, { source: "json", fileName: "second.json" }],
			]);
		});

		test("does not supersede a pending server graph when the import dialog is cancelled", async () => {
			const requests = mockGraphRequests();
			register(fakeClient(["getGraphData"]), "event");
			const server = handler("cwtools.showGraph")();
			await vi.waitFor(() => assert.strictEqual(requests.length, 1));
			state.showOpenDialog.mockResolvedValue(undefined);

			await handler("cwtools.graphFromJson")();
			requests[0].result.resolve([{ id: "pending" }]);
			await server;

			assert.deepStrictEqual(state.readFile.mock.calls, []);
			assert.deepStrictEqual(state.initialiseGraph.mock.calls, [
				[[{ id: "pending" }], 1, { source: "server", entityType: "event", depth: 3 }],
			]);
		});
	});

	suite("saved graph restoration", () => {
		test("re-requests a saved server graph with its entity type and depth", async () => {
			await withRestoreRecorder(async (restoredPanel, webviewPanel) => {
				const requests = mockGraphRequests();
				register(fakeClient(["getGraphData"]), "current-type");
				const serializer = await capturedSerializer();
				const restoring = serializer.deserializeWebviewPanel(
					webviewPanel,
					{ source: "server", entityType: "saved-type", depth: 5 },
				);
				await vi.waitFor(() => assert.strictEqual(requests.length, 1));
				assert.deepStrictEqual(
					[requests[0].entityType, requests[0].depth],
					["saved-type", 5],
				);
				requests[0].result.resolve([{ id: "restored" }]);
				await restoring;

				assert.deepStrictEqual(state.initialiseGraph.mock.calls, [
					[[{ id: "restored" }], 1, {
						source: "server",
						entityType: "saved-type",
						depth: 5,
					}],
				]);
				assert.strictEqual(GraphPanel.currentPanel, restoredPanel);
			});
		});

		test("re-prompts for a saved JSON graph and initializes the selected file", async () => {
			const restoredInitialiseGraph = vi.fn();
			await withRestoreRecorder(async (_restoredPanel, webviewPanel) => {
				register(fakeClient([]));
				const serializer = await capturedSerializer();
				state.showOpenDialog.mockResolvedValue([{ fsPath: "/tmp/rebuilt-graph.json" }]);
				state.readFile.mockResolvedValue(bytes('{"elements":{"nodes":[],"edges":[]}}'));

				await serializer.deserializeWebviewPanel(
					webviewPanel,
					{ source: "json", fileName: "old-graph.json" },
				);

				assert.deepStrictEqual(state.showOpenDialog.mock.calls, [
					[{ filters: { Json: ["json"] } }],
				]);
				assert.deepStrictEqual(state.readFile.mock.calls, [
					[{ fsPath: "/tmp/rebuilt-graph.json" }],
				]);
				assert.deepStrictEqual(restoredInitialiseGraph.mock.calls, [
					['{"elements":{"nodes":[],"edges":[]}}', 1, {
						source: "json",
						fileName: "rebuilt-graph.json",
					}],
				]);
				assert.deepStrictEqual(state.initialiseGraph.mock.calls, []);
				assert.deepStrictEqual(graphPanelCreate.mock.calls, []);
			}, restoredInitialiseGraph);
		});

		test("explains why a saved JSON graph cannot be restored when selection is canceled", async () => {
			await withRestoreRecorder(async (_restoredPanel, webviewPanel) => {
				register(fakeClient([]));
				const serializer = await capturedSerializer();
				state.showOpenDialog.mockResolvedValue(undefined);

				await serializer.deserializeWebviewPanel(
					webviewPanel,
					{ source: "json", fileName: "saved-graph.json" },
				);

				assert.deepStrictEqual(state.showInformationMessage.mock.calls, [[
					"CWTools: graph data from a JSON export isn't persisted across reloads. Run 'CWTools: Recreate graph from json' to rebuild it.",
				]]);
				assert.deepStrictEqual(state.readFile.mock.calls, []);
				assert.deepStrictEqual(state.initialiseGraph.mock.calls, []);
			});
		});

		test("falls back to the latest entity type and current depth when state is absent", async () => {
			await withRestoreRecorder(async (_restoredPanel, webviewPanel) => {
				const requests = mockGraphRequests();
				register(fakeClient(["getGraphData"]), "latest-type");
				const adjustDepth = handler("cwtools.setGraphDepth");
				state.showInputBox.mockResolvedValue("2");
				const depthChange = adjustDepth();
				await vi.waitFor(() => assert.strictEqual(requests.length, 1));
				requests[0].result.resolve([{ id: "depth-update" }]);
				await depthChange;

				const serializer = await capturedSerializer();
				const restoring = serializer.deserializeWebviewPanel(
					webviewPanel,
					undefined,
				);
				await vi.waitFor(() => assert.strictEqual(requests.length, 2));
				assert.deepStrictEqual(
					[requests[1].entityType, requests[1].depth],
					["latest-type", 2],
				);
				requests[1].result.resolve([{ id: "fallback" }]);
				await restoring;

				assert.deepStrictEqual(state.initialiseGraph.mock.calls, [
					[[{ id: "depth-update" }], 1, {
						source: "server",
						entityType: "latest-type",
						depth: 2,
					}],
					[[{ id: "fallback" }], 1, {
						source: "server",
						entityType: "latest-type",
						depth: 2,
					}],
				]);
			});
		});

		test("warns when the restored server does not provide graph data", async () => {
			await withRestoreRecorder(async (_restoredPanel, webviewPanel) => {
				register(fakeClient([]));
				const serializer = await capturedSerializer();

				await serializer.deserializeWebviewPanel(
					webviewPanel,
					{ source: "server", entityType: "saved-type", depth: 5 },
				);

				assert.deepStrictEqual(state.showWarningMessage.mock.calls, [[
					"CWTools: this language server doesn't provide graph data, so the graph can't be restored.",
				]]);
				assert.deepStrictEqual(state.executeCommand.mock.calls, []);
				assert.deepStrictEqual(state.initialiseGraph.mock.calls, []);
			});
		});

		test("explains the missing saved graph when the server cannot rebuild it", async () => {
			await withRestoreRecorder(async (_restoredPanel, webviewPanel) => {
				register(fakeClient([]), "latest-type");
				const serializer = await capturedSerializer();

				await serializer.deserializeWebviewPanel(
					webviewPanel,
					undefined,
				);

				assert.deepStrictEqual(state.showInformationMessage.mock.calls, [[
					"CWTools: graph data isn't persisted across reloads. Run 'CWTools: Show graph' to rebuild the graph.",
				]]);
				assert.deepStrictEqual(state.executeCommand.mock.calls, []);
				assert.deepStrictEqual(state.initialiseGraph.mock.calls, []);
			});
		});

		test("logs saved server graph request failures", async () => {
			await withRestoreRecorder(async (_restoredPanel, webviewPanel) => {
				const requests = mockGraphRequests();
				register(fakeClient(["getGraphData"]), "latest-type");
				const serializer = await capturedSerializer();
				const restoring = serializer.deserializeWebviewPanel(
					webviewPanel,
					{ source: "server", entityType: "saved-type", depth: 5 },
				);
				await vi.waitFor(() => assert.strictEqual(requests.length, 1));
				requests[0].result.reject(new Error("saved request failed"));
				await restoring;

				assert.deepStrictEqual(state.logError.mock.calls, [[
					"graph panel restore failed: saved request failed",
				]]);
				assert.deepStrictEqual(state.initialiseGraph.mock.calls, []);
			});
		});

		test("logs failures reading a re-selected JSON graph", async () => {
			await withRestoreRecorder(async (_restoredPanel, webviewPanel) => {
				register(fakeClient([]));
				const serializer = await capturedSerializer();
				const failure = new Error("saved file could not be read");
				state.showOpenDialog.mockResolvedValue([{ fsPath: "/tmp/rebuilt-graph.json" }]);
				state.readFile.mockRejectedValue(failure);

				await serializer.deserializeWebviewPanel(
					webviewPanel,
					{ source: "json", fileName: "old-graph.json" },
				);

				assert.deepStrictEqual(state.logError.mock.calls, [[
					"graph panel restore failed: saved file could not be read",
				]]);
				assert.deepStrictEqual(state.initialiseGraph.mock.calls, []);
			});
		});
	});

	suite("cwtools.setGraphDepth", () => {
		interface InputBoxOptions {
			value: string;
			validateInput: (value: string) => string | undefined;
		}
		const prompts = () =>
			state.showInputBox.mock.calls.map(([options]) => options as InputBoxOptions);

		async function redrawAt(requests: GraphRequest[], answer: string): Promise<void> {
			const before = requests.length;
			state.showInputBox.mockResolvedValueOnce(answer);
			const pending = handler("cwtools.setGraphDepth")();
			await vi.waitFor(() => assert.strictEqual(requests.length, before + 1));
			requests[before].result.resolve([]);
			await pending;
		}

		test("the prompt accepts only whole numbers of at least 1", async () => {
			register(fakeClient(["getGraphData"]), "event");

			await handler("cwtools.setGraphDepth")();

			const { validateInput } = prompts()[0];
			for (const input of ["", " ", "0", "-2", "1.5", "abc", "9007199254740993"]) {
				assert.strictEqual(
					validateInput(input),
					"Please enter a whole number of at least 1",
					JSON.stringify(input),
				);
			}
			for (const input of ["1", "3", "12"]) {
				assert.strictEqual(validateInput(input), undefined, input);
			}
		});

		test.each(["0", undefined])("sends no request and keeps the remembered depth when the prompt answers %j", async (answer) => {
			const requests = mockGraphRequests();
			register(fakeClient(["getGraphData"]), "event");
			await redrawAt(requests, "2");

			state.showInputBox.mockResolvedValueOnce(answer);
			await handler("cwtools.setGraphDepth")();
			await handler("cwtools.setGraphDepth")();

			assert.deepStrictEqual(
				requests.map((request) => [request.entityType, request.depth]),
				[["event", 2]],
			);
			assert.deepStrictEqual(
				prompts().map((prompt) => prompt.value),
				["3", "2", "2"],
			);
		});
	});

	for (const [command, unsupported] of [
		["fixAllWorkspace", "fixing the workspace"],
		["formatWorkspace", "formatting the workspace"],
	]) {
		suite(`cwtools.${command}`, () => {
			test("reports that the server stopped instead of sending a request", async () => {
				const client = fakeClient([command]);
				client.isRunning.mockReturnValue(false);
				register(client);

				await handler(`cwtools.${command}`)();

				assert.deepStrictEqual(state.showWarningMessage.mock.calls, [[STOPPED]]);
				assert.deepStrictEqual(client.sendRequest.mock.calls, []);
			});

			test("warns and sends nothing when the server doesn't advertise the command", async () => {
				const client = fakeClient(["getGraphData"]);
				register(client);

				await handler(`cwtools.${command}`)();

				assert.deepStrictEqual(state.showWarningMessage.mock.calls, [
					[
						`CWTools: this language server doesn't support ${unsupported}. Update the language server to enable it.`,
					],
				]);
				assert.deepStrictEqual(client.sendRequest.mock.calls, []);
			});

			test("shows a failure message when the server rejects", async () => {
				const client = fakeClient([command]);
				client.sendRequest.mockRejectedValue(new Error("boom"));
				register(client);

				await handler(`cwtools.${command}`)();

				assert.deepStrictEqual(state.showErrorMessage.mock.calls, [
					[`CWTools: ${command} failed: boom`],
				]);
				assert.deepStrictEqual(state.showInformationMessage.mock.calls, []);
			});

			test("stays silent when the request is cancelled", async () => {
				const client = fakeClient([command]);
				client.sendRequest.mockRejectedValue({
					code: LSPErrorCodes.RequestCancelled,
				});
				register(client);

				await handler(`cwtools.${command}`)();

				assert.deepStrictEqual(state.showErrorMessage.mock.calls, []);
				assert.deepStrictEqual(state.showInformationMessage.mock.calls, []);
			});
		});
	}

	// The fix lands as one snapshot; Cancel stays the $/cancelRequest path, so
	// no token even against a server that supports progress.
	test("fixAllWorkspace sends the exact command without a workDoneToken and shows the reply", async () => {
		const client = fakeClient(["fixAllWorkspace"], true);
		client.sendRequest.mockResolvedValue("Fixed 12 problem(s) in 3 file(s).");
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
	});

	test("formatWorkspace carries a workDoneToken against a server that supports progress", async () => {
		const client = fakeClient(["formatWorkspace"], true);
		client.sendRequest.mockResolvedValue("Formatted 2 file(s)");
		register(client);

		await handler("cwtools.formatWorkspace")();

		const [type, params, cancellation] = client.sendRequest.mock.calls[0] as [
			unknown,
			{ command: string; arguments: unknown[]; workDoneToken?: string },
			unknown,
		];
		assert.strictEqual(type, state.requestType);
		assert.strictEqual(params.command, "formatWorkspace");
		assert.match(params.workDoneToken ?? "", /^cwtools\/command\//);
		assert.strictEqual(cancellation, undefined);
		assert.deepStrictEqual(state.showInformationMessage.mock.calls, [
			["CWTools: Formatted 2 file(s)"],
		]);
	});

	suite("cwtools.exportProfilingLog", () => {
		test("sends the exact request and saves the fetched log", async () => {
			const client = fakeClient(["exportProfilingLog"]);
			const log = "phase=parse 12ms\nrss=31MB";
			client.sendRequest.mockResolvedValue(log);
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
			assert.deepStrictEqual(state.writeFile.mock.calls, [
				[{ fsPath: "/tmp/prof.log" }, Buffer.from(log, "utf8")],
			]);
			assert.deepStrictEqual(state.showInformationMessage.mock.calls, [
				["CWTools: profiling log written to /tmp/prof.log"],
			]);
		});

		test("warns instead of saving an empty log", async () => {
			const client = fakeClient(["exportProfilingLog"]);
			client.sendRequest.mockResolvedValue("");
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

		test("shows a failure message when the fetch rejects", async () => {
			const client = fakeClient(["exportProfilingLog"]);
			client.sendRequest.mockRejectedValue(new Error("boom"));
			register(client);

			await handler("cwtools.exportProfilingLog")();

			assert.deepStrictEqual(state.showErrorMessage.mock.calls, [
				["CWTools: could not fetch profiling log: boom"],
			]);
			assert.deepStrictEqual(state.writeFile.mock.calls, []);
		});
	});
});
