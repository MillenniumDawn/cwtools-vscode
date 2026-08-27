import { afterEach, beforeAll, beforeEach, suite, test, vi } from "vitest";
import * as assert from "assert";
import type { ExtensionContext, WebviewPanel } from "vscode";
import type { LanguageClient } from "vscode-languageclient/node";
import type { EditorTracker } from "../../src/host/documentLanguage";
import type { GraphData } from "../../src/common/graphTypes";
import { GRAPH_DATA_COMMAND } from "../../src/host/graphAvailability";

// The window-reload serializer in commands.ts is registered through the real
// vscode boundary, so the vscode mock captures the registered serializer and
// the tests drive deserializeWebviewPanel directly.

interface SerializerShape {
	deserializeWebviewPanel(webviewPanel: unknown, state: unknown): Promise<void>;
}

const {
	executeCommand,
	registerCommand,
	showWarningMessage,
	showInformationMessage,
	showOpenDialog,
	readFile,
	registerWebviewPanelSerializer,
	serializers,
	logError,
} = vi.hoisted(() => {
	const serializers: SerializerShape[] = [];
	return {
		executeCommand: vi.fn(),
		registerCommand: vi.fn(() => ({ dispose: () => {} })),
		showWarningMessage: vi.fn(),
		showInformationMessage: vi.fn(),
		showOpenDialog: vi.fn(),
		readFile: vi.fn(),
		registerWebviewPanelSerializer: vi.fn(
			(_viewType: string, serializer: SerializerShape) => {
				serializers.push(serializer);
				return { dispose: () => {} };
			},
		),
		serializers,
		logError: vi.fn(),
	};
});

vi.mock("vscode", async (importOriginal) => ({
	...(await importOriginal<object>()),
	commands: { executeCommand, registerCommand },
	Uri: { file: (p: string) => ({ fsPath: p, toString: () => `file://${p}` }) },
	ViewColumn: { One: 1 },
	window: {
		activeTextEditor: undefined,
		registerWebviewPanelSerializer,
		showInformationMessage,
		showOpenDialog,
		showWarningMessage,
	},
	workspace: {
		fs: { readFile },
		getConfiguration: vi.fn(() => ({ get: () => 1 })),
	},
}));

vi.mock("vscode-languageclient/node", () => ({
	ExecuteCommandRequest: { type: {} },
}));

vi.mock("../../src/host/logger", () => ({
	logError,
	logInfo: vi.fn(),
	logWarn: vi.fn(),
	errorMessage: (err: unknown) => (err instanceof Error ? err.message : ""),
}));

import { registerCommands } from "../../src/host/commands";
import { GraphPanel } from "../../src/host/graphPanel";

suite("graph panel restore", () => {
	const context = {
		extensionPath: "/ext",
		subscriptions: [],
	} as unknown as ExtensionContext;
	const client: {
		initializeResult: {
			capabilities: { executeCommandProvider: { commands: string[] } };
		};
	} = {
		initializeResult: {
			capabilities: {
				executeCommandProvider: { commands: [GRAPH_DATA_COMMAND] },
			},
		},
	};
	const tracker = { getLatestType: () => "idea" } as unknown as EditorTracker;

	const graphData: GraphData = [
		{
			id: "a",
			name: "A",
			isPrimary: true,
			entityType: "idea",
			location: { filename: "a.txt", line: 1, column: 0 },
			references: [],
		},
	];
	const sampleJson = JSON.stringify({ elements: { nodes: [] } });

	// A stand-in for the panel the host hands to the serializer: enough of the
	// WebviewPanel surface for GraphPanel.restore to rebuild the panel, with
	// postMessage recorded and a way to simulate the webview's "ready" message.
	function fakePanel() {
		let messageListener: ((message: unknown) => void) | undefined;
		const postMessage = vi.fn();
		const panel = {
			webview: {
				html: "",
				cspSource: "https://test.webview",
				asWebviewUri: () => ({
					toString: () => "https://test.webview/graph.js",
				}),
				postMessage,
				onDidReceiveMessage: (listener: (message: unknown) => void) => {
					messageListener = listener;
					return { dispose: () => {} };
				},
			},
			onDidDispose: () => ({ dispose: () => {} }),
			onDidChangeViewState: () => ({ dispose: () => {} }),
			dispose: vi.fn(),
		};
		return {
			panel: panel as unknown as WebviewPanel,
			postMessage,
			ready: () => messageListener?.({ command: "ready" }),
		};
	}

	let serializer: SerializerShape | undefined;
	// The restored panel's constructor calls setContext before the fetch, so
	// assert on the getGraphData requests rather than the whole call log.
	const graphRequests = () =>
		executeCommand.mock.calls.filter((call) => call[0] === "getGraphData");

	beforeAll(async () => {
		registerCommands(
			context,
			client as unknown as LanguageClient,
			tracker,
			undefined,
		);
		await vi.waitFor(() => {
			if (serializers.length === 0) {
				throw new Error("serializer not registered");
			}
		});
		serializer = serializers[0];
	});
	const deserialize = (panel: unknown, state: unknown) =>
		serializer!.deserializeWebviewPanel(panel, state);

	beforeEach(() => {
		vi.clearAllMocks();
		client.initializeResult = {
			capabilities: {
				executeCommandProvider: { commands: [GRAPH_DATA_COMMAND] },
			},
		};
	});

	afterEach(() => {
		GraphPanel.currentPanel?.dispose();
	});

	test("re-requests the server graph for the persisted entity type and depth", async () => {
		executeCommand.mockResolvedValue(graphData);
		const panel = fakePanel();

		await deserialize(panel.panel, {
			source: "server",
			entityType: "idea",
			depth: 4,
		});

		assert.deepStrictEqual(graphRequests(), [["getGraphData", "idea", 4]]);
		assert.ok(GraphPanel.currentPanel, "restored panel becomes currentPanel");

		panel.ready();
		assert.deepStrictEqual(panel.postMessage.mock.calls, [
			[
				{
					command: "go",
					data: graphData,
					settings: { wheelSensitivity: 1 },
					persist: { source: "server", entityType: "idea", depth: 4 },
				},
			],
		]);
	});

	test("keeps a persisted depth of zero instead of defaulting", async () => {
		executeCommand.mockResolvedValue(graphData);
		const panel = fakePanel();

		await deserialize(panel.panel, {
			source: "server",
			entityType: "idea",
			depth: 0,
		});

		assert.deepStrictEqual(graphRequests(), [["getGraphData", "idea", 0]]);
	});

	test("defaults the depth to the current graph depth when the state omits it", async () => {
		executeCommand.mockResolvedValue(graphData);
		const panel = fakePanel();

		await deserialize(panel.panel, {
			source: "server",
			entityType: "idea",
		});

		assert.deepStrictEqual(graphRequests(), [["getGraphData", "idea", 3]]);
	});

	test("warns instead of restoring when the server provides no graph data", async () => {
		client.initializeResult = {
			capabilities: {
				executeCommandProvider: { commands: ["getFileTypes"] },
			},
		};
		const panel = fakePanel();

		await deserialize(panel.panel, {
			source: "server",
			entityType: "idea",
			depth: 3,
		});

		assert.deepStrictEqual(graphRequests(), []);
		assert.strictEqual(panel.postMessage.mock.calls.length, 0);
		assert.deepStrictEqual(showWarningMessage.mock.calls, [
			[
				"CWTools: this language server doesn't provide graph data, so the graph can't be restored.",
			],
		]);
	});

	test("re-imports the JSON export the user picks", async () => {
		showOpenDialog.mockResolvedValue([{ fsPath: "/tmp/graph.json" }]);
		readFile.mockResolvedValue(Buffer.from(sampleJson, "utf8"));
		const panel = fakePanel();

		await deserialize(panel.panel, { source: "json" });

		assert.deepStrictEqual(showOpenDialog.mock.calls, [
			[{ filters: { Json: ["json"] } }],
		]);
		assert.deepStrictEqual(readFile.mock.calls, [
			[{ fsPath: "/tmp/graph.json" }],
		]);

		panel.ready();
		assert.deepStrictEqual(panel.postMessage.mock.calls, [
			[
				{
					command: "importJson",
					json: sampleJson,
					settings: { wheelSensitivity: 1 },
					persist: { source: "json" },
				},
			],
		]);
	});

	test("tells the user a JSON graph can't be restored when the import is cancelled", async () => {
		showOpenDialog.mockResolvedValue(undefined);
		const panel = fakePanel();

		await deserialize(panel.panel, { source: "json" });

		assert.strictEqual(panel.postMessage.mock.calls.length, 0);
		assert.deepStrictEqual(showInformationMessage.mock.calls, [
			[
				"CWTools: graph data from a JSON export isn't persisted across reloads. " +
					"Run 'CWTools: Recreate graph from json' to rebuild it.",
			],
		]);
	});

	test("falls back to the last active entity type when no state was persisted", async () => {
		executeCommand.mockResolvedValue(graphData);
		const panel = fakePanel();

		await deserialize(panel.panel, undefined);

		assert.deepStrictEqual(graphRequests(), [["getGraphData", "idea", 3]]);
		panel.ready();
		assert.deepStrictEqual(panel.postMessage.mock.calls, [
			[
				{
					command: "go",
					data: graphData,
					settings: { wheelSensitivity: 1 },
					persist: { source: "server", entityType: "idea", depth: 3 },
				},
			],
		]);
	});

	test("falls back when the state names a server graph without an entity type", async () => {
		executeCommand.mockResolvedValue(graphData);
		const panel = fakePanel();

		await deserialize(panel.panel, { source: "server" });

		assert.deepStrictEqual(graphRequests(), [["getGraphData", "idea", 3]]);
	});

	test("shows an info message when there is nothing to restore", async () => {
		client.initializeResult = {
			capabilities: {
				executeCommandProvider: { commands: ["getFileTypes"] },
			},
		};
		const panel = fakePanel();

		await deserialize(panel.panel, undefined);

		assert.deepStrictEqual(graphRequests(), []);
		assert.strictEqual(panel.postMessage.mock.calls.length, 0);
		assert.deepStrictEqual(showInformationMessage.mock.calls, [
			[
				"CWTools: graph data isn't persisted across reloads. " +
					"Run 'CWTools: Show graph' to rebuild the graph.",
			],
		]);
	});

	test("logs a restore failure instead of rejecting", async () => {
		const failure = new Error("server down");
		executeCommand.mockRejectedValue(failure);
		const panel = fakePanel();

		await assert.doesNotReject(() =>
			deserialize(panel.panel, {
				source: "server",
				entityType: "idea",
				depth: 3,
			}),
		);
		assert.ok(
			GraphPanel.currentPanel,
			"panel still restored before the fetch failed",
		);
		assert.deepStrictEqual(logError.mock.calls, [
			["graph panel restore failed", failure],
		]);
	});
});
